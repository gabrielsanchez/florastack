//! The camera color model's math, done by LightCraft's implementation of the DNG specification's chapter 6
//! (`lightcraft_raw::color`): white-balance multipliers and the matrix from white-balanced camera RGB to linear
//! Rec. 2020 for a chosen white, and the ICC profile that developed frames are tagged with.

use std::sync::{Arc, OnceLock};

use crate::frame::{CameraColor, Chromaticity};

/// How a frame's camera RGB becomes linear Rec. 2020 for one white: `rec2020 = matrix · (multipliers ⊙ camera)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Transform {
    /// White-balance multipliers for red, green and blue, the smallest 1, so that the brightest neutral the sensor
    /// records without clipping comes out at 1.0.
    pub multipliers: [f32; 3],
    /// White-balanced camera RGB to linear Rec. 2020 (D65), mapping (1, 1, 1) to (1, 1, 1).
    pub matrix: [[f32; 3]; 3],
}

impl Transform {
    /// What a monochrome sensor gets: its one channel as gray.
    pub const NEUTRAL: Transform = Transform { multipliers: [1.0; 3], matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
}

/// LightCraft's color tags for `color` (no profile look: FloraStack never renders one).
pub(crate) fn color_data(color: &CameraColor) -> lightcraft_raw::ColorData {
    let m = |m: &Option<[[f64; 3]; 3]>| m.map(lightcraft_raw::Mat3);
    lightcraft_raw::ColorData {
        illuminant: color.illuminants,
        color_matrix: [m(&color.color_matrices[0]), m(&color.color_matrices[1])],
        forward_matrix: [m(&color.forward_matrices[0]), m(&color.forward_matrices[1])],
        camera_calibration: [m(&color.calibrations[0]), m(&color.calibrations[1])],
        analog_balance: color.analog_balance,
        as_shot_neutral: color.as_shot_neutral,
        as_shot_white_xy: color.as_shot_white.map(Chromaticity::xy),
        baseline_exposure: color.baseline_exposure,
        ..lightcraft_raw::ColorData::default()
    }
}

/// The white the camera recorded, or D65 when it recorded none.
pub(crate) fn as_shot_white(color: &CameraColor) -> Chromaticity {
    Chromaticity::from_xy(lightcraft_raw::color::as_shot_white_of(&color_data(color), color.camera_multipliers))
}

/// The transform of a frame with `color` for the scene white `white`.
pub(crate) fn transform(color: &CameraColor, white: Chromaticity) -> Transform {
    let t = lightcraft_raw::color::camera_transform_of(&color_data(color), white.xy());
    let multipliers = if t.wb.iter().all(|v| v.is_finite() && *v > 0.0) { t.wb } else { [1.0; 3] };
    let matrix = t.matrix.to_f32();
    let matrix = if matrix.iter().flatten().all(|v| v.is_finite()) { matrix } else { Transform::NEUTRAL.matrix };
    Transform { multipliers, matrix }
}

/// The ICC profile of developed frames: linear light in Rec. 2020 primaries with a D65 white. The bytes are
/// LightCraft's, as florastack-codecs writes the same profile, so an output in the input's profile keeps it.
pub fn working_space_icc() -> Arc<[u8]> {
    static ICC: OnceLock<Arc<[u8]>> = OnceLock::new();
    ICC.get_or_init(|| {
        let space = lightcraft_codecs::NamedSpace::Rec2020.rgb_space();
        Arc::from(lightcraft_codecs::icc::write_matrix_trc(&space, &lightcraft_codecs::Trc::Linear))
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_space_profile_is_linear_rec2020() {
        let icc = working_space_icc();
        let info = lightcraft_codecs::icc::parse(&icc).unwrap();
        assert_eq!(info.named, Some(lightcraft_codecs::NamedSpace::Rec2020));
        let linear = |t: &lightcraft_codecs::Trc| matches!(t, lightcraft_codecs::Trc::Linear) || *t == lightcraft_codecs::Trc::Parametric(vec![1.0]);
        assert!(matches!(info.kind, lightcraft_codecs::icc::IccKind::MatrixTrc { ref trc, .. } if trc.iter().all(linear)), "{:?}", info.kind);
        assert!(Arc::ptr_eq(&icc, &working_space_icc()), "made once");
    }

    #[test]
    fn without_matrices_the_fallback_still_balances_white() {
        // No matrices: camera RGB is taken for linear sRGB, and its as-shot multipliers make white neutral.
        let color = CameraColor { camera_multipliers: Some([2.0, 1.0, 1.5]), ..CameraColor::default() };
        let white = as_shot_white(&color);
        let t = transform(&color, white);
        let rel = |a: f32, b: f32| (a / b - 1.0).abs();
        assert!(rel(t.multipliers[0], 2.0) < 1e-3 && rel(t.multipliers[2], 1.5) < 1e-3, "{:?}", t.multipliers);
        let neutral = [0.5 * t.multipliers[0], 1.0, 1.0 / 1.5 * t.multipliers[2]];
        let out: Vec<f32> = t.matrix.iter().map(|row| row.iter().zip(neutral).map(|(m, v)| m * v).sum()).collect();
        assert!(out.iter().all(|v| (v - 1.0).abs() < 2e-3), "{out:?}");
        assert_eq!(as_shot_white(&CameraColor::default()), Chromaticity::D65, "no record: D65");
    }
}
