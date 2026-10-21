//! Helpers shared by florastack-raw's tests: synthetic RAW frames built with the public API (a scene sampled through a
//! color filter array, as a sensor would), synthetic DNG files written with LightCraft's DNG writer, and comparisons.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use florastack_core::{Image, Orientation, Rect};
use florastack_raw::{CameraColor, Cfa, ColorSource, Mosaic, RawFormat, RawFrame, RawInfo};

/// The Fujifilm X-Trans layout (LightCraft's, anchored at the sensor's top-left corner).
pub fn xtrans() -> Cfa {
    Cfa::new(6, 6, lightcraft_raw::Cfa::xtrans().pattern).unwrap()
}

/// A plausible camera: CIE XYZ to camera RGB (rows sum to something sensible; invertible), measured under D65.
pub const CAMERA_MATRIX: [[f64; 3]; 3] = [[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]];

/// D65's XYZ.
const D65_XYZ: [f64; 3] = [0.950_47, 1.0, 1.088_83];

/// The camera RGB of a neutral surface under D65 for [`CAMERA_MATRIX`], the largest channel 1.
pub fn camera_neutral() -> [f64; 3] {
    let n: Vec<f64> = CAMERA_MATRIX.iter().map(|row| row.iter().zip(D65_XYZ).map(|(m, v)| m * v).sum()).collect();
    let max = n.iter().copied().fold(f64::MIN, f64::max);
    [n[0] / max, n[1] / max, n[2] / max]
}

/// A DNG-style color model: [`CAMERA_MATRIX`] at D65, with the camera's neutral as its as-shot white.
pub fn dng_color() -> CameraColor {
    CameraColor {
        illuminants: [21, 0],
        color_matrices: [Some(CAMERA_MATRIX), None],
        as_shot_neutral: Some(camera_neutral()),
        source: ColorSource::File,
        ..CameraColor::default()
    }
}

/// A frame whose sensor sees `scene(x, y)` (camera RGB, not white balanced, white = 1) through `cfa`, or all three
/// channels when `cfa` is `None` (a converted, linear frame).
pub fn frame(width: usize, height: usize, cfa: Option<Cfa>, color: CameraColor, scene: impl Fn(usize, usize) -> [f32; 3]) -> RawFrame {
    let channels = if cfa.is_some() { 1 } else { 3 };
    let mut samples = Vec::with_capacity(width * height * channels);
    for y in 0..height {
        for x in 0..width {
            let p = scene(x, y);
            match &cfa {
                Some(cfa) => samples.push(p[usize::from(cfa.color_at(x, y))]),
                None => samples.extend_from_slice(&p),
            }
        }
    }
    let layout = cfa.as_ref().map_or(florastack_raw::SensorLayout::Linear, Cfa::layout);
    let info = RawInfo {
        format: RawFormat::Dng,
        make: Some("Synthetic".into()),
        model: Some("Test".into()),
        width,
        height,
        layout,
        bits: 16,
        orientation: Orientation::Normal,
        color: color.source,
        as_shot_white: None,
        distortion_correction: false,
    };
    let sensor = Mosaic { width, height, channels, samples, cfa, float: false };
    RawFrame::new(info, sensor, color, Rect::new(0, 0, width, height), 1.0).unwrap()
}

/// A smooth color field of camera RGB values between about 0.1 and 0.8: the "smooth region" of the demosaic
/// agreement tests (its features are tens of pixels wide).
pub fn smooth_scene(x: usize, y: usize) -> [f32; 3] {
    let (fx, fy) = (x as f32 / 37.0, y as f32 / 29.0);
    [0.45 + 0.3 * fx.sin() * fy.cos(), 0.4 + 0.25 * (fx * 0.7 + fy * 0.5).cos(), 0.35 + 0.2 * (fy * 0.9).sin()]
}

/// The pixel `(x, y)` of a developed three-channel image.
pub fn px(image: &Image<f32>, x: usize, y: usize) -> [f32; 3] {
    let p = image.pixel(x, y).unwrap();
    [p[0], p[1], p[2]]
}

