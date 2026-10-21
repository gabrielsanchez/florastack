//! Linear DNG (Adobe's Digital Negative; the files declare `DNGVersion` 1.4 and `DNGBackwardVersion` 1.3) for
//! stacked output, written with lightcraft-tiff's IFD writer: one `LinearRaw` image of three 16-bit samples per pixel
//! in IFD0, uncompressed or in lossless JPEG tiles (encoded by lightcraft-raw's `ljpeg`). The tests read the files
//! back with LightCraft's RAW decoder; Adobe's own readers haven't checked them yet (docs/ROADMAP.md, 1.2).
//!
//! The samples are linear light in the output profile's primaries, white balanced, white at 65535. The color tags
//! describe exactly that space: `ForwardMatrix1` is its colorant matrix (RGB to XYZ relative to D50, as ICC profiles
//! store it), `ColorMatrix1` the inverse, `CalibrationIlluminant1` D50 and `AsShotNeutral` (1, 1, 1). The
//! `UniqueCameraModel` names FloraStack after the camera, so that no raw converter applies a camera profile made
//! for the sensor's own colors. A rendered stack gets an identity `ProfileToneCurve`, so that the converter adds no
//! tone curve to what is already rendered.

use florastack_core::{AnyImage, Image};
use lightcraft_tiff::writer::{rational, srational};
use lightcraft_tiff::{IfdBuilder, ImageData, Value, tags as t};
use rayon::prelude::*;

use super::strips::{self};
use super::tiff::{add_metadata, write};
use super::{dim32, first_channels};
use crate::codec::{Encoded, ImageEncoder};
use crate::color::{self, Profile};
use crate::error::{BackendError, CodecError, Warning};
use crate::format::Format;
use crate::metadata::PreparedMetadata;
use crate::options::{DngCompression, DngLook, OutputFormat, OutputSpec, TiffCompression};

pub(crate) struct DngEncoder;

const F: Format = Format::Dng;

/// The side of a lossless JPEG tile.
const TILE: usize = 256;

/// D50, as an EXIF `LightSource` code (`CalibrationIlluminant1`).
const ILLUMINANT_D50: u16 = 23;

fn matrix_value(m: &[[f64; 3]; 3]) -> Value {
    Value::SRational(m.iter().flatten().map(|&v| srational(v)).collect())
}

fn invert(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    lightcraft_color::Mat3(*m).inverse().map(|i| i.0)
}

/// The three-channel samples to store: gray expanded, alpha dropped.
fn rgb(image: &Image<u16>, warnings: &mut Vec<Warning>) -> Result<Image<u16>, CodecError> {
    let c = image.channels();
    if c == 2 || c == 4 {
        warnings.push(Warning::AlphaDropped(F));
    }
    let samples: Vec<u16> = if c >= 3 {
        first_channels(image, 3)
    } else {
        warnings.push(Warning::GrayExpanded(F));
        image.rows().flat_map(|row| row.chunks_exact(c).flat_map(|px| [px.first().copied().unwrap_or(0); 3])).collect()
    };
    Ok(Image::from_vec(image.width(), image.height(), 3, image.color_space().clone(), samples)?)
}

/// Lossless JPEG tiles of `TILE` x `TILE` pixels in row-major order, the edge tiles padded with the last row and
/// column.
fn ljpeg_tiles(image: &Image<u16>) -> Vec<Vec<u8>> {
    let (w, h) = (image.width(), image.height());
    let (across, down) = (w.div_ceil(TILE), h.div_ceil(TILE));
    (0..across * down)
        .into_par_iter()
        .map(|i| {
            let (tx, ty) = (i % across * TILE, i / across * TILE);
            let mut buf = Vec::with_capacity(TILE * TILE * 3);
            for y in 0..TILE {
                let sy = (ty + y).min(h - 1);
                for x in 0..TILE {
                    let sx = (tx + x).min(w - 1);
                    buf.extend_from_slice(image.pixel(sx, sy).unwrap_or(&[0, 0, 0]));
                }
            }
            lightcraft_raw::ljpeg::encode(&buf, TILE, TILE, 3, 16, 1, 0)
        })
        .collect()
}

impl ImageEncoder for DngEncoder {
    fn format(&self) -> Format {
        F
    }

