//! Images for display ([`load`], [`display`]): a frame, an output, a preview or a thumbnail as a viewer shows it, read
//! and converted here so that the desktop app only uploads pixels and keeps no color or file logic of its own.
//!
//! A [`DisplayImage`] is linear light in sRGB's primaries (the viewer's shader applies sRGB's curve: the display
//! transform), as RGBA half floats, rows from the top, ready for an `Rgba16Float` texture. Files are read as stacking
//! reads its frames ([`crate::frames::FrameReader`]): camera RAW files developed with the fast demosaic and the
//! default settings, every file turned by its recorded orientation. Images with an embedded ICC profile are converted
//! from its primaries (linear Rec. 2020 for developed RAW frames and their stacks); those without are taken as sRGB.
//! Gray images become gray RGB; an alpha channel (the second of two, the fourth of four) is kept, and opaque images
//! get alpha 1. Samples that aren't finite become 0, and values beyond half floats' range are clamped to it; values
//! above 1 stay, for the viewer to clip.
//!
//! Loading is plain file I/O and arithmetic with no engine state, so a viewer runs it on a thread of its own.

use std::path::{Path, PathBuf};

use florastack_codecs::{ColorEngine, LightCraftColor, ReadOptions, StandardCurve};
use florastack_core::{Image, ImageError};
use florastack_raw::{DevelopSettings, Quality};

use crate::frames::{FrameError, FrameReader};

/// The largest file [`load`] reads: far beyond any frame, small enough not to exhaust memory on a hostile path.
const MAX_FILE_BYTES: u64 = 2 << 30;

/// The largest half float.
const HALF_MAX: f32 = 65504.0;

/// An image ready for a viewer. See the module documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayImage {
    pub width: usize,
    pub height: usize,
    /// RGBA, IEEE half floats as their bits, linear light in sRGB's primaries, `width * height * 4` of them.
    pub rgba: Vec<u16>,
    /// Whether the image had an alpha channel (some pixels may be transparent).
    pub alpha: bool,
}

impl DisplayImage {
    /// The image as 8-bit sRGB RGBA, unpremultiplied: sRGB's curve applied, values clipped to `[0, 1]`. For small
    /// images a viewer draws without its own shader, such as thumbnails.
    pub fn to_srgb8(&self) -> Vec<u8> {
        let encode = |bits: u16, curve: bool| {
            let v = half::f16::from_bits(bits).to_f32().clamp(0.0, 1.0);
            let v = if curve { florastack_core::convert::linear_to_srgb(v) } else { v };
            florastack_core::Sample::from_normalized(v)
        };
        self.rgba.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, a]| [encode(r, true), encode(g, true), encode(b, true), encode(a, false)]).collect()
    }
}

/// Why an image couldn't be prepared for display.
#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is {bytes} bytes, more than the {MAX_FILE_BYTES} a viewer reads")]
    TooLarge { path: PathBuf, bytes: u64 },
    #[error("cannot decode {path}")]
    Decode {
        path: PathBuf,
        #[source]
        source: FrameError,
    },
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error("cannot reduce the image")]
    Reduce(#[source] florastack_align::AlignError),
}

/// The image in the file at `path` for display, reduced to at most `max_side` pixels on its longer side when given
/// (thumbnails). See the module documentation.
pub fn load(path: &Path, max_side: Option<usize>) -> Result<DisplayImage, ViewError> {
    let read = |source| ViewError::Read { path: path.to_path_buf(), source };
    let bytes = std::fs::metadata(path).map_err(read)?.len();
    if bytes > MAX_FILE_BYTES {
        return Err(ViewError::TooLarge { path: path.to_path_buf(), bytes });
    }
    let contents = std::fs::read(path).map_err(read)?;
    let mut reader = FrameReader::new(DevelopSettings::default(), ReadOptions::default());
    let frame = reader.read(&contents, Quality::Preview).map_err(|source| ViewError::Decode { path: path.to_path_buf(), source })?;
    drop(contents);
    display(&frame.image, max_side)
}

