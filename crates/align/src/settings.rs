//! How a stack is aligned, ordered and framed: [`AlignSettings`], chosen once per stack and stored in its project (the
//! `align` preference group), with serde support for project files and presets and a JSON schema for commands.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::AlignError;
use crate::framing::Framing;

/// The largest `max_shift` and `max_scale`, in percent.
const MAX_PERCENT: f64 = 50.0;
/// The largest `max_rotation`, in degrees.
const MAX_ROTATION: f64 = 45.0;
/// The default `min_sharp_fraction`: a frame that is among the sharpest on less than 0.2% of the stack's in-focus
/// detail has, for all practical purposes, none (see `focus.rs`).
pub const DEFAULT_MIN_SHARP_FRACTION: f64 = 0.002;
/// The default `min_motion`, in frame pixels.
pub const DEFAULT_MIN_MOTION: f64 = 2.0;
/// The smallest and largest `min_motion`, in frame pixels.
const MOTION_RANGE: (f64, f64) = (0.1, 100.0);
/// The smallest and largest `resolution`, in pixels.
pub const MIN_RESOLUTION: u32 = 64;
pub const MAX_RESOLUTION: u32 = 16_384;

/// One of the geometric terms of a frame's alignment, named in project files as its setting is (`shift_x`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
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

/// How a stack's frames are aligned to its master frame, in which order the stack is processed, how its output is
/// framed, when a frame counts as out of focus or moved, and whether sensor defects are detected. Every field has a
/// default, so that a preset can name only some: by default every term is corrected, within generous limits, the stack
/// is processed from its narrow end and the output is cropped to that end's field of view.
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
    /// Process the stack from the end with the narrower field of view, found from the scales of its first and last
    /// frames: the frames are aligned to that end's frame, and with the default framing every pixel of the output is
    /// covered by every frame. Off, the stack is processed from its first frame.
    pub automatic_order: bool,
    /// Process the stack from the other end: from its wide end with `automatic_order`, from its last frame without.
    pub reverse_order: bool,
    /// The output's field of view: `narrow` crops to the narrow end's (every frame covers it), `wide` keeps the
    /// widest, marking what each frame doesn't cover in its coverage mask, and `common` crops to what every frame
    /// covers once aligned.
    pub framing: Framing,
    /// A frame is flagged as out of focus when its sharp-content fraction, the share of the stack's in-focus detail on
    /// which it is among the sharpest frames, is less than this, from 0 to 1 (0 flags none). Such frames, common at the
    /// start and the end of a stack, add nothing but blur; they are only flagged, for the user to exclude.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub min_sharp_fraction: f64,
    /// The smallest local movement flagged, in pixels of the frames, from 0.1 to 100: a region of a frame that moved at
    /// least this much against its neighbor, once aligned, is marked in its motion map, and a frame with such a region
    /// is flagged as moved (`motion.rs`). Movement is measured on a level of at most 1024 pixels, and below three
    /// quarters of a pixel there it can't be told from the apparent motion of defocus and noise: on larger frames, the
    /// threshold is at least that many frame pixels (4.4 on a 6000-pixel frame).
    #[schemars(range(min = 0.1, max = 100.0))]
    pub min_motion: f64,
    /// Before aligning, when the project has no defect mask, detect one from the frames (dust and hot pixels, which
    /// stay at the same sensor pixels) and inpaint it in every frame. Off by default: review a detected mask before
    /// stacking with it (`defects.rs`). A mask the project has, detected or supplied, is always inpainted.
    pub detect_defects: bool,
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
            automatic_order: true,
            reverse_order: false,
            framing: Framing::Narrow,
            min_sharp_fraction: DEFAULT_MIN_SHARP_FRACTION,
            min_motion: DEFAULT_MIN_MOTION,
            detect_defects: false,
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
            ("min_sharp_fraction", self.min_sharp_fraction, 1.0, "between 0 and 1"),
        ];
        for (name, value, max, expected) in limits {
            if !(0.0..=max).contains(&value) {
                return Err(AlignError::Setting { name, value, expected });
            }
        }
        if !(MOTION_RANGE.0..=MOTION_RANGE.1).contains(&self.min_motion) {
            return Err(AlignError::Setting { name: "min_motion", value: self.min_motion, expected: "between 0.1 and 100 pixels" });
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
            AlignSettings { min_sharp_fraction: -0.1, ..AlignSettings::default() },
            AlignSettings { min_motion: 0.0, ..AlignSettings::default() },
        ];
        let names: Vec<&str> = bad
            .iter()
            .map(|s| match s.validate() {
                Err(AlignError::Setting { name, .. }) => name,
                other => panic!("{s:?}: {other:?}"),
            })
            .collect();
        assert_eq!(names, ["max_shift", "max_scale", "max_rotation", "min_score", "resolution", "resolution", "min_sharp_fraction", "min_motion"]);
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
        // Ordering and framing, as a preset and the project file spell them.
        let wide: AlignSettings = toml::from_str("framing = \"wide\"\nautomatic_order = false\nreverse_order = true\n").unwrap();
        assert_eq!(wide, AlignSettings { framing: Framing::Wide, automatic_order: false, reverse_order: true, ..AlignSettings::default() });
        assert_eq!(serde_json::to_value(&wide).unwrap()["framing"], "wide");
        assert!(toml::from_str::<AlignSettings>("framing = \"widest\"\n").is_err());
    }
}
