//! The adjustments of an output: what the user set, as data that the project stores and commands take.
//!
//! [`Adjustments`] holds the whole image's adjustments ([`Global`]) and a list of [`Mask`]s, each a region made of
//! [`Component`]s (shapes combined by adding, subtracting or intersecting) with adjustments of its own ([`Local`]).
//! Every field has a neutral default, so neutral adjustments leave the image as it is and a preset or a command can
//! name only some fields. [`Adjustments::validate`] checks what the types can't say: the ranges, which the schemas
//! mirror, and the sizes, so that hostile parameters can't make a render allocate without bound.
//!
//! Coordinates are fractions of the image: `[0, 0]` is its top left corner and `[1, 1]` its bottom right one. Sizes
//! (a radial mask's radii, a brush's size) are fractions of the image's long edge, so that a preview and the full
//! image look alike.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::EditError;

/// The most masks an output may have.
pub const MAX_MASKS: usize = 64;
/// The most components a mask may have.
pub const MAX_COMPONENTS: usize = 32;
/// The most strokes a brush component may have.
pub const MAX_STROKES: usize = 10_000;
/// The most points all the strokes of an output's masks may have together.
pub const MAX_POINTS: usize = 1_000_000;
/// The most colors a color-range component may sample.
pub const MAX_SAMPLES: usize = 16;
/// The most different depth ranges an output's masks may select (see [`Shape::Depth`]).
pub const MAX_DEPTH_RANGES: usize = 5;
/// The longest name a mask may have, in characters.
pub const MAX_NAME: usize = 64;
/// The most clicks an AI selection may take.
pub const MAX_CLICKS: usize = 64;
/// The longest description an AI selection may take, in characters.
pub const MAX_DESCRIPTION: usize = 200;
/// The largest grid a stored AI selection may claim (a hostile project can't make rendering allocate without bound).
pub const MAX_SELECTION_SIDE: u32 = 1024;
/// How far outside the image a coordinate may lie, in fractions of it: a gradient may start off the image.
const COORDINATE_LIMIT: f64 = 10.0;

/// An output's adjustments: the whole image's, then each mask's in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Adjustments {
    /// The adjustments of the whole image.
    pub global: Global,
    /// The masks, each a region with adjustments of its own, applied on top of the whole image's.
    pub masks: Vec<Mask>,
}

/// The adjustments of the whole image. Sliders run from -100 to 100 with 0 neutral unless their field says otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Global {
    /// White balance: warmer (positive) or cooler (negative), on a scale of mireds around the image as stacked.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub temperature: f64,
    /// White balance: magenta (positive) or green (negative).
    #[schemars(range(min = -100.0, max = 100.0))]
    pub tint: f64,
    /// Exposure in stops, from -5 to 5.
    #[schemars(range(min = -5.0, max = 5.0))]
    pub exposure: f64,
    /// Contrast of the tones around middle gray.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub contrast: f64,
    /// Brightness of the bright tones, recovering detail in them when negative.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub highlights: f64,
    /// Brightness of the dark tones, opening them up when positive.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub shadows: f64,
    /// The white point.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub whites: f64,
    /// The black point.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub blacks: f64,
    /// Contrast of fine detail and surface texture.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub texture: f64,
    /// Contrast of mid-sized detail: punch when positive, softness when negative.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub clarity: f64,
    /// Removes haze (positive) or adds it (negative).
    #[schemars(range(min = -100.0, max = 100.0))]
    pub dehaze: f64,
    /// Saturation that spares colors already saturated, and skin tones.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub vibrance: f64,
    /// Saturation of every color alike; -100 is black and white.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub saturation: f64,
    /// Sharpening, from 0 (none) to 150.
    #[schemars(range(min = 0.0, max = 150.0))]
    pub sharpening: f64,
    /// The size of the detail sharpening works on, in pixels, from 0.5 to 3.
    #[schemars(range(min = 0.5, max = 3.0))]
    pub sharpen_radius: f64,
    /// How much sharpening favors fine detail over edges, from 0 to 100.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub sharpen_detail: f64,
    /// Limits sharpening to edges, from 0 (everywhere) to 100 (only the strongest edges).
    #[schemars(range(min = 0.0, max = 100.0))]
    pub sharpen_masking: f64,
    /// Luminance noise reduction, from 0 (none) to 100.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub noise_reduction: f64,
    /// How much detail luminance noise reduction keeps, from 0 to 100.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub noise_detail: f64,
    /// Color noise reduction, from 0 (none) to 100.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub color_noise: f64,
}