/// `image` (linear, tagged with its color space) for display, reduced to at most `max_side` pixels on its longer
/// side when given. See the module documentation.
pub fn display(image: &Image<f32>, max_side: Option<usize>) -> Result<DisplayImage, ViewError> {
    let mut image = match max_side {
        Some(side) if image.width().max(image.height()) > side => {
            florastack_align::thumbnail(image, side.max(florastack_align::MIN_THUMBNAIL_SIDE)).map_err(ViewError::Reduce)?
        }
        _ => image.clone(),
    };
    to_srgb_primaries(&mut image);
    let (w, h, c) = (image.width(), image.height(), image.channels());
    let alpha = matches!(c, 2 | 4);
    let mut rgba = Vec::with_capacity(w * h * 4);
    let half = |v: f32| half::f16::from_f32(if v.is_finite() { v.clamp(-HALF_MAX, HALF_MAX) } else { 0.0 }).to_bits();
    for row in image.rows() {
        for pixel in row.chunks_exact(c.max(1)) {
            let (rgb, a) = match *pixel {
                [v] => ([v, v, v], 1.0),
                [v, a] => ([v, v, v], a),
                [r, g, b] => ([r, g, b], 1.0),
                [r, g, b, a, ..] => ([r, g, b], a),
                [] => ([0.0; 3], 1.0),
            };
            rgba.extend(rgb.into_iter().map(half));
            rgba.push(half(a.clamp(0.0, 1.0)));
        }
    }
    Ok(DisplayImage { width: w, height: h, rgba, alpha })
}

/// `rect` of `image` for display, reduced by `factor` (1 for full size): `rect` is widened to a multiple of `factor`
/// on a grid from the image's corner, so that the regions of an image always line up with the image reduced whole,
/// and each block of `factor` x `factor` pixels is averaged. Returns the display pixels and the rectangle they show,
/// in reduced pixels. What the retouching view uploads as strokes change it.
pub fn display_region(image: &Image<f32>, rect: florastack_core::Rect, factor: usize) -> Result<(DisplayImage, florastack_core::Rect), ViewError> {
    let f = factor.max(1);
    let (w, h) = (image.width(), image.height());
    let (x0, y0) = (rect.x.min(w) / f, rect.y.min(h) / f);
    let (x1, y1) = ((rect.x + rect.width).min(w).div_ceil(f), (rect.y + rect.height).min(h).div_ceil(f));
    let (rw, rh) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    let c = image.channels().max(1);
    let mut data = vec![0.0f32; rw * rh * c];
    for j in 0..rh {
        for i in 0..rw {
            let (bx0, by0) = ((x0 + i) * f, (y0 + j) * f);
            let (bx1, by1) = ((bx0 + f).min(w), (by0 + f).min(h));
            let mut sum = [0.0f64; 4];
            for y in by0..by1 {
                let Some(row) = image.row(y) else { continue };
                for x in bx0..bx1 {
                    for (k, s) in sum.iter_mut().enumerate().take(c) {
                        *s += f64::from(row.get(x * c + k).copied().unwrap_or(0.0));
                    }
                }
            }
            #[allow(clippy::cast_precision_loss)] // A count of pixels.
            let n = ((bx1 - bx0) * (by1 - by0)).max(1) as f64;
            for (k, s) in sum.iter().enumerate().take(c) {
                #[allow(clippy::cast_possible_truncation)] // An average of f32 samples.
                let v = (s / n) as f32;
                data[(j * rw + i) * c + k] = v;
            }
        }
    }
    let region = Image::from_vec(rw.max(1), rh.max(1), c, image.color_space().clone(), if rw * rh == 0 { vec![0.0; c] } else { data })?;
    Ok((display(&region, None)?, florastack_core::Rect::new(x0, y0, rw, rh)))
}

/// Converts `image`'s color channels from its profile's primaries to sRGB's, in place; an image without a profile is
/// sRGB already, and one whose profile can't be converted (a lookup-table profile) is shown as it is.
fn to_srgb_primaries(image: &mut Image<f32>) {
    let Some(icc) = image.color_space().icc.clone() else { return };
    if image.channels() < 3 {
        return;
    }
    let engine = LightCraftColor;
    let converted = engine
        .parse(&icc)
        .and_then(|from| Ok((from, engine.standard(florastack_codecs::NamedSpace::Srgb, StandardCurve::Linear)?)))
        .and_then(|(from, to)| engine.convert(image, &from, &to));
    if let Err(e) = converted {
        tracing::debug!("showing an image in its own primaries: its profile can't be converted ({e})");
    }
}

#[cfg(test)]
mod tests {
    use florastack_core::ColorSpace;

    use super::*;

    fn value(bits: u16) -> f32 {
        half::f16::from_bits(bits).to_f32()
    }

