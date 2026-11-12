//! Linear DNG (Adobe's Digital Negative) for stacked output, built from lightcraft-tiff's IFDs: one `LinearRaw` image
//! of three samples per pixel, either 16-bit integers (uncompressed, or in lossless JPEG tiles encoded by
//! lightcraft-raw's `ljpeg`) or 32-bit floats (uncompressed, or Deflate strips with the floating-point predictor).
//! The tests read the files back with LightCraft's RAW decoder; Adobe's own readers haven't checked them yet
//! (docs/ROADMAP.md, 1.2).
//!
//! With a preview (the default), the file is laid out as Adobe's own DNG files are: IFD0 holds a small 8-bit sRGB
//! picture of the image (`NewSubFileType` 1, uncompressed, at most [`PREVIEW_SIDE`] pixels a side) that any TIFF
//! reader and file browser can show, and the raw image is its sub-IFD. Without one, the raw image is IFD0. The DNG
//! tags and the metadata are in IFD0 either way.
//!
//! The samples are linear light in the output profile's primaries, white balanced, white at 65535 for integers and
//! at 1 for floats, which keep values above it when the output keeps its extended range. The color tags describe
//! exactly that space: `ForwardMatrix1` is its colorant matrix (RGB to XYZ relative to D50, as ICC profiles store
//! it), `ColorMatrix1` the inverse, `CalibrationIlluminant1` D50 and `AsShotNeutral` (1, 1, 1). The
//! `UniqueCameraModel` names FloraStack after the camera, so that no raw converter applies a camera profile made
//! for the sensor's own colors. A rendered stack gets an identity `ProfileToneCurve`, so that the converter adds no
//! tone curve to what is already rendered.
//!
//! The files declare `DNGVersion` 1.4; `DNGBackwardVersion` is 1.3 for integer files and 1.4 for float ones, which
//! older readers can't read. Like TIFF, a file is written a band of rows at a time ([`super::container`]).

use std::io::Cursor;

use florastack_core::{AnyImage, Image, Sample};
use lightcraft_tiff::writer::{rational, srational};
use lightcraft_tiff::{IfdBuilder, ImageData, Value, tags as t};
use rayon::prelude::*;

use super::container::{Chunks, Container, Layout};
use super::tiff::{add_metadata, band_strips, worst_case};
use super::{dim32, first_channels, strips};
use crate::codec::{Encoded, ImageEncoder};
use crate::color::{self, Profile};
use crate::error::{BackendError, CodecError, Warning};
use crate::format::Format;
use crate::metadata::PreparedMetadata;
use crate::options::{BitDepth, DngCompression, DngLook, OutputFormat, OutputSpec, TiffCompression};
use crate::stream::{self, Bands, Sink, WholeBands, Written};

pub(crate) struct DngEncoder;

const F: Format = Format::Dng;

/// The side of a lossless JPEG tile.
const TILE: usize = 256;

/// The longest side of the preview, in pixels: enough for a file browser's largest thumbnails, and 3 MB at most.
pub(crate) const PREVIEW_SIDE: usize = 1024;

/// D50, as an EXIF `LightSource` code (`CalibrationIlluminant1`).
const ILLUMINANT_D50: u16 = 23;

fn matrix_value(m: &[[f64; 3]; 3]) -> Value {
    Value::SRational(m.iter().flatten().map(|&v| srational(v)).collect())
}

fn invert(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    lightcraft_color::Mat3(*m).inverse().map(|i| i.0)
}

/// The three-channel samples to store: gray expanded, alpha dropped.
fn rgb<T: Sample>(image: Image<T>) -> Result<Image<T>, CodecError> {
    let c = image.channels();
    if c == 3 {
        return Ok(image);
    }
    let samples: Vec<T> = if c > 3 {
        first_channels(&image, 3)
    } else {
        image.rows().flat_map(|row| row.chunks_exact(c).flat_map(|px| px.first().copied().into_iter().flat_map(|v| [v; 3]))).collect()
    };
    Ok(Image::from_vec(image.width(), image.height(), 3, image.color_space().clone(), samples)?)
}

/// Lossless JPEG tiles of `TILE` x `TILE` pixels in row-major order, the edge tiles padded with the last row and
/// column. `image` is a band of whole tile rows, or the image's last rows.
fn ljpeg_tiles(image: &Image<u16>) -> Vec<Vec<u8>> {
    let (w, h) = (image.width(), image.height());
    let (across, down) = (w.div_ceil(TILE), h.div_ceil(TILE));
    (0..across * down)
        .into_par_iter()
        .map(|i| {
            let (tx, ty) = (i % across * TILE, i / across * TILE);
            let mut buf = Vec::with_capacity(TILE * TILE * 3);
            for y in 0..TILE {
                let sy = (ty + y).min(h.saturating_sub(1));
                for x in 0..TILE {
                    let sx = (tx + x).min(w.saturating_sub(1));
                    buf.extend_from_slice(image.pixel(sx, sy).unwrap_or(&[0, 0, 0]));
                }
            }
            lightcraft_raw::ljpeg::encode(&buf, TILE, TILE, 3, 16, 1, 0)
        })
        .collect()
}

