//! Reading a stack's frames, whatever kind of file each one is: every file goes to the crate that reads it and comes
//! back as linear `f32`, what alignment and stacking work on. This is the one place that decides between the RAW path
//! and the image path, so that the commands and jobs built on it (adding frames to a project, the watch folder, the
//! CLI's `stack`) all decide alike.
//!
//! - **Camera RAW and DNG files**, which [`florastack_raw::probe`] recognizes from their bytes, are decoded by
//!   florastack-raw and developed with the stack's one [`RawDevelop`]: the [`DevelopSettings`] given, resolved on the
//!   first RAW frame read, unless the project already holds that resolution ([`FrameReader::with_develop`]). A DNG
//!   that a raw converter already demosaiced is imported unchanged.
//! - **Every other image file** — above all the 16-bit TIFF files that users' own raw converters export, but also
//!   JPEG, PNG, OpenEXR and the other formats of florastack-codecs — is read by florastack-codecs and imported
//!   unchanged: its samples are only converted to linear `f32` (sRGB-encoded ones decoded, others through their ICC
//!   profile's curve, as the codecs do), and keep their profile. No develop setting applies to them.
//!
//! Both kinds are turned upright as [`ReadOptions::orientation`] says.

use florastack_codecs::{CodecError, Format, OrientationPolicy, ReadOptions, Warning};
use florastack_core::{AnyImage, Encoding, Image, ImageError, Orientation};
use florastack_raw::{DevelopSettings, DevelopWarning, Quality, RawDevelop, RawError, RawFormat};

/// What kind of file a frame comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameSource {
    /// A camera RAW or DNG file, read by florastack-raw.
    Raw(RawFormat),
    /// Another image file, read by florastack-codecs.
    Image(Format),
}

/// Something that reading a frame did without failing, for the user to see.
#[derive(Clone, Debug, PartialEq)]
pub enum FrameWarning {
    Develop(DevelopWarning),
    Codec(Warning),
}

/// A frame, read.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Linear light, tagged with its color space: linear Rec. 2020 for developed RAW frames, the file's own profile
    /// for the others.
    pub image: Image<f32>,
    pub source: FrameSource,
    /// The orientation that was applied.
    pub orientation: Orientation,
    /// Whether the samples are the file's own rather than developed here: every image file, and every DNG that a raw
    /// converter already demosaiced.
    pub unchanged: bool,
    pub warnings: Vec<FrameWarning>,
}

/// Why a frame can't be read.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error(transparent)]
    Raw(#[from] RawError),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error(transparent)]
    Image(#[from] ImageError),
    /// In the browser build, a frame too large for the page's memory, refused from its headers before it is decoded
    /// ([`check_size`]).
    #[error(
        "the frame is {width}x{height} ({megapixels} MP), and beside the {held_mb} MB of files it holds a browser page can read frames of          up to about {limit_mp} MP: give it fewer or smaller files, or use the desktop app"
    )]
    TooLargeForPage { width: usize, height: usize, megapixels: u64, limit_mp: u64, held_mb: u64 },
}

/// In the browser build, refuses a frame whose pixels wouldn't fit in the page's memory beside the files it holds
/// ([`crate::compute::browser_frame_pixels`]), from its headers: what adding a frame checks, and every read before it
/// decodes, since the files held may have grown since. A decoder that runs out of the page's memory aborts the whole
/// page, which loses the project. Native targets have the memory, and check nothing here.
pub fn check_size(bytes: &[u8]) -> Result<(), FrameError> {
    #[cfg(target_arch = "wasm32")]
    {
        let held = florastack_core::fs::bytes_in_memory().unwrap_or(0);
        fits(bytes, crate::compute::browser_frame_pixels(held), held)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (bytes, fits);
        Ok(())
    }
}

