//! Display transforms for a viewer ([`DisplayLut`]): what a monitor's ICC profile makes of sRGB, as a 3D table that a
//! shader samples. A viewer encodes its linear sRGB pixels with sRGB's curve and looks them up in the table, which
//! gives the values to send to the display: its own primaries and curve, by relative colorimetric intent. Built
//! with moxcms (the crate's ICC engine for the profiles it writes), behind a panic guard, since a display profile is
//! as much a file from elsewhere as an image's.

use crate::error::{BackendError, CodecError};

/// Grid points per side of the tables a viewer uses: 33, the usual size for display tables, which interpolates
/// within a thousandth of the transform for matrix and curve profiles.
pub const LUT_SIZE: usize = 33;
/// The largest display profile read: 16 MB, far above any monitor's (a few KB, up to a few hundred with tables).
pub const MAX_PROFILE_BYTES: usize = 16 << 20;

/// A display's transform from sRGB-encoded RGB, as a table. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayLut {
    /// Grid points per side.
    pub size: usize,
    /// The display's RGB at each grid point (sRGB-encoded `(r, g, b) / (size - 1)`), red fastest, then green, then
    /// blue: `size³ × 3` values in `[0, 1]`.
    pub rgb: Vec<f32>,
    /// The profile's description, when it has one.
    pub description: Option<String>,
}

impl DisplayLut {
    /// The display RGB at grid point `(r, g, b)`.
    pub fn at(&self, r: usize, g: usize, b: usize) -> Option<[f32; 3]> {
        let i = ((b * self.size + g) * self.size + r) * 3;
        let v = self.rgb.get(i..i + 3)?;
        Some([v[0], v[1], v[2]])
    }
}

/// The table, of `size` points a side (2 to 65), for the display profile `icc`. A profile that isn't an RGB display
/// or color-space profile, can't be read or makes no transform is an error: the viewer then shows sRGB.
pub fn display_lut(icc: &[u8], size: usize) -> Result<DisplayLut, CodecError> {
    let unusable = |why: &'static str| CodecError::Profile { source: BackendError::message(why) };
    if !(2..=65).contains(&size) {
        return Err(unusable("a display table has 2 to 65 points a side"));
    }
    if icc.len() > MAX_PROFILE_BYTES {
        return Err(unusable("the display profile is larger than 16 MB"));
    }
    let parsed = std::panic::catch_unwind(|| moxcms::ColorProfile::new_from_slice(icc)).ok().and_then(Result::ok);
    let Some(display) = parsed else { return Err(unusable("moxcms can't read the display profile")) };
    if display.color_space != moxcms::DataColorSpace::Rgb {
        return Err(unusable("the display profile isn't an RGB profile"));
    }
    if !matches!(display.profile_class, moxcms::ProfileClass::DisplayDevice | moxcms::ProfileClass::ColorSpace) {
        return Err(unusable("the profile isn't a display or color-space profile"));
    }
    let options = moxcms::TransformOptions {
        rendering_intent: moxcms::RenderingIntent::RelativeColorimetric,
        prefer_fixed_point: false,
        ..moxcms::TransformOptions::default()
    };
    let srgb = moxcms::ColorProfile::new_srgb();
    let transform = std::panic::catch_unwind(|| srgb.create_transform_f32(moxcms::Layout::Rgb, &display, moxcms::Layout::Rgb, options))
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| unusable("moxcms makes no transform to the display profile"))?;
    #[allow(clippy::cast_precision_loss, reason = "65 points a side at most")]
    let step = |i: usize| i as f32 / (size - 1) as f32;
    let mut grid = Vec::with_capacity(size * size * size * 3);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                grid.extend([step(r), step(g), step(b)]);
            }
        }
    }
    let mut rgb = vec![0.0; grid.len()];
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| transform.transform(&grid, &mut rgb))).ok().and_then(Result::ok);
    if done.is_none() {
        return Err(unusable("moxcms can't transform to the display profile"));
    }
    for v in &mut rgb {
        *v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    }
    Ok(DisplayLut { size, rgb, description: description(&display) })
}