/// Whether two images have the same size and the same samples, bit for bit.
pub fn identical(a: &Image<f32>, b: &Image<f32>) -> bool {
    a.width() == b.width()
        && a.height() == b.height()
        && a.rows().zip(b.rows()).all(|(r, s)| r.iter().zip(s).all(|(x, y)| x.to_bits() == y.to_bits()))
}

/// The largest difference between two images' samples, `border` pixels from each edge left out.
pub fn max_difference(a: &Image<f32>, b: &Image<f32>, border: usize) -> f32 {
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    let mut worst = 0.0f32;
    for y in border..a.height() - border {
        for x in border..a.width() - border {
            for (p, q) in px(a, x, y).iter().zip(px(b, x, y)) {
                worst = worst.max((p - q).abs());
            }
        }
    }
    worst
}

/// A LightCraft raw image to write as a DNG: `width` x `height` samples from `scene` through `cfa` (or RGB), stored as
/// 16-bit values over a black level of `black` with white at `white`, and these color tags.
pub fn lightcraft_raw(
    width: usize,
    height: usize,
    cfa: Option<&Cfa>,
    color: lightcraft_raw::ColorData,
    scene: impl Fn(usize, usize) -> [f32; 3],
) -> lightcraft_raw::RawImage {
    let (black, white) = (512.0f32, 16383.0f32);
    let cpp = if cfa.is_some() { 1 } else { 3 };
    let mut data = Vec::with_capacity(width * height * cpp);
    for y in 0..height {
        for x in 0..width {
            let p = scene(x, y);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Between black and white: in range.
            let code = |v: f32| (black + v.clamp(0.0, 1.0) * (white - black)).round() as u16;
            match cfa {
                Some(cfa) => data.push(code(p[usize::from(cfa.color_at(x, y))])),
                None => data.extend(p.iter().map(|&v| code(v))),
            }
        }
    }
    let full = lightcraft_raw::Rect::new(0, 0, width, height);
    lightcraft_raw::RawImage {
        format: lightcraft_raw::RawFormat::Dng,
        width,
        height,
        cpp,
        data: lightcraft_raw::RawData::U16(data),
        cfa: cfa.map(|c| lightcraft_raw::Cfa { width: c.width(), height: c.height(), pattern: c.pattern().to_vec() }),
        bits: 14,
        black: lightcraft_raw::BlackLevel::uniform(black),
        white: vec![white],
        active_area: full,
        crop: full,
        orientation: lightcraft_raw::Orientation::Normal,
        color,
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: lightcraft_raw::OpcodeLists::default(),
        metadata: lightcraft_raw::Metadata { make: Some("Synthetic".into()), model: Some("Test".into()), ..Default::default() },
        lens_metadata: lightcraft_raw::LensMetadata::default(),
    }
}

/// `raw` as an uncompressed DNG file.
pub fn dng(raw: &lightcraft_raw::RawImage) -> Vec<u8> {
    let options = lightcraft_raw::DngWriteOptions { compression: lightcraft_raw::DngCompression::Uncompressed, ..Default::default() };
    lightcraft_raw::write_dng(raw, &options).unwrap()
}

/// The color tags of a file whose samples are linear Rec. 2020 already, as FloraStack's own DNG writer describes them:
/// `ForwardMatrix1` the space's colorants (RGB to XYZ relative to D50), `ColorMatrix1` their inverse, D50 and an
/// as-shot neutral of (1, 1, 1).
pub fn rec2020_tags() -> lightcraft_raw::ColorData {
    let forward = lightcraft_codecs::space::rgb_to_xyz_d50(&lightcraft_codecs::NamedSpace::Rec2020.rgb_space());
    lightcraft_raw::ColorData {
        illuminant: [23, 0],
        color_matrix: [forward.inverse(), None],
        forward_matrix: [Some(forward), None],
        as_shot_neutral: Some([1.0; 3]),
        ..lightcraft_raw::ColorData::default()
    }
}