/// Whether the frame in `bytes` has at most `limit` pixels, as its headers say; a file whose headers can't be read
/// passes, for its read to say what is wrong.
fn fits(bytes: &[u8], limit: u64, held: u64) -> Result<(), FrameError> {
    let size = match source(bytes) {
        Ok(FrameSource::Raw(_)) => florastack_raw::info(bytes).ok().map(|i| (i.width, i.height)),
        Ok(FrameSource::Image(_)) => florastack_codecs::probe(bytes).ok().map(|i| (i.width, i.height)),
        Err(_) => None,
    };
    let Some((width, height)) = size else { return Ok(()) };
    let pixels = (width as u64).saturating_mul(height as u64);
    if pixels <= limit {
        return Ok(());
    }
    Err(FrameError::TooLargeForPage { width, height, megapixels: pixels / 1_000_000, limit_mp: limit / 1_000_000, held_mb: held / 1_000_000 })
}

/// The file extensions of the image formats florastack-codecs reads, lower case and without the dot.
pub const IMAGE_EXTENSIONS: [&str; 9] = ["jpg", "jpeg", "jpe", "png", "tif", "tiff", "exr", "webp", "bmp"];

/// Whether a file's name says it is a frame FloraStack reads: a camera RAW or DNG file, or an image file. What picks
/// the files of a folder; the file's bytes decide in the end ([`source`]).
pub fn is_frame_name(path: &std::path::Path) -> bool {
    let Some(extension) = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase) else { return false };
    IMAGE_EXTENSIONS.contains(&extension.as_str()) || florastack_raw::extensions().any(|e| e == extension)
}

/// Which crate reads `bytes`: florastack-raw for camera RAW and DNG files, florastack-codecs for every image format it
/// knows. Anything else is the codecs' error for it, which says what to do.
pub fn source(bytes: &[u8]) -> Result<FrameSource, FrameError> {
    if let Some(format) = florastack_raw::probe(bytes) {
        return Ok(FrameSource::Raw(format));
    }
    Ok(FrameSource::Image(florastack_codecs::sniff(bytes)?))
}

/// The orientation a read applies to pixels whose file records `recorded`.
pub fn orientation(policy: OrientationPolicy, recorded: Orientation) -> Orientation {
    match policy {
        OrientationPolicy::Apply => recorded,
        OrientationPolicy::Ignore => Orientation::Normal,
        OrientationPolicy::Force(o) => o,
    }
}

/// How much of a frame a read asks for.
#[derive(Clone, Copy, Debug)]
enum Size {
    Full(Quality),
    /// At least this many pixels on the longer side.
    Reduced(usize),
}

/// Reads the frames of one stack, developing its RAW frames alike.
#[derive(Clone, Debug)]
pub struct FrameReader {
    settings: DevelopSettings,
    develop: Option<RawDevelop>,
    options: ReadOptions,
}

impl FrameReader {
    /// A reader that develops RAW frames with `settings`, resolved on the first RAW frame it reads.
    pub fn new(settings: DevelopSettings, options: ReadOptions) -> FrameReader {
        FrameReader { settings, develop: None, options }
    }

    /// A reader with the stack's development already resolved (as a project stores it).
    pub fn with_develop(develop: RawDevelop, options: ReadOptions) -> FrameReader {
        FrameReader { settings: develop.settings().clone(), develop: Some(develop), options }
    }

    /// The stack's development, once resolved: what to store in the project.
    pub fn develop(&self) -> Option<&RawDevelop> {
        self.develop.as_ref()
    }

    /// Reads one frame from its file's bytes; `quality` picks the demosaic of RAW frames.
    pub fn read(&mut self, bytes: &[u8], quality: Quality) -> Result<Frame, FrameError> {
        self.read_as(bytes, Size::Full(quality))
    }

    /// Reads one frame for a thumbnail or a small preview: a RAW frame comes at a fraction of its size, developed
    /// straight from its mosaic without a demosaic (`RawDevelop::develop_reduced`), with at least `min_side` pixels
    /// on its longer side where it has them; every other frame, and a RAW frame that can't be reduced so, comes
    /// whole, as [`FrameReader::read`] gives it for a preview. The caller reduces it to the size it wants.
    pub fn read_reduced(&mut self, bytes: &[u8], min_side: usize) -> Result<Frame, FrameError> {
        self.read_as(bytes, Size::Reduced(min_side))
    }