    fn encode(&self, image: &AnyImage, profile: Option<&Profile>, metadata: &PreparedMetadata, spec: &OutputSpec) -> Result<Encoded, CodecError> {
        let OutputFormat::Dng { compression, look } = spec.format else {
            return Err(CodecError::encode(F, BackendError::message("not DNG options")));
        };
        let AnyImage::U16(image) = image else {
            return Err(CodecError::Depth { format: F, depth: spec.depth });
        };
        let mut warnings = Vec::new();
        let image = rgb(image, &mut warnings)?;
        let (w, h) = (image.width(), image.height());
        let forward = color::rgb_to_xyz_d50(profile);
        let inverse = invert(&forward).ok_or_else(|| CodecError::encode(F, BackendError::message("the profile's colorants are singular")))?;
        let mut ifd = IfdBuilder::new();
        ifd.set(t::NEW_SUBFILE_TYPE, Value::Long(vec![0]));
        ifd.set(t::IMAGE_WIDTH, Value::Long(vec![dim32(w)]));
        ifd.set(t::IMAGE_LENGTH, Value::Long(vec![dim32(h)]));
        ifd.set(t::BITS_PER_SAMPLE, Value::Short(vec![16; 3]));
        ifd.set(t::PHOTOMETRIC, Value::Short(vec![t::photometric::LINEAR_RAW]));
        ifd.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        ifd.set(t::PLANAR_CONFIGURATION, Value::Short(vec![1]));
        ifd.set(t::DNG_VERSION, Value::Byte(vec![1, 4, 0, 0]));
        ifd.set(t::DNG_BACKWARD_VERSION, Value::Byte(vec![1, 3, 0, 0]));
        let summary = metadata.summary();
        let camera = [summary.make.as_deref(), summary.model.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let unique =
            if camera.is_empty() { metadata.software().to_owned() } else { format!("{camera} ({})", florastack_core::branding::DISPLAY_NAME) };
        ifd.set(t::UNIQUE_CAMERA_MODEL, Value::Ascii(unique));
        ifd.set(t::COLOR_MATRIX_1, matrix_value(&inverse));
        ifd.set(t::FORWARD_MATRIX_1, matrix_value(&forward));
        ifd.set(t::CALIBRATION_ILLUMINANT_1, Value::Short(vec![ILLUMINANT_D50]));
        ifd.set(t::AS_SHOT_NEUTRAL, Value::Rational(vec![rational(1.0); 3]));
        ifd.set(t::BLACK_LEVEL_REPEAT_DIM, Value::Short(vec![1, 1]));
        ifd.set(t::BLACK_LEVEL, Value::Long(vec![0; 3]));
        ifd.set(t::WHITE_LEVEL, Value::Long(vec![65_535; 3]));
        ifd.set(t::BASELINE_EXPOSURE, Value::SRational(vec![(0, 1)]));
        ifd.set(t::DEFAULT_CROP_ORIGIN, Value::Long(vec![0, 0]));
        ifd.set(t::DEFAULT_CROP_SIZE, Value::Long(vec![dim32(w), dim32(h)]));
        if look == DngLook::Rendered {
            ifd.set(t::PROFILE_TONE_CURVE, Value::Float(vec![0.0, 0.0, 1.0, 1.0]));
        }
        let data_bytes = match compression {
            DngCompression::Uncompressed => {
                ifd.set(t::COMPRESSION, Value::Short(vec![1]));
                let rows = strips::rows_per_strip(w * 3 * 2, h);
                let data = strips::strips(&image, rows, TiffCompression::None, F)?;
                let bytes = data.iter().map(|s| s.len() as u64).sum();
                ifd.set_image(ImageData::Strips { rows_per_strip: dim32(rows), strips: data });
                bytes
            }
            DngCompression::LosslessJpeg => {
                ifd.set(t::COMPRESSION, Value::Short(vec![t::compression::JPEG]));
                let tiles = ljpeg_tiles(&image);
                let bytes = tiles.iter().map(|s| s.len() as u64).sum();
                ifd.set_image(ImageData::Tiles { tile_width: dim32(TILE), tile_height: dim32(TILE), tiles });
                bytes
            }
        };
        add_metadata(&mut ifd, metadata, None, &mut warnings);
        let bytes = write(ifd, data_bytes, F)?;
        Ok(Encoded { bytes, warnings })
    }
}