impl Default for Global {
    fn default() -> Self {
        Global {
            temperature: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            texture: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            vibrance: 0.0,
            saturation: 0.0,
            sharpening: 0.0,
            sharpen_radius: 1.0,
            sharpen_detail: 25.0,
            sharpen_masking: 0.0,
            noise_reduction: 0.0,
            noise_detail: 50.0,
            color_noise: 0.0,
        }
    }
}

/// A mask's adjustments, applied where it selects. Sliders run from -100 to 100 with 0 neutral unless their field says
/// otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Local {
    /// Warmer (positive) or cooler (negative).
    #[schemars(range(min = -100.0, max = 100.0))]
    pub temperature: f64,
    /// Magenta (positive) or green (negative).
    #[schemars(range(min = -100.0, max = 100.0))]
    pub tint: f64,
    /// Exposure in stops, from -5 to 5.
    #[schemars(range(min = -5.0, max = 5.0))]
    pub exposure: f64,
    /// Contrast.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub contrast: f64,
    /// Brightness of the bright tones.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub highlights: f64,
    /// Brightness of the dark tones.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub shadows: f64,
    /// The white point.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub whites: f64,
    /// The black point.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub blacks: f64,
    /// Contrast of fine detail and surface texture.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub texture: f64,
    /// Contrast of mid-sized detail.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub clarity: f64,
    /// Removes haze (positive) or adds it (negative).
    #[schemars(range(min = -100.0, max = 100.0))]
    pub dehaze: f64,
    /// Saturation; -100 is black and white.
    #[schemars(range(min = -100.0, max = 100.0))]
    pub saturation: f64,
    /// Sharpens (positive) or softens (negative).
    #[schemars(range(min = -100.0, max = 100.0))]
    pub sharpness: f64,
    /// Reduces noise (positive) or keeps more grain (negative).
    #[schemars(range(min = -100.0, max = 100.0))]
    pub noise: f64,
    /// How strongly the mask applies its adjustments, from 0 (not at all) to 100.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub amount: f64,
}

impl Default for Local {
    fn default() -> Self {
        Local {
            temperature: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            texture: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            saturation: 0.0,
            sharpness: 0.0,
            noise: 0.0,
            amount: 100.0,
        }
    }
}

/// A region of the image with adjustments of its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Mask {
    /// The mask's id, unique among the output's masks and never reused: commands name a mask by it.
    pub id: u32,
    /// Its name, at most 64 characters.
    pub name: String,
    /// Whether its adjustments apply. A hidden mask is kept but changes nothing.
    pub visible: bool,
    /// Whether it selects what its components leave out instead.
    pub invert: bool,
    /// How closely its edges follow the image's own, from 0 (as drawn) to 100.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub refine: f64,
    /// Its shapes, combined in order: the first one's region, then each next one added, subtracted or intersected.
    pub components: Vec<Component>,
    /// The adjustments it applies where it selects.
    pub adjust: Local,
}

impl Default for Mask {
    fn default() -> Self {
        Mask { id: 0, name: String::new(), visible: true, invert: false, refine: 0.0, components: Vec::new(), adjust: Local::default() }
    }
}

/// How a component combines with the components before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    /// Adds its region (the larger of the two selections).
    #[default]
    Add,
    /// Takes its region away.
    Subtract,
    /// Keeps only what both select.
    Intersect,
}

/// One shape of a mask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// How it combines with the components before it.
    #[serde(default)]
    pub op: Op,
    /// Whether it selects what its shape leaves out instead.
    #[serde(default)]
    pub invert: bool,
    /// The shape.
    pub shape: Shape,
}