    fn read_as(&mut self, bytes: &[u8], size: Size) -> Result<Frame, FrameError> {
        check_size(bytes)?;
        match source(bytes)? {
            FrameSource::Raw(format) => {
                let raw = florastack_raw::decode(bytes, self.options.max_pixels)?;
                let develop = match &mut self.develop {
                    Some(develop) => develop,
                    empty => empty.insert(RawDevelop::new(&self.settings, &raw)?),
                };
                let developed = match size {
                    Size::Full(quality) => develop.develop(&raw, quality)?,
                    Size::Reduced(min_side) => develop.develop_reduced(&raw, min_side)?,
                };
                drop(raw);
                let applied = orientation(self.options.orientation, developed.orientation);
                let image = if applied == Orientation::Normal { developed.image } else { developed.image.oriented(applied)? };
                let warnings = developed.warnings.into_iter().map(FrameWarning::Develop).collect();
                Ok(Frame { image, source: FrameSource::Raw(format), orientation: applied, unchanged: developed.converted, warnings })
            }
            FrameSource::Image(format) => {
                let decoded = florastack_codecs::read(bytes, &self.options)?;
                let image = match decoded.image {
                    AnyImage::U8(i) => i.to_linear_f32()?,
                    AnyImage::U16(i) => i.to_linear_f32()?,
                    AnyImage::F32(mut i) => {
                        i.convert_encoding(Encoding::Linear);
                        i
                    }
                };
                let warnings = decoded.warnings.into_iter().map(FrameWarning::Codec).collect();
                Ok(Frame { image, source: FrameSource::Image(format), orientation: decoded.orientation_applied, unchanged: true, warnings })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_policies_choose_what_is_applied() {
        assert_eq!(orientation(OrientationPolicy::Apply, Orientation::Rotate90), Orientation::Rotate90);
        assert_eq!(orientation(OrientationPolicy::Ignore, Orientation::Rotate90), Orientation::Normal);
        assert_eq!(orientation(OrientationPolicy::Force(Orientation::Rotate270), Orientation::Rotate90), Orientation::Rotate270);
    }

    #[test]
    fn files_go_to_the_crate_that_reads_them() {
        assert_eq!(source(b"\x89PNG\r\n\x1a\n\0\0\0\x0d").unwrap(), FrameSource::Image(Format::Png));
        assert_eq!(source(b"FUJIFILMCCD-RAW 0201FF383501").unwrap(), FrameSource::Raw(RawFormat::Raf));
        assert!(matches!(source(b"hello"), Err(FrameError::Codec(CodecError::UnknownFormat))));
    }

    #[test]
    fn a_frame_too_large_for_a_page_is_refused_from_its_headers() {
        let image = Image::<u8>::new(40, 30, 3, florastack_core::ColorSpace::SRGB).unwrap();
        let spec = florastack_codecs::OutputSpec::new(Format::Png).unwrap();
        let png = florastack_codecs::write(&AnyImage::U8(image), &spec, &florastack_codecs::OutputMetadata::default()).unwrap().bytes;
        assert!(fits(&png, 1200, 0).is_ok(), "40 x 30 is 1200 pixels");
        let refused = fits(&png, 1199, 5_000_000).unwrap_err();
        assert!(matches!(refused, FrameError::TooLargeForPage { width: 40, height: 30, held_mb: 5, .. }), "{refused:?}");
        assert!(refused.to_string().contains("40x30") && refused.to_string().contains("desktop app"), "{refused}");
        assert!(fits(b"hello", 0, 0).is_ok(), "a file whose headers can't be read is left to its read");
        assert!(check_size(&png).is_ok(), "native targets have the memory");
    }
}