/// The preview, gathered as the bands go by: every pixel of the image added to the preview pixel it falls in, so
/// that each one is the mean of a block of the image (a box filter, which a reduction by a whole factor wants).
struct Preview {
    /// How many image pixels a preview pixel spans, each way.
    factor: usize,
    width: usize,
    height: usize,
    /// The sums of the linear samples, three per preview pixel, and how many pixels each holds.
    sums: Vec<f32>,
    counts: Vec<u32>,
    /// From the file's linear RGB to linear sRGB.
    to_srgb: lightcraft_color::Mat3,
}

impl Preview {
    fn new(width: usize, height: usize, forward: &[[f64; 3]; 3]) -> Option<Preview> {
        let factor = width.max(height).div_ceil(PREVIEW_SIDE).max(1);
        let (pw, ph) = ((width / factor).max(1), (height / factor).max(1));
        let srgb = color::rgb_to_xyz_d50(None);
        let to_srgb = lightcraft_color::Mat3(invert(&srgb)?).mul(&lightcraft_color::Mat3(*forward));
        Some(Preview { factor, width: pw, height: ph, sums: vec![0.0; pw * ph * 3], counts: vec![0; pw * ph], to_srgb })
    }

    /// Adds a band of three-channel rows whose first is row `y` of the image. The last partial block of rows and
    /// columns joins the block before it.
    fn add<T: Sample>(&mut self, band: &Image<T>, y: usize) {
        for (dy, row) in band.rows().enumerate() {
            let py = ((y + dy) / self.factor).min(self.height - 1);
            for (x, px) in row.as_chunks::<3>().0.iter().enumerate() {
                let at = py * self.width + (x / self.factor).min(self.width - 1);
                if let (Some(sum), Some(count)) = (self.sums.get_mut(at * 3..at * 3 + 3), self.counts.get_mut(at)) {
                    for (s, v) in sum.iter_mut().zip(px) {
                        let v = v.to_normalized();
                        // A float file may hold anything: what isn't a number adds nothing.
                        *s += if v.is_finite() { v } else { 0.0 };
                    }
                    *count += 1;
                }
            }
        }
    }

    /// The preview's IFD: 8-bit sRGB in one uncompressed strip.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // Means of samples, clamped to 0..=255.
    fn ifd(&self) -> IfdBuilder {
        let mut pixels = Vec::with_capacity(self.sums.len());
        for (sum, &count) in self.sums.as_chunks::<3>().0.iter().zip(&self.counts) {
            let n = count.max(1) as f32;
            let linear = self.to_srgb.apply_f32(sum.map(|v| v / n));
            pixels.extend(linear.map(|v| (florastack_core::convert::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8));
        }
        let mut ifd = IfdBuilder::new();
        // A reduced-resolution image of another in this file.
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![1]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![dim32(self.width)]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![dim32(self.height)]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![8; 3]));
        ifd.set(t::COMPRESSION, Value::Short(vec![1]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![2]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
        ifd.set_image(ImageData::Strips { rows_per_strip: dim32(self.height), strips: vec![pixels] });
        ifd
    }
}

impl ImageEncoder for DngEncoder {
    fn format(&self) -> Format {
        F
    }

    fn encode(&self, image: &AnyImage, profile: Option<&Profile>, metadata: &PreparedMetadata, spec: &OutputSpec) -> Result<Encoded, CodecError> {
        let mut out = Cursor::new(Vec::new());
        let written = self.encode_to(&mut out, &mut WholeBands(image), profile, metadata, spec)?;
        Ok(Encoded { bytes: out.into_inner(), warnings: written.warnings })
    }

    fn streams(&self) -> bool {
        true
    }