/// What a component selects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    /// The whole image.
    Whole,
    /// A linear gradient: everything at `start` and before it, fading to nothing at `end`.
    Linear {
        /// Where the gradient selects fully, as fractions of the image's width and height.
        start: [f64; 2],
        /// Where it selects nothing.
        end: [f64; 2],
    },
    /// An ellipse, fading out at its edge.
    Radial {
        /// Its center, as fractions of the image's width and height.
        center: [f64; 2],
        /// Its horizontal radius before rotation, as a fraction of the image's long edge.
        #[schemars(range(min = 0.0, max = 10.0))]
        radius_x: f64,
        /// Its vertical radius before rotation, as a fraction of the image's long edge.
        #[schemars(range(min = 0.0, max = 10.0))]
        radius_y: f64,
        /// Its rotation in degrees, clockwise.
        #[schemars(range(min = -360.0, max = 360.0))]
        angle: f64,
        /// How softly it fades out, from 0 (a hard edge) to 100.
        #[schemars(range(min = 0.0, max = 100.0))]
        feather: f64,
    },
    /// Painted strokes.
    Brush {
        /// The strokes, in the order they were painted.
        strokes: Vec<Stroke>,
    },
    /// The pixels in a range of brightness, measured in stops: 0 is 8 stops below white (or darker), 1 is white, and
    /// each eighth is a stop.
    Luminance {
        /// The darkest brightness it selects fully, from 0 to 1.
        #[schemars(range(min = 0.0, max = 1.0))]
        low: f64,
        /// The brightest it selects fully, from 0 to 1; at 1, everything brighter than white too.
        #[schemars(range(min = 0.0, max = 1.0))]
        high: f64,
        /// How far below `low` the selection fades out, from 0 to 1.
        #[schemars(range(min = 0.0, max = 1.0))]
        low_feather: f64,
        /// How far above `high` it fades out, from 0 to 1.
        #[schemars(range(min = 0.0, max = 1.0))]
        high_feather: f64,
    },
    /// The pixels close to sampled colors.
    Color {
        /// The colors it selects, in `OkLab` (lightness, then the two color axes), as `adjust.sampleColor` gives them.
        samples: Vec<[f64; 3]>,
        /// How close a color must be to a sample, from 0 (loosely) to 100 (only very close colors).
        #[schemars(range(min = 0.0, max = 100.0))]
        refine: f64,
    },
    /// Sharp areas: where the stack has fine detail, which some frame had in focus. Found without a model; it works
    /// for a subject in front of a blurred background, not on a surface in focus (`object` selects a subject there).
    Subject,
    /// Soft areas: where the stack has no fine detail, which no frame had in focus.
    Background,
    /// An object, selected by the AI model (SAM 3) from clicks on it, and kept: rendering needs no model.
    Object {
        /// The clicks: on the object (`include`) or on what to leave out of it, as fractions of the image.
        clicks: Vec<Click>,
        /// What the model selected for them; none until it has (the shape selects nothing until then).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selection: Option<Selection>,
    },
    /// Everything a description names ("insect", "the red flower"), selected by the AI model (SAM 3), and kept.
    Described {
        /// The description, at most 200 characters.
        text: String,
        /// What the model selected for it; none until it has.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selection: Option<Selection>,
    },
    /// A range of depth: the parts of the subject in focus in a range of the stack's frames, counted in the order they
    /// are stacked: 0 is the first frame, 1 the last (the nearest and the farthest, or the other way round, as the
    /// stack was taken).
    Depth {
        /// Where the range starts, from 0 (the first frame) to 1 (the last).
        #[schemars(range(min = 0.0, max = 1.0))]
        from: f64,
        /// Where it ends, at or after `from`.
        #[schemars(range(min = 0.0, max = 1.0))]
        to: f64,
        /// How far beyond the range the selection fades out, from 0 to 1.
        #[schemars(range(min = 0.0, max = 1.0))]
        feather: f64,
    },
}

/// One click of an AI selection.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Click {
    /// Where, as fractions of the image's width and height.
    pub at: [f64; 2],
    /// Whether the click is on what to select (`true`) or on what to leave out.
    #[serde(default = "yes")]
    pub include: bool,
}

fn yes() -> bool {
    true
}

/// A selection the AI model made, kept with its mask: the model's logits over the image stretched to a square,
/// `side` by `side`, rounded to an eighth, deflated and base64-encoded (LightCraft's format, which its pipeline
/// renders: each point's coverage is the logit's sigmoid, so edges stay smooth at any size).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    /// The grid's side, at most 1024.
    #[schemars(range(min = 1, max = 1024))]
    pub side: u32,
    /// The logits, quantized, deflated and base64-encoded.
    pub data: String,
}