    #[test]
    fn channels_become_rgba_and_hostile_samples_become_safe() {
        let gray = Image::from_vec(2, 1, 1, ColorSpace::LINEAR, vec![0.25, 0.5]).unwrap();
        let shown = display(&gray, None).unwrap();
        assert_eq!((shown.width, shown.height, shown.alpha), (2, 1, false));
        assert_eq!(shown.rgba.iter().map(|&b| value(b)).collect::<Vec<_>>(), [0.25, 0.25, 0.25, 1.0, 0.5, 0.5, 0.5, 1.0]);
        let gray_alpha = Image::from_vec(1, 1, 2, ColorSpace::LINEAR, vec![0.75, 0.5]).unwrap();
        let shown = display(&gray_alpha, None).unwrap();
        assert!(shown.alpha);
        assert_eq!(shown.rgba.iter().map(|&b| value(b)).collect::<Vec<_>>(), [0.75, 0.75, 0.75, 0.5]);
        let hostile = Image::from_vec(1, 1, 4, ColorSpace::LINEAR, vec![f32::NAN, 1e9, -1e9, 7.0]).unwrap();
        let shown = display(&hostile, None).unwrap();
        assert_eq!(shown.rgba.iter().map(|&b| value(b)).collect::<Vec<_>>(), [0.0, HALF_MAX, -HALF_MAX, 1.0]);
    }

    #[test]
    fn eight_bit_srgb_applies_the_curve_to_colors_only() {
        let image = Image::from_vec(1, 1, 4, ColorSpace::LINEAR, vec![0.0, 0.214_041_14, 1.5, 0.5]).unwrap();
        let bytes = display(&image, None).unwrap().to_srgb8();
        // Linear 0.214 is sRGB 0.5 (127 or 128 once it has been a half float); 1.5 clips to white; alpha 0.5 is only
        // scaled.
        assert_eq!([bytes[0], bytes[2], bytes[3]], [0, 255, 128]);
        assert!(bytes[1].abs_diff(128) <= 1, "{bytes:?}");
    }

    #[test]
    fn thumbnails_fit_their_side() {
        let big = Image::from_fn(300, 120, 3, ColorSpace::LINEAR, |x, _, _| (x % 7) as f32 / 7.0).unwrap();
        let shown = display(&big, Some(64)).unwrap();
        // The longer side fits; the shorter keeps the aspect within a pixel (120 * 64 / 300 = 25.6).
        assert_eq!(shown.width, 64);
        assert!(shown.height.abs_diff(26) <= 1, "{}", shown.height);
        assert_eq!(shown.rgba.len(), shown.width * shown.height * 4);
        let small = display(&big, Some(1000)).unwrap();
        assert_eq!((small.width, small.height), (300, 120), "an image that fits isn't enlarged");
    }

    #[test]
    fn wide_gamut_images_come_to_srgb_primaries() {
        // Pure Rec. 2020 green lies outside sRGB: in sRGB's primaries it has negative red and blue.
        let engine = LightCraftColor;
        let rec2020 = engine.standard(florastack_codecs::NamedSpace::Rec2020, StandardCurve::Linear).unwrap();
        let color = ColorSpace { encoding: florastack_core::Encoding::Linear, icc: Some(rec2020.bytes().clone()) };
        let green = Image::from_vec(1, 1, 3, color, vec![0.0, 1.0, 0.0]).unwrap();
        let shown = display(&green, None).unwrap();
        let [r, g, b, a] = [0, 1, 2, 3].map(|i| value(shown.rgba[i]));
        assert!(r < -0.1 && g > 1.0 && b < -0.01 && (a - 1.0).abs() < 1e-6, "{r} {g} {b} {a}");
        // White stays white.
        let color = ColorSpace { encoding: florastack_core::Encoding::Linear, icc: Some(rec2020.bytes().clone()) };
        let white = Image::from_vec(1, 1, 3, color, vec![1.0, 1.0, 1.0]).unwrap();
        let shown = display(&white, None).unwrap();
        assert!(shown.rgba[..3].iter().all(|&b| (value(b) - 1.0).abs() < 2e-3), "{:?}", shown.rgba);
    }

    #[test]
    fn files_that_dont_exist_or_dont_decode_are_errors() {
        let dir = std::env::temp_dir().join(format!("florastack-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(load(&dir.join("missing.tif"), None), Err(ViewError::Read { .. })));
        let junk = dir.join("junk.tif");
        std::fs::write(&junk, b"not an image").unwrap();
        assert!(matches!(load(&junk, None), Err(ViewError::Decode { .. })));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