    #[allow(clippy::too_many_lines)] // One file from top to bottom: the image data, then the tags in the order of the specification.
    fn encode_to(
        &self,
        sink: &mut dyn Sink,
        bands: &mut dyn Bands,
        profile: Option<&Profile>,
        metadata: &PreparedMetadata,
        spec: &OutputSpec,
    ) -> Result<Written, CodecError> {
        let OutputFormat::Dng { compression, look, preview } = spec.format else {
            return Err(CodecError::encode(F, BackendError::message("not DNG options")));
        };
        let (w, h, c, depth) = (bands.width(), bands.height(), bands.channels(), bands.depth());
        let float = match depth {
            BitDepth::U16 => false,
            BitDepth::F32 => true,
            BitDepth::U8 => return Err(CodecError::Depth { format: F, depth }),
        };
        if !compression.stores(depth) {
            return Err(CodecError::Depth { format: F, depth });
        }
        let mut warnings = Vec::new();
        if c == 2 || c == 4 {
            warnings.push(Warning::AlphaDropped(F));
        }
        if c < 3 {
            warnings.push(Warning::GrayExpanded(F));
        }
        let forward = color::rgb_to_xyz_d50(profile);
        let inverse = invert(&forward).ok_or_else(|| CodecError::encode(F, BackendError::message("the profile's colorants are singular")))?;
        let too_wide = || CodecError::encode(F, BackendError::message("the image is too wide"));
        let row_bytes = w.checked_mul(3).and_then(|n| n.checked_mul(if float { 4 } else { 2 })).ok_or_else(too_wide)?;
        let sample_bytes = (row_bytes as u64).saturating_mul(h as u64);
        // How the image data is cut up and packed, and the most it can take.
        let strip_rows = strips::rows_per_strip(row_bytes, h);
        let (layout, packing, unit, bound) = match compression {
            DngCompression::Uncompressed => (Layout::Strips { rows: dim32(strip_rows) }, Some(TiffCompression::None), strip_rows, sample_bytes),
            DngCompression::Deflate => (
                Layout::Strips { rows: dim32(strip_rows) },
                Some(TiffCompression::Deflate),
                strip_rows,
                worst_case(sample_bytes, TiffCompression::Deflate),
            ),
            DngCompression::LosslessJpeg => {
                // Whole tiles, the edge ones padded, and up to twice their samples when nothing in them repeats.
                let tiles = (w.div_ceil(TILE) as u64).saturating_mul(h.div_ceil(TILE) as u64);
                (Layout::Tiles { width: dim32(TILE), height: dim32(TILE) }, None, TILE, tiles.saturating_mul((TILE * TILE * 3 * 4) as u64))
            }
        };
        let mut container = Container::new(sink, bound, F)?;
        let mut chunks = Chunks::default();
        let mut thumbnail = if preview { Preview::new(w, h, &forward) } else { None };
        let band_rows = stream::band_rows(row_bytes, unit);
        let mut y = 0;
        while y < h {
            let n = band_rows.min(h - y);
            let band = bands.rows(y, n)?;
            stream::check_band(&band, w, n, c, depth, F)?;
            let band = match band {
                AnyImage::U16(i) => AnyImage::U16(rgb(i)?),
                AnyImage::F32(i) => AnyImage::F32(rgb(i)?),
                AnyImage::U8(_) => return Err(CodecError::Depth { format: F, depth }),
            };
            if let Some(thumbnail) = &mut thumbnail {
                match &band {
                    AnyImage::U16(i) => thumbnail.add(i, y),
                    AnyImage::F32(i) => thumbnail.add(i, y),
                    AnyImage::U8(_) => {}
                }
            }
            let data = match (packing, &band) {
                (Some(packing), band) => band_strips(band, strip_rows, packing, F)?,
                (None, AnyImage::U16(i)) => ljpeg_tiles(i),
                (None, _) => return Err(CodecError::Depth { format: F, depth }),
            };
            for chunk in &data {
                container.chunk(chunk, &mut chunks)?;
            }
            y += n;
        }

        // The raw image.
        let mut raw = IfdBuilder::new();
        raw.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![dim32(w)]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![dim32(h)]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![if float { 32 } else { 16 }; 3]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::LINEAR_RAW]));
        raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        raw.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
        if float {
            raw.set(t::SAMPLE_FORMAT, Value::Short(vec![3; 3]));
        }
        match compression {
            DngCompression::Uncompressed => raw.set(t::COMPRESSION, Value::Short(vec![1])),
            DngCompression::Deflate => {
                raw.set(t::COMPRESSION, Value::Short(vec![strips::compression_tag(TiffCompression::Deflate)]));
                raw.set(t::PREDICTOR, Value::Short(vec![strips::predictor_for(TiffCompression::Deflate, float)]))
            }
            DngCompression::LosslessJpeg => raw.set(t::COMPRESSION, Value::Short(vec![t::compression::JPEG])),
        };
        raw.set(t::BLACK_LEVEL_REPEAT_DIM, Value::Short(vec![1, 1]));
        raw.set(t::BLACK_LEVEL, Value::Long(vec![0; 3]));
        raw.set(t::WHITE_LEVEL, Value::Long(vec![if float { 1 } else { 65_535 }; 3]));
        raw.set(t::DEFAULT_CROP_ORIGIN, Value::Long(vec![0, 0]));
        raw.set(t::DEFAULT_CROP_SIZE, Value::Long(vec![dim32(w), dim32(h)]));
        container.place(&mut raw, layout, chunks)?;

        // IFD0: the preview with the raw image under it, or the raw image itself; the DNG tags and the metadata.
        let mut ifd = match &thumbnail {
            Some(thumbnail) => {
                let mut ifd = thumbnail.ifd();
                ifd.add_sub_ifd(raw);
                ifd
            }
            None => raw,
        };
        ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
        ifd.set(t::DNG_BACKWARD_VERSION, Value::Byte(vec![1, if float { 4 } else { 3 }, 0, 0]));
        let summary = metadata.summary();
        let camera = [summary.make.as_deref(), summary.model.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let unique =
            if camera.is_empty() { metadata.software().to_owned() } else { format!("{camera} ({})", florastack_core::branding::DISPLAY_NAME) };
        ifd.set(t::UNIQUE_CAMERA_MODEL, Value::Ascii(unique));
        ifd.set(t::COLOR_MATRIX_1, matrix_value(&inverse));
        ifd.set(t::FORWARD_MATRIX_1, matrix_value(&forward));
        ifd.set(t::CALIBRATION_ILLUMINANT_1, Value::Short(vec![ILLUMINANT_D50]));
        ifd.set(t::AS_SHOT_NEUTRAL, Value::Rational(vec![rational(1.0); 3]));
        ifd.set(t::BASELINE_EXPOSURE, Value::SRational(vec![(0, 1)]));
        if look == DngLook::Rendered {
            ifd.set(t::PROFILE_TONE_CURVE, Value::Float(vec![0.0, 0.0, 1.0, 1.0]));
        }
        add_metadata(&mut ifd, metadata, None, &mut warnings);
        let (_, bytes) = container.finish(&[ifd])?;
        Ok(Written { bytes, warnings })
    }
}