/// One brush stroke.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Stroke {
    /// The pointer's path, as fractions of the image's width and height.
    pub points: Vec<[f64; 2]>,
    /// The brush's radius, as a fraction of the image's long edge, above 0 and at most 1.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub size: f64,
    /// How softly its edge fades, from 0 (hard) to 100.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub feather: f64,
    /// How much each dab adds, from 0 to 100.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub flow: f64,
    /// Whether it erases what earlier strokes painted.
    pub erase: bool,
}

impl Default for Stroke {
    fn default() -> Self {
        Stroke { points: Vec::new(), size: 0.03, feather: 50.0, flow: 100.0, erase: false }
    }
}

impl Adjustments {
    /// Whether these adjustments leave the image as it is: every whole-image setting neutral and no visible mask
    /// with an adjustment that changes something.
    pub fn is_neutral(&self) -> bool {
        self.global.is_neutral() && self.masks.iter().all(|m| !m.visible || m.components.is_empty() || m.adjust.is_neutral())
    }

    /// The mask with this id.
    pub fn mask(&self, id: u32) -> Option<&Mask> {
        self.masks.iter().find(|m| m.id == id)
    }

    /// The mask with this id, to change.
    pub fn mask_mut(&mut self, id: u32) -> Option<&mut Mask> {
        self.masks.iter_mut().find(|m| m.id == id)
    }

    /// An id no mask has had: one more than the largest.
    pub fn next_mask_id(&self) -> u32 {
        self.masks.iter().map(|m| m.id.saturating_add(1)).max().unwrap_or(1).max(1)
    }

    /// Whether some visible mask selects from the subject (a subject, background or depth component).
    pub fn uses_subject(&self) -> bool {
        self.shapes().any(|s| matches!(s, Shape::Subject | Shape::Background | Shape::Depth { .. }))
    }

    /// Whether some visible mask needs the stack's depth map (a depth component).
    pub fn needs_depth(&self) -> bool {
        self.shapes().any(|s| matches!(s, Shape::Depth { .. }))
    }

    /// The shapes of the visible masks.
    fn shapes(&self) -> impl Iterator<Item = &Shape> {
        self.masks.iter().filter(|m| m.visible).flat_map(|m| m.components.iter().map(|c| &c.shape))
    }

    /// The different depth ranges the visible masks select, in the order they first appear.
    pub(crate) fn depth_ranges(&self) -> Vec<[f64; 3]> {
        let mut ranges: Vec<[f64; 3]> = Vec::new();
        for shape in self.shapes() {
            if let Shape::Depth { from, to, feather } = *shape {
                let range = [from, to, feather];
                if !ranges.contains(&range) {
                    ranges.push(range);
                }
            }
        }
        ranges
    }

    /// Checks every range and size.
    pub fn validate(&self) -> Result<(), EditError> {
        self.global.validate()?;
        if self.masks.len() > MAX_MASKS {
            return Err(EditError::invalid(format!("an output has at most {MAX_MASKS} masks, not {}", self.masks.len())));
        }
        let mut ids: Vec<u32> = self.masks.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|w| w.first() == w.get(1)) {
            return Err(EditError::invalid("two masks have the same id"));
        }
        let mut points = 0usize;
        for mask in &self.masks {
            mask.validate(&mut points)?;
        }
        let ranges = self.depth_ranges().len();
        if ranges > MAX_DEPTH_RANGES {
            return Err(EditError::invalid(format!("the masks select at most {MAX_DEPTH_RANGES} different depth ranges, not {ranges}")));
        }
        Ok(())
    }
}

/// Checks that `value` is finite and within `[min, max]`.
fn check(name: &str, value: f64, min: f64, max: f64) -> Result<(), EditError> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(EditError::invalid(format!("{name} must be from {min} to {max}, not {value}")))
    }
}

fn check_point(name: &str, p: [f64; 2]) -> Result<(), EditError> {
    for v in p {
        check(name, v, -COORDINATE_LIMIT, COORDINATE_LIMIT)?;
    }
    Ok(())
}

/// The neutral value of every slider: what [`Global::default`] and [`Local::default`] hold.
#[allow(clippy::float_cmp)] // Neutral means exactly the default.
fn all_equal(a: &[f64], b: &[f64]) -> bool {
    a == b
}