/// The profile's description, in English when it has a choice.
fn description(profile: &moxcms::ColorProfile) -> Option<String> {
    match profile.description.as_ref()? {
        moxcms::ProfileText::PlainString(text) => Some(text.clone()),
        moxcms::ProfileText::Localizable(strings) => {
            let english = strings.iter().find(|s| s.language == "en").or_else(|| strings.first());
            english.map(|s| s.value.clone())
        }
        moxcms::ProfileText::Description(d) => Some(d.ascii_string.clone()),
    }
    .map(|text| text.trim_end_matches('\0').trim().to_owned())
    .filter(|text| !text.is_empty())
}

/// A standard display profile, as ICC bytes: for tests, and for a viewer that tags its output with the space it
/// sends (macOS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandardDisplay {
    Srgb,
    DisplayP3,
}

/// The ICC bytes of `display`'s standard profile, written by moxcms.
pub fn standard_display_profile(display: StandardDisplay) -> Result<Vec<u8>, CodecError> {
    let profile = match display {
        StandardDisplay::Srgb => moxcms::ColorProfile::new_srgb(),
        StandardDisplay::DisplayP3 => moxcms::ColorProfile::new_display_p3(),
    };
    crate::color::encode_profile(&profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(v: f32) -> f32 {
        if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    }

    #[test]
    fn the_srgb_profile_leaves_srgb_as_it_is() {
        let lut = display_lut(&standard_display_profile(StandardDisplay::Srgb).unwrap(), LUT_SIZE).unwrap();
        assert_eq!((lut.size, lut.rgb.len()), (33, 33 * 33 * 33 * 3));
        let worst = lut.rgb.chunks(3).enumerate().fold(0.0f32, |worst, (i, v)| {
            let want = [i % 33, i / 33 % 33, i / 1089].map(|g| g as f32 / 32.0);
            (0..3).fold(worst, |w, c| w.max((v[c] - want[c]).abs()))
        });
        // moxcms works to 14 bits in floating point, and adapts the white to D50 and back: within a few thousandths.
        assert!(worst < 3e-3, "{worst}");
    }

    #[test]
    fn display_p3_takes_srgb_primaries_to_its_own() {
        let lut = display_lut(&standard_display_profile(StandardDisplay::DisplayP3).unwrap(), LUT_SIZE).unwrap();
        // Linear sRGB red in Display P3's primaries is (0.8225, 0.0332, 0.0171), encoded with the same curve.
        let red = lut.at(32, 0, 0).unwrap();
        let want = [0.8225f32, 0.0332, 0.0171].map(encode);
        for c in 0..3 {
            assert!((red[c] - want[c]).abs() < 4e-3, "red: {red:?} against {want:?}");
        }
        // White and gray stay neutral; black stays black.
        let white = lut.at(32, 32, 32).unwrap();
        assert!(white.iter().all(|v| (v - 1.0).abs() < 3e-3), "{white:?}");
        let gray = lut.at(16, 16, 16).unwrap();
        assert!(gray.iter().all(|v| (v - 0.5).abs() < 3e-3), "{gray:?}");
        assert_eq!(lut.at(0, 0, 0), Some([0.0, 0.0, 0.0]));
        assert!(lut.description.is_some());
    }

    #[test]
    fn profiles_a_display_cant_use_are_errors() {
        for bytes in [Vec::new(), vec![0u8; 200], b"not a profile at all".repeat(20)] {
            assert!(display_lut(&bytes, LUT_SIZE).is_err());
        }
        let gray = crate::color::encode_profile(&moxcms::ColorProfile::new_gray_with_gamma(2.2)).unwrap();
        assert!(display_lut(&gray, LUT_SIZE).is_err(), "a gray profile");
        let srgb = standard_display_profile(StandardDisplay::Srgb).unwrap();
        assert!(display_lut(&srgb, 1).is_err() && display_lut(&srgb, 66).is_err());
        // Cut short anywhere, it fails without a panic.
        for end in (0..srgb.len()).step_by(37) {
            let _ = display_lut(&srgb[..end], 5);
        }
    }
}