#[cfg(test)]
mod tests {
    use florastack_core::ColorSpace;

    use super::*;

    #[test]
    fn the_preview_is_the_mean_of_each_block_in_srgb() {
        // 2500 x 1200 reduces by 3 to 833 x 400; linear sRGB in, so the matrix is the identity.
        let forward = color::rgb_to_xyz_d50(None);
        let mut preview = Preview::new(2500, 1200, &forward).unwrap();
        assert_eq!((preview.factor, preview.width, preview.height), (3, 833, 400));
        // Two bands, cut inside a block: left half black, right half linear 0.5, and non-numbers ignored.
        let band = |rows: usize| {
            Image::from_fn(2500, rows, 3, ColorSpace::LINEAR, |x, _, _| {
                if x == 7 {
                    f32::NAN
                } else if x < 1250 {
                    0.0
                } else {
                    0.5
                }
            })
            .unwrap()
        };
        preview.add(&band(700), 0);
        preview.add(&band(500), 700);
        assert!(preview.counts.iter().all(|&n| n >= 9), "every block is whole; the last one also takes the image's last column");
        let ifd = preview.ifd();
        let Some(ImageData::Strips { strips, .. }) = &ifd.image else { panic!("one strip") };
        let pixel = |x: usize, y: usize| &strips[0][(y * 833 + x) * 3..][..3];
        assert_eq!(pixel(0, 0), [0, 0, 0]);
        assert_eq!(pixel(2, 399), [0, 0, 0], "the block with a NaN column");
        assert_eq!(pixel(832, 200), [188, 188, 188], "linear 0.5 is 188 in sRGB");
        // The block across the edge holds columns 1248 and 1249, black, and 1250, bright: a linear mean of 1/6, which
        // is 113 or 114 in sRGB, in both bands (the cut at row 700 is inside a block of rows).
        for y in [0, 233, 399] {
            let edge = pixel(416, y);
            assert!((112..=115).contains(&edge[0]) && edge[0] == edge[1] && edge[1] == edge[2], "{edge:?}");
        }
        assert_eq!(pixel(417, 0), [188, 188, 188]);
    }

    #[test]
    fn gray_becomes_three_channels_and_alpha_goes() {
        let gray = Image::from_vec(2, 1, 2, ColorSpace::LINEAR, vec![5u16, 9, 7, 9]).unwrap();
        assert_eq!(rgb(gray).unwrap().as_raw(), [5, 5, 5, 7, 7, 7]);
        let rgba = Image::from_vec(1, 1, 4, ColorSpace::LINEAR, vec![1.0f32, 2.0, 3.0, 0.5]).unwrap();
        assert_eq!(rgb(rgba).unwrap().as_raw(), [1.0, 2.0, 3.0]);
    }
}