impl Global {
    fn sliders(&self) -> [(&'static str, f64, f64, f64); 20] {
        [
            ("temperature", self.temperature, -100.0, 100.0),
            ("tint", self.tint, -100.0, 100.0),
            ("exposure", self.exposure, -5.0, 5.0),
            ("contrast", self.contrast, -100.0, 100.0),
            ("highlights", self.highlights, -100.0, 100.0),
            ("shadows", self.shadows, -100.0, 100.0),
            ("whites", self.whites, -100.0, 100.0),
            ("blacks", self.blacks, -100.0, 100.0),
            ("texture", self.texture, -100.0, 100.0),
            ("clarity", self.clarity, -100.0, 100.0),
            ("dehaze", self.dehaze, -100.0, 100.0),
            ("vibrance", self.vibrance, -100.0, 100.0),
            ("saturation", self.saturation, -100.0, 100.0),
            ("sharpening", self.sharpening, 0.0, 150.0),
            ("sharpen_radius", self.sharpen_radius, 0.5, 3.0),
            ("sharpen_detail", self.sharpen_detail, 0.0, 100.0),
            ("sharpen_masking", self.sharpen_masking, 0.0, 100.0),
            ("noise_reduction", self.noise_reduction, 0.0, 100.0),
            ("noise_detail", self.noise_detail, 0.0, 100.0),
            ("color_noise", self.color_noise, 0.0, 100.0),
        ]
    }

    /// Checks every slider's range.
    pub fn validate(&self) -> Result<(), EditError> {
        self.sliders().into_iter().try_for_each(|(name, v, min, max)| check(name, v, min, max))
    }

    /// Whether these settings change nothing. Sharpening's radius, detail and masking and noise reduction's detail
    /// change nothing without the amounts they qualify.
    pub fn is_neutral(&self) -> bool {
        let neutral = Global { sharpen_radius: 1.0, sharpen_detail: 25.0, sharpen_masking: 0.0, noise_detail: 50.0, ..*self };
        let values = neutral.sliders().map(|s| s.1);
        let defaults = Global::default().sliders().map(|s| s.1);
        all_equal(&values, &defaults)
    }
}

impl Local {
    fn sliders(&self) -> [(&'static str, f64, f64, f64); 14] {
        [
            ("temperature", self.temperature, -100.0, 100.0),
            ("tint", self.tint, -100.0, 100.0),
            ("exposure", self.exposure, -5.0, 5.0),
            ("contrast", self.contrast, -100.0, 100.0),
            ("highlights", self.highlights, -100.0, 100.0),
            ("shadows", self.shadows, -100.0, 100.0),
            ("whites", self.whites, -100.0, 100.0),
            ("blacks", self.blacks, -100.0, 100.0),
            ("texture", self.texture, -100.0, 100.0),
            ("clarity", self.clarity, -100.0, 100.0),
            ("dehaze", self.dehaze, -100.0, 100.0),
            ("saturation", self.saturation, -100.0, 100.0),
            ("sharpness", self.sharpness, -100.0, 100.0),
            ("noise", self.noise, -100.0, 100.0),
        ]
    }

    /// Checks every slider's range.
    pub fn validate(&self) -> Result<(), EditError> {
        self.sliders().into_iter().try_for_each(|(name, v, min, max)| check(name, v, min, max))?;
        check("amount", self.amount, 0.0, 100.0)
    }

    /// Whether these adjustments change nothing, wherever they apply.
    pub fn is_neutral(&self) -> bool {
        let values = self.sliders().map(|s| s.1);
        all_equal(&values, &[0.0; 14]) || self.amount <= 0.0
    }
}

impl Mask {
    fn validate(&self, points: &mut usize) -> Result<(), EditError> {
        if self.name.chars().count() > MAX_NAME {
            return Err(EditError::invalid(format!("a mask's name has at most {MAX_NAME} characters")));
        }
        check("refine", self.refine, 0.0, 100.0)?;
        self.adjust.validate()?;
        if self.components.len() > MAX_COMPONENTS {
            return Err(EditError::invalid(format!("a mask has at most {MAX_COMPONENTS} components, not {}", self.components.len())));
        }
        self.components.iter().try_for_each(|c| c.shape.validate(points))
    }
}

impl Shape {
    fn validate(&self, points: &mut usize) -> Result<(), EditError> {
        match self {
            Shape::Whole | Shape::Subject | Shape::Background => Ok(()),
            Shape::Object { clicks, selection } => {
                if clicks.len() > MAX_CLICKS {
                    return Err(EditError::invalid(format!("an AI selection takes at most {MAX_CLICKS} clicks, not {}", clicks.len())));
                }
                clicks.iter().try_for_each(|c| check_point("a click", c.at))?;
                selection.as_ref().map_or(Ok(()), Selection::validate)
            }
            Shape::Described { text, selection } => {
                if text.trim().is_empty() || text.chars().count() > MAX_DESCRIPTION || text.chars().any(char::is_control) {
                    return Err(EditError::invalid(format!("a description has 1 to {MAX_DESCRIPTION} characters, none of them control characters")));
                }
                selection.as_ref().map_or(Ok(()), Selection::validate)
            }
            Shape::Linear { start, end } => {
                check_point("start", *start)?;
                check_point("end", *end)
            }
            Shape::Radial { center, radius_x, radius_y, angle, feather } => {
                check_point("center", *center)?;
                check("radius_x", *radius_x, 0.0, 10.0)?;
                check("radius_y", *radius_y, 0.0, 10.0)?;
                check("angle", *angle, -360.0, 360.0)?;
                check("feather", *feather, 0.0, 100.0)
            }
            Shape::Brush { strokes } => {
                if strokes.len() > MAX_STROKES {
                    return Err(EditError::invalid(format!("a brush has at most {MAX_STROKES} strokes, not {}", strokes.len())));
                }
                strokes.iter().try_for_each(|s| s.validate(points))
            }
            Shape::Luminance { low, high, low_feather, high_feather } => {
                check("low", *low, 0.0, 1.0)?;
                check("high", *high, 0.0, 1.0)?;
                check("low_feather", *low_feather, 0.0, 1.0)?;
                check("high_feather", *high_feather, 0.0, 1.0)?;
                if low > high {
                    return Err(EditError::invalid("a luminance range's low end must not be above its high end"));
                }
                Ok(())
            }
            Shape::Color { samples, refine } => {
                if samples.len() > MAX_SAMPLES {
                    return Err(EditError::invalid(format!("a color range has at most {MAX_SAMPLES} samples, not {}", samples.len())));
                }
                for s in samples {
                    for v in s {
                        check("a color sample", *v, -2.0, 2.0)?;
                    }
                }
                check("refine", *refine, 0.0, 100.0)
            }
            Shape::Depth { from, to, feather } => {
                check("from", *from, 0.0, 1.0)?;
                check("to", *to, 0.0, 1.0)?;
                check("feather", *feather, 0.0, 1.0)?;
                if from > to {
                    return Err(EditError::invalid("a depth range must not end before it starts"));
                }
                Ok(())
            }
        }
    }
}

impl Selection {
    /// A selection from `side` by `side` logits over the image stretched to a square (row by row; positive inside):
    /// what the model gives, and what tests make.
    pub fn from_logits(side: usize, logits: &[f32]) -> Selection {
        let seg = lightcraft_develop::SegMask::from_logits(side, logits);
        Selection { side: seg.side, data: seg.data }
    }

