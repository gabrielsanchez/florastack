//! Diffraction-only object-space depth, using the wave term λ n / NA² in Nikon `MicroscopyU`,
//! <https://www.microscopyu.com/microscopy-basics/depth-of-field-and-depth-of-focus>.
//! Air (n=1), green light (λ=0.55 µm), paraxial thin lens, pupil magnification one:
//! `N_eff=N(1+m)`, `NA≈m/(2 N_eff)`, `depth=4 λ (N_eff/m)²`. We recommend half this depth.
//! Pixel pitch assesses sampling; it does not enlarge the depth with a circle of confusion.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Optics {
    /// Image size divided by object size, e.g. 1 for 1:1 macro.
    #[schemars(range(min = 0.01, max = 100))]
    pub magnification: f64,
    /// Marked f-number at infinity, before extension (not the camera's effective f-number).
    #[schemars(range(min = 0.5, max = 64))]
    pub aperture: f64,
    /// Sensor pixel spacing in micrometers.
    #[schemars(range(min = 0.1, max = 100))]
    pub pixel_pitch: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Recommendation {
    pub effective_aperture: f64,
    pub depth_micrometers: f64,
    pub step_micrometers: f64,
    pub airy_diameter_pixels: f64,
    pub explanation: String,
}

impl Optics {
    pub fn validate(&self) -> Result<(), Error> {
        for (name, value, min, max) in
            [("magnification", self.magnification, 0.01, 100.0), ("aperture", self.aperture, 0.5, 64.0), ("pixelPitch", self.pixel_pitch, 0.1, 100.0)]
        {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(Error::Invalid(format!("{name} must be finite and between {min} and {max}")));
            }
        }
        Ok(())
    }

    pub fn recommend(&self) -> Result<Recommendation, Error> {
        self.validate()?;
        let effective_aperture = self.aperture * (1.0 + self.magnification);
        let depth = 4.0 * 0.55 * (effective_aperture / self.magnification).powi(2);
        let airy = 2.44 * 0.55 * effective_aperture / self.pixel_pitch;
        Ok(Recommendation {
            effective_aperture,
            depth_micrometers: depth,
            step_micrometers: depth * 0.5,
            airy_diameter_pixels: airy,
            explanation: format!(
                "Air, 550 nm, pupil ratio 1: effective f/{effective_aperture:.2}; diffraction depth {depth:.2} µm. \
                 Use 50% overlap: {:.2} µm rail spacing. Airy diameter {airy:.2} pixels: {}. \
                 Approximate paraxial model; verify a trial stack for aberrations and occlusions. \
                 Motor pulses are uncalibrated and cannot be converted to this physical spacing.",
                depth * 0.5,
                if airy < 2.0 { "sensor undersamples the Airy disk" } else { "diffraction spans multiple pixels" }
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_cases_and_sampling() {
        let r = Optics { magnification: 1.0, aperture: 4.0, pixel_pitch: 4.0 }.recommend().unwrap();
        assert!((r.depth_micrometers - 140.8).abs() < 1e-10);
        assert!((r.step_micrometers - 70.4).abs() < 1e-10);
        assert!((r.airy_diameter_pixels - 2.684).abs() < 1e-10);
        let r = Optics { magnification: 5.0, aperture: 2.8, pixel_pitch: 4.0 }.recommend().unwrap();
        assert!((r.step_micrometers - 12.41856).abs() < 1e-10);
        let r2 = Optics { magnification: 5.0, aperture: 2.8, pixel_pitch: 8.0 }.recommend().unwrap();
        assert_eq!(r2.step_micrometers, r.step_micrometers);
        assert_eq!(r2.airy_diameter_pixels * 2.0, r.airy_diameter_pixels);
    }

    #[test]
    fn invalid_inputs() {
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e30] {
            assert!(Optics { magnification: value, aperture: 4.0, pixel_pitch: 4.0 }.recommend().is_err());
            assert!(Optics { magnification: 1.0, aperture: value, pixel_pitch: 4.0 }.recommend().is_err());
            assert!(Optics { magnification: 1.0, aperture: 4.0, pixel_pitch: value }.recommend().is_err());
        }
    }
}
