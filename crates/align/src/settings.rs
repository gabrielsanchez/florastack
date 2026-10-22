//! How a stack is aligned: [`AlignSettings`], chosen once per stack and stored in its project (the `align` preference
//! group), with serde support for project files and presets and a JSON schema for commands.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::AlignError;

/// The largest `max_shift` and `max_scale`, in percent.
const MAX_PERCENT: f64 = 50.0;
/// The largest `max_rotation`, in degrees.
const MAX_ROTATION: f64 = 45.0;
/// The smallest and largest `resolution`, in pixels.
pub const MIN_RESOLUTION: u32 = 64;
pub const MAX_RESOLUTION: u32 = 16_384;

/// One of the geometric terms of a frame's alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Term {
    ShiftX,
    ShiftY,
    Scale,
    Rotation,
}

impl Term {
    pub const ALL: [Term; 4] = [Term::ShiftX, Term::ShiftY, Term::Scale, Term::Rotation];

    /// The term's name, as its setting spells it.
    pub fn name(self) -> &'static str {
        match self {
            Term::ShiftX => "shift_x",
            Term::ShiftY => "shift_y",
            Term::Scale => "scale",
            Term::Rotation => "rotation",
        }
    }
}

/// How a stack's frames are aligned to its master frame. Every field has a default, so that a preset can name only
/// some: by default every term is corrected, within generous limits.
///
/// Each term the stack doesn't need can be turned off, which makes alignment both faster and more accurate: a rail
/// moves the camera without turning it (`rotation = false`), and steady lighting keeps the brightness
/// (`brightness = false`). A term that is off is not estimated at all: it stays exactly 0 (shifts, rotation) or 1
/// (scale, gain).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // Independent switches, one per term, as presets and the settings panel name them.
pub struct AlignSettings {
    /// Correct horizontal shifts.
    pub shift_x: bool,
    /// Correct vertical shifts.
    pub shift_y: bool,
    /// Correct changes of scale: focus breathing, or the magnification a rail changes as it moves.
    pub scale: bool,
    /// Correct rotation.
    pub rotation: bool,
    /// Match each frame's brightness to the master's with a gain.
    pub brightness: bool,
    /// After chaining each frame to its neighbor, align it again directly against the master, starting from the
    /// chained estimate, so that the small errors of a long chain don't add up. The direct estimate is kept only
    /// when it scores at least `min_score`: frames focused far from the master share little detail with it, so this
    /// helps shallow stacks of similar frames most.
    pub refine: bool,
    /// The largest shift relative to the master, in percent of the frame's width horizontally and of its height
    /// vertically, from 0 to 50.
    #[schemars(range(min = 0.0, max = 50.0))]
    pub max_shift: f64,
    /// The largest change of scale relative to the master, in percent, from 0 to 50.
    #[schemars(range(min = 0.0, max = 50.0))]
    pub max_scale: f64,
    /// The largest rotation relative to the master, in degrees, from 0 to 45.
    #[schemars(range(min = 0.0, max = 45.0))]
    pub max_rotation: f64,
    /// The lowest score at which a frame counts as matched, from 0 to 1: the correlation of the frame's band-passed
    /// luminance with its reference's once aligned. On the synthetic test stacks, neighbors score above 0.9 and a
    /// frame of another scene near 0.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub min_score: f64,
    /// Reject the frames whose alignment hit a limit, scored below `min_score` or found no match: a rejected frame
    /// keeps the transform of the frame it was compared with, and the next frame is aligned to the last frame that
    /// wasn't rejected. Off, such frames are only flagged.
    pub reject: bool,
    /// The longer side, in pixels, of the finest level alignment works at, from 64 to 16384: larger frames are
    /// reduced to it first. Alignment is sub-pixel at that level, so higher values cost time for little gain.
    #[schemars(range(min = 64, max = 16384))]
    pub resolution: u32,
}

impl Default for AlignSettings {
    fn default() -> Self {
        AlignSettings {
            shift_x: true,
            shift_y: true,
            scale: true,
            rotation: true,
            brightness: true,
            refine: false,
            max_shift: 10.0,
            max_scale: 15.0,
            max_rotation: 5.0,
            min_score: 0.3,
            reject: true,
            resolution: 2048,
        }
    }
}

impl AlignSettings {
    /// Checks the settings that a type can't: the ranges of the limits, the score and the resolution.
    pub fn validate(&self) -> Result<(), AlignError> {
        let limits = [
            ("max_shift", self.max_shift, MAX_PERCENT, "between 0 and 50 percent"),
            ("max_scale", self.max_scale, MAX_PERCENT, "between 0 and 50 percent"),
            ("max_rotation", self.max_rotation, MAX_ROTATION, "between 0 and 45 degrees"),
            ("min_score", self.min_score, 1.0, "between 0 and 1"),
        ];
        for (name, value, max, expected) in limits {
            if !(0.0..=max).contains(&value) {
                return Err(AlignError::Setting { name, value, expected });
            }
        }
        if !(MIN_RESOLUTION..=MAX_RESOLUTION).contains(&self.resolution) {
            return Err(AlignError::Setting { name: "resolution", value: f64::from(self.resolution), expected: "between 64 and 16384 pixels" });
        }
        Ok(())
    }

    /// Whether `term` is corrected.
    pub fn corrects(&self, term: Term) -> bool {
        match term {
            Term::ShiftX => self.shift_x,
            Term::ShiftY => self.shift_y,
            Term::Scale => self.scale,
            Term::Rotation => self.rotation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_correct_everything_and_validate() {
        let settings = AlignSettings::default();
        assert!(Term::ALL.iter().all(|&t| settings.corrects(t)) && settings.brightness);
        assert_eq!(settings.validate(), Ok(()));
    }

    #[test]
    fn out_of_range_settings_are_errors() {
        let bad = [
            AlignSettings { max_shift: -1.0, ..AlignSettings::default() },
            AlignSettings { max_scale: 51.0, ..AlignSettings::default() },
            AlignSettings { max_rotation: f64::NAN, ..AlignSettings::default() },
            AlignSettings { min_score: 1.5, ..AlignSettings::default() },
            AlignSettings { resolution: 32, ..AlignSettings::default() },
            AlignSettings { resolution: u32::MAX, ..AlignSettings::default() },
        ];
        let names: Vec<&str> = bad
            .iter()
            .map(|s| match s.validate() {
                Err(AlignError::Setting { name, .. }) => name,
                other => panic!("{s:?}: {other:?}"),
            })
            .collect();
        assert_eq!(names, ["max_shift", "max_scale", "max_rotation", "min_score", "resolution", "resolution"]);
        let edges = AlignSettings { max_shift: 0.0, max_scale: 50.0, max_rotation: 45.0, min_score: 1.0, resolution: 64, ..AlignSettings::default() };
        assert_eq!(edges.validate(), Ok(()));
    }

    #[test]
    fn settings_round_trip_through_json_and_partial_toml() {
        let settings = AlignSettings { rotation: false, max_shift: 2.5, resolution: 1024, ..AlignSettings::default() };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<AlignSettings>(&json).unwrap(), settings);
        // A preset names only what it changes.
        let rail: AlignSettings = toml::from_str("rotation = false\nbrightness = false\n").unwrap();
        assert_eq!(rail, AlignSettings { rotation: false, brightness: false, ..AlignSettings::default() });
        assert!(toml::from_str::<AlignSettings>("rotaton = false\n").is_err(), "a misspelled key is an error");
        assert!(toml::from_str::<AlignSettings>("resolution = -5\n").is_err());
    }
}