    /// Checks the grid's size and that the data is no larger than its grid could deflate to (base64 of at most one byte
    /// a cell, with room for the deflate stream's overhead).
    fn validate(&self) -> Result<(), EditError> {
        if !(1..=MAX_SELECTION_SIDE).contains(&self.side) {
            return Err(EditError::invalid(format!("a selection's grid is 1 to {MAX_SELECTION_SIDE} cells a side, not {}", self.side)));
        }
        let cells = u64::from(self.side) * u64::from(self.side);
        if self.data.len() as u64 > cells * 2 + 1024 {
            return Err(EditError::invalid("a selection's data is larger than its grid"));
        }
        Ok(())
    }
}

impl Stroke {
    fn validate(&self, points: &mut usize) -> Result<(), EditError> {
        if !(self.size.is_finite() && self.size > 0.0 && self.size <= 1.0) {
            return Err(EditError::invalid(format!("a stroke's size must be above 0 and at most 1, not {}", self.size)));
        }
        check("feather", self.feather, 0.0, 100.0)?;
        check("flow", self.flow, 0.0, 100.0)?;
        *points = points.saturating_add(self.points.len());
        if *points > MAX_POINTS {
            return Err(EditError::invalid(format!("the strokes of an output's masks have at most {MAX_POINTS} points")));
        }
        self.points.iter().try_for_each(|p| check_point("a stroke's point", *p))
    }
}
