//! Stacking: the stacking settings, a preference group of the project (`stack.get`, `stack.set`, `stack.reset`), the
//! weighted average, the pyramid method and the depth-map method of the aligned frames as new outputs
//! (`stack.average`, `stack.pyramid`, `stack.depthMap`, jobs), and a quarter-scale preview of any of them
//! (`stack.preview`, a preview job).
//!
//! Every job stacks the enabled frames of the stored alignment run, in the run's processing order, on the canvas of the
//! framing the alignment settings choose, as `align.info` shows it; frames whose alignment was rejected are left out
//! (their transform is only their reference's). Every enabled frame must be in the run: a frame added or enabled
//! since is an error that asks for `align.run`. Each frame is read and developed as alignment read it, with the
//! stack's defect mask inpainted (`align::Plan`), its brightness matched to the master's with the gain alignment
//! measured, and handed to florastack-stack's stacker, then dropped: memory holds one frame and the accumulators,
//! whatever the depth of the stack.
//!
//! - **The full stack** (`stack.average`, `stack.pyramid`, `stack.depthMap`) warps each frame onto the canvas a tile at
//!   a time, as the stacker reads it ([`Warped`]: florastack-align's `warp_to_canvas`, bicubic, with the frame's
//!   coverage, so that in wide framing a frame adds nothing where it has no pixels), and RAW frames get the settings'
//!   demosaic. Its commit adds the output (`Edit::AddOutput`, one undo step): the stacked image in the cache, linear
//!   `f32` TIFF, with an alpha channel when some pixels are covered by no frame (only wide framing has them), and a
//!   small JPEG preview for lists, with the method's parameters and the frames it was made from.
//! - **Range** (the pyramid method): the stack comes out unclipped and is brought into `[0, 1]` as its `tone` setting
//!   says (florastack-stack's `compress_range`). When that changes any pixel, the result before it is kept as the
//!   output's `extended` map, which `output.save` writes for "retain extended range" files.
//! - **Two passes** (the depth-map method): every frame is read twice, once to measure its focus and once to
//!   composite, with the depth map made between them from the focus statistics. The output keeps the depth map and the
//!   confidence map as its `depth` and `confidence` maps, 16-bit (`output.save` with `map`).
//! - **The preview** (`stack.preview`) reduces each frame to a quarter of its size as soon as it is read
//!   (florastack-align's `thumbnail`, which the filmstrip uses too: block averages, then Lanczos3) and warps the small
//!   copy onto the quarter-size canvas as the full stack warps the frame (tile by tile, bicubic, with its feathered
//!   coverage), with the settings scaled to match (`Method::reduced`); RAW frames get the fast demosaic. It
//!   runs in the preview lane, beside a full stack, and a newer call cancels an older one still running, so that a
//!   preview follows every change of a setting. Its result is a lossless TIFF in `previews/`, which nothing in the
//!   project refers to: no undo step, and the file goes when the project is next closed or opened.
//!
//! Slabbing (`stack.slabbed`, `slab.rs`) stacks with the same plan, a method for the slabs and one for the final stack.

use std::borrow::Cow;
use std::ops::Range;

use florastack_align::{AlignError, Canvas, DEFAULT_FEATHER, Framing, Status, thumbnail, warp_to_canvas};
use florastack_core::{Affine, AnyImage, Image, Monitor, Rect};
use florastack_gpu::ops::WarpGeometry;
use florastack_gpu::{Frame as GpuFrame, Gpu, GpuError, GpuStacked, GpuStacker, GpuTiling, Method as GpuMethod};
use florastack_project::{Area, CacheDir, CacheRef, Edit, FrameId, Group, Input, Maps, NewOutput, OutputId, Preferences, Table};
use florastack_raw::Quality;
use florastack_stack::{
    AVERAGE, AverageSettings, DEPTH_MAP, Decisions, DepthMap, DepthSettings, FocusStats, FrameProgress, FrameSource, Gained, MAX_RADIUS, Method,
    PREVIEW_FACTOR, PYRAMID, Patch, PyramidSettings, StackError, StackSettings, StackShape, Stacked, Stacker, ThresholdMode, Tiling, Tone,
    compress_range, out_of_range,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

use super::align::Plan;
use super::check_name;
use crate::compute::{Backend, Compute};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobContext, JobPlan, Lane, commit};
use crate::project::{
    align_settings, confidence_map, depth_map, develop_record, extended_map, stack_settings, stored_alignment, write_image, write_image_in,
    write_preview,
};
use crate::registry::{CommandSpec, Meta, NoParams, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("stack.get", "Stacking settings", "The stacking methods' settings.")
                .help(
                    "Every setting, with the keys the preferences don't set at their defaults: for the weighted average \
                     (`average`), the radius sharpness is measured over, the smoothing of the blend and the power that \
                     sets how strongly the sharpest frame wins; for the pyramid method (`pyramid`), the number of \
                     levels, the grit suppression, the decisions (luminance or channels), the range limit, the tone \
                     compression and the coarse limit; for the depth-map method (`depth_map`), the estimation radius, \
                     the smoothing radius and the contrast threshold: how it is chosen (`threshold`: auto, level or \
                     percentile), and its `level` and `percentile`.",
                )
                .requires(Requires::Project),
            get,
        ),
        CommandSpec::instant(
            Meta::new("stack.set", "Set stacking settings", "Changes some of the stacking methods' settings.")
                .help(
                    "Only the keys given change, as when a preset is applied: `{\"average\": {\"radius\": 2}}` changes \
                     the weighted average's radius and leaves the rest as it is. The result is checked whole, and a \
                     misspelled key is an error. The settings apply to the next stack and preview.",
                )
                .undoable()
                .examples(&[
                    r#"{ "average": { "radius": 2.0 } }"#,
                    r#"{ "pyramid": { "grit": 0.25, "tone": "clip" } }"#,
                    r#"{ "depth_map": { "threshold": "percentile", "percentile": 40 } }"#,
                ]),
            set,
        ),
        CommandSpec::instant(
            Meta::new("stack.reset", "Reset stacking settings", "Puts every stacking setting back at its default.").undoable(),
            reset,
        ),
    ]
    .into_iter()
    .chain(stacking())
    .collect()
}

/// The stacking jobs: one per method, and the preview.
fn stacking() -> Vec<CommandSpec> {
    vec![
        CommandSpec::job(
            Meta::new("stack.average", "Stack by weighted average", "Stacks the aligned frames by weighted average into a new output.")
                .help(
                    "Blends every frame into every pixel, each weighted by its sharpness there: the Laplacian's \
                     energy, averaged over `radius` pixels and raised to `power`, the weights smoothed over \
                     `smoothing` pixels. Fast, and a good fallback for simple subjects; where near and far detail \
                     overlap, the pyramid method is sharper. Uses the project's settings (`stack.get`), or the ones \
                     given for this stack only, and the stored alignment, framed as the alignment settings say \
                     (`align.info`): align the stack first with `align.run`, and again after adding or enabling \
                     frames. Frames whose alignment was rejected are left out. Pixels that no frame covers, which only \
                     wide framing has, are transparent. Runs as a job; the output is added in one undo step when it \
                     ends, or not at all when it is cancelled.",
                )
                .undoable()
                .examples(&["{}", r#"{ "radius": 2.0, "smoothing": 2.0, "name": "Average, smooth" }"#]),
            Lane::Heavy,
            average,
        ),
        CommandSpec::job(
            Meta::new("stack.pyramid", "Stack by pyramid", "Stacks the aligned frames by the pyramid method into a new output.")
                .help(
                    "Decomposes every frame into a Laplacian pyramid and keeps, at every level and position, the \
                     coefficients of the frame with the most local energy: the finest detail of every frame survives \
                     where near and far structure overlap (hair, bristles, antennae), without the veils the weighted \
                     average leaves around out-of-focus edges. The finest level is blended to keep noise down \
                     (`grit`), choices are made on luminance or on each channel (`decisions`), every pixel is held \
                     within the range the frames span there (`rangeLimit`), and the result is brought into [0, 1] as \
                     `tone` says; when that changes any pixel, the result before it is kept with the output for \
                     \"retain extended range\" files (`output.save` with `extendedRange` and 32-bit float). \
                     `coarseLimit` (off by default) keeps the coarse levels' choices within that many frames of the \
                     frame in focus there. Uses the project's settings (`stack.get`), or the ones given for this stack \
                     only, and the stored alignment, as `stack.average` does. Runs as a job; the output is added in \
                     one undo step when it ends, or not at all when it is cancelled.",
                )
                .undoable()
                .examples(&["{}", r#"{ "grit": 0.25, "name": "Pyramid, more detail" }"#, r#"{ "decisions": "channels", "tone": "keep" }"#]),
            Lane::Heavy,
            pyramid,
        ),
        CommandSpec::job(
            Meta::new("stack.depthMap", "Stack by depth map", "Stacks the aligned frames by the depth-map method into a new output.")
                .help(
                    "Finds, for every pixel, the frame in which it is sharpest, smooths that depth map with an \
                     edge-aware filter and takes each pixel from the frames at its depth, blending the two on either \
                     side: every pixel's color and contrast are its frames', for smooth tones and accurate color. Where \
                     the confidence (how clearly the sharpest frame stands out) is below the threshold, as in an \
                     out-of-focus background or on a smooth surface, the depth is filled from the confident pixels \
                     around instead. `radius` is the estimation radius the focus is measured over, `smoothing` the \
                     reach of the depth map's smoothing, and `threshold` how the contrast threshold is chosen: `auto` \
                     (from the statistics: the pixels whose depth follows noise, or else the 10th percentile), `level` \
                     (the absolute `level`) or `percentile` (`percentile` percent of the pixels). Reads every frame \
                     twice. The output keeps the depth map (the frame index scaled to 16 bits, 0 for the first frame) \
                     and the confidence map (0.5 at the threshold) as its `depth` and `confidence` maps, which \
                     `output.save` writes with `map`. Where near and far detail overlap \
                     (hair, bristles), the pyramid method is sharper. Uses the project's settings (`stack.get`), or the \
                     ones given for this stack only, and the stored alignment, as `stack.average` does. Runs as a job; \
                     the output is added in one undo step when it ends, or not at all when it is cancelled.",
                )
                .undoable()
                .examples(&[
                    "{}",
                    r#"{ "threshold": "percentile", "percentile": 50, "name": "Depth map, more filled" }"#,
                    r#"{ "radius": 3.0, "smoothing": 16.0 }"#,
                ]),
            Lane::Heavy,
            depth,
        ),
        CommandSpec::job(
            Meta::new("stack.preview", "Preview stack", "Stacks the aligned frames at a quarter of their size, as a quick preview.")
                .help(
                    "The weighted average of `stack.average`, or with `method` \"pyramid\" or \"depth-map\" the \
                     method of `stack.pyramid` or `stack.depthMap`, on frames reduced to a quarter of their size on \
                     each side with the settings scaled to match, so that it shows in seconds what the full stack will \
                     look like. Takes the same settings as that method's command. The result is a linear 32-bit float \
                     TIFF in the project's `previews/` folder, as a path, kept until the project is closed: no output, \
                     no undo step. Runs beside other jobs as a preview; a newer call replaces an older one still \
                     running, so call it after every change of a setting.",
                )
                .requires(Requires::Project)
                .supersede()
                .examples(&[
                    "{}",
                    r#"{ "smoothing": 3.0 }"#,
                    r#"{ "method": "pyramid", "grit": 0.25 }"#,
                    r#"{ "method": "depth-map", "threshold": "level", "level": 0.002 }"#,
                ]),
            Lane::Preview,
            preview,
        ),
    ]
}

/// The stacking settings.
#[derive(Debug, Serialize, JsonSchema)]
struct Settings {
    /// Every setting, with the defaults filled in.
    settings: StackSettings,
}

fn get(session: &Session, _: NoParams) -> Result<Settings, CommandError> {
    Ok(Settings { settings: stack_settings(session.project()?)? })
}

/// Some stacking settings: any of [`StackSettings`]' keys, checked as such.
#[derive(Debug)]
struct StackPatch(Table);

impl<'de> Deserialize<'de> for StackPatch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<StackPatch, D::Error> {
        let table = Table::deserialize(deserializer)?;
        // Every key, and the type of every value, is checked by the settings type itself.
        StackSettings::deserialize(serde_json::Value::Object(table.clone())).map_err(serde::de::Error::custom)?;
        Ok(StackPatch(table))
    }
}

impl JsonSchema for StackPatch {
    fn schema_name() -> Cow<'static, str> {
        StackSettings::schema_name()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        StackSettings::json_schema(generator)
    }
}

impl Params for StackPatch {}

/// Sets the stacking preferences to `table`, after checking the settings it makes; no step when nothing changes.
fn set_table(session: &mut Session, table: Table) -> Result<Settings, CommandError> {
    let mut preferences = Preferences::new();
    preferences.set(Group::Stack, &table)?;
    let settings: StackSettings = preferences.get(Group::Stack)?;
    settings.validate().map_err(|e| session.invalid("settings", e.to_string()))?;
    if session.project()?.preferences.group(Group::Stack) != preferences.group(Group::Stack) {
        session.apply(Edit::SetPreferences { group: Group::Stack, table: preferences.group(Group::Stack).clone() })?;
    }
    Ok(Settings { settings })
}

fn set(session: &mut Session, patch: StackPatch) -> Result<Settings, CommandError> {
    let mut top = Preferences::new();
    top.set(Group::Stack, &patch.0)?;
    let mut merged = session.project()?.preferences.only(&[Group::Stack]);
    merged.layer(&top);
    set_table(session, merged.group(Group::Stack).clone())
}

fn reset(session: &mut Session, _: NoParams) -> Result<Settings, CommandError> {
    set_table(session, Table::new())
}

/// The weighted average's settings for one stack, on top of the project's: each one left out is the project's.
#[derive(Debug, Default)]
struct AverageParams {
    radius: Option<f64>,
    smoothing: Option<f64>,
    power: Option<f64>,
}

impl AverageParams {
    /// The settings these parameters make on top of `base`.
    fn settings(&self, base: &AverageSettings) -> AverageSettings {
        AverageSettings {
            radius: self.radius.unwrap_or(base.radius),
            smoothing: self.smoothing.unwrap_or(base.smoothing),
            power: self.power.unwrap_or(base.power),
        }
    }

    fn given(&self) -> Option<&'static str> {
        [("radius", self.radius.is_some()), ("smoothing", self.smoothing.is_some()), ("power", self.power.is_some())]
            .into_iter()
            .find_map(|(name, given)| given.then_some(name))
    }

    fn check(&self) -> Result<(), Invalid> {
        for (field, value) in [("radius", self.radius), ("smoothing", self.smoothing)] {
            if let Some(v) = value
                && !(v.is_finite() && (0.0..=MAX_RADIUS).contains(&v))
            {
                return Err(Invalid::new(field, "must be from 0 to 64 pixels"));
            }
        }
        self.settings(&AverageSettings::default()).validate().map_err(|e| Invalid::new("power", e.to_string()))
    }
}

/// The pyramid method's settings for one stack, on top of the project's: each one left out is the project's.
#[derive(Debug, Default)]
struct PyramidOverrides {
    levels: Option<u32>,
    grit: Option<f64>,
    decisions: Option<Decisions>,
    range_limit: Option<f64>,
    tone: Option<Tone>,
    #[allow(clippy::option_option)] // Missing (the project's), `null` (off for this stack) or a limit.
    coarse_limit: Option<Option<u32>>,
}

impl PyramidOverrides {
    fn settings(&self, base: &PyramidSettings) -> PyramidSettings {
        PyramidSettings {
            levels: self.levels.unwrap_or(base.levels),
            grit: self.grit.unwrap_or(base.grit),
            decisions: self.decisions.unwrap_or(base.decisions),
            range_limit: self.range_limit.unwrap_or(base.range_limit),
            tone: self.tone.unwrap_or(base.tone),
            coarse_limit: self.coarse_limit.unwrap_or(base.coarse_limit),
        }
    }

    fn given(&self) -> Option<&'static str> {
        [
            ("levels", self.levels.is_some()),
            ("grit", self.grit.is_some()),
            ("decisions", self.decisions.is_some()),
            ("rangeLimit", self.range_limit.is_some()),
            ("tone", self.tone.is_some()),
            ("coarseLimit", self.coarse_limit.is_some()),
        ]
        .into_iter()
        .find_map(|(name, given)| given.then_some(name))
    }

    fn check(&self) -> Result<(), Invalid> {
        let settings = self.settings(&PyramidSettings::default());
        settings.validate().map_err(|e| match e {
            StackError::Setting { name, .. } => Invalid::new(
                match name {
                    "range_limit" => "rangeLimit",
                    "coarse_limit" => "coarseLimit",
                    other => other,
                },
                e.to_string(),
            ),
            other => Invalid::new("settings", other.to_string()),
        })
    }
}

/// The depth-map method's settings for one stack, on top of the project's: each one left out is the project's.
#[derive(Debug, Default)]
pub(super) struct DepthOverrides {
    pub(super) radius: Option<f64>,
    pub(super) smoothing: Option<f64>,
    pub(super) threshold: Option<ThresholdMode>,
    pub(super) level: Option<f64>,
    pub(super) percentile: Option<f64>,
}

impl DepthOverrides {
    pub(super) fn settings(&self, base: &DepthSettings) -> DepthSettings {
        DepthSettings {
            radius: self.radius.unwrap_or(base.radius),
            smoothing: self.smoothing.unwrap_or(base.smoothing),
            threshold: self.threshold.unwrap_or(base.threshold),
            level: self.level.unwrap_or(base.level),
            percentile: self.percentile.unwrap_or(base.percentile),
        }
    }

    /// The first threshold setting given, if any.
    fn given_threshold(&self) -> Option<&'static str> {
        [("threshold", self.threshold.is_some()), ("level", self.level.is_some()), ("percentile", self.percentile.is_some())]
            .into_iter()
            .find_map(|(name, given)| given.then_some(name))
    }

    pub(super) fn check(&self) -> Result<(), Invalid> {
        self.settings(&DepthSettings::default()).validate().map_err(|e| match e {
            StackError::Setting { name, .. } => Invalid::new(name, e.to_string()),
            other => Invalid::new("settings", other.to_string()),
        })
    }
}

/// A present value, even `null`: tells `"coarseLimit": null` (off for this stack) from a missing key (the project's).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

/// A weighted average stacked into a new output.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StackParams {
    /// The output's name; "Weighted average" when left out.
    #[serde(default)]
    name: Option<String>,
    /// The radius sharpness is measured over, in pixels, from 0 to 64; the project's (`stack.get`) when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 64.0))]
    radius: Option<f64>,
    /// How smoothly the blend moves from one frame to the next, in pixels, from 0 to 64; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 64.0))]
    smoothing: Option<f64>,
    /// How strongly the sharpest frame wins, from 0.5 to 4; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.5, max = 4.0))]
    power: Option<f64>,
}

impl Params for StackParams {
    fn check(&self) -> Result<(), Invalid> {
        if let Some(name) = &self.name {
            check_name("name", name)?;
        }
        AverageParams { radius: self.radius, smoothing: self.smoothing, power: self.power }.check()
    }
}

/// A pyramid stacked into a new output.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PyramidParams {
    /// The output's name; "Pyramid" when left out.
    #[serde(default)]
    name: Option<String>,
    /// The number of pyramid levels, 1 to 10, or 0 to choose from the canvas's size; the project's (`stack.get`) when
    /// left out.
    #[serde(default)]
    #[schemars(range(max = 10))]
    levels: Option<u32>,
    /// Grit suppression at the finest level, 0 (off) or from 0.25 to 1; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    grit: Option<f64>,
    /// Whether choices are made on luminance or on each channel; the project's when left out.
    #[serde(default)]
    decisions: Option<Decisions>,
    /// How strictly each pixel is held within the frames' range there, from 0 to 1; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    range_limit: Option<f64>,
    /// How the result is brought into [0, 1]: compress, clip or keep; the project's when left out.
    #[serde(default)]
    tone: Option<Tone>,
    /// The coarse limit in frames, 0 to 1000, or `null` for off; the project's when left out.
    #[serde(default, deserialize_with = "present")]
    #[schemars(range(max = 1000))]
    #[allow(clippy::option_option)] // Missing (the project's), `null` (off for this stack) or a limit.
    coarse_limit: Option<Option<u32>>,
}

impl PyramidParams {
    fn overrides(&self) -> PyramidOverrides {
        PyramidOverrides {
            levels: self.levels,
            grit: self.grit,
            decisions: self.decisions,
            range_limit: self.range_limit,
            tone: self.tone,
            coarse_limit: self.coarse_limit,
        }
    }
}

impl Params for PyramidParams {
    fn check(&self) -> Result<(), Invalid> {
        if let Some(name) = &self.name {
            check_name("name", name)?;
        }
        self.overrides().check()
    }
}

/// A depth map stacked into a new output.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DepthParams {
    /// The output's name; "Depth map" when left out.
    #[serde(default)]
    name: Option<String>,
    /// The estimation radius, in pixels, from 0 to 64; the project's (`stack.get`) when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 64.0))]
    radius: Option<f64>,
    /// The depth map's smoothing radius, in pixels, from 0 to 256; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 256.0))]
    smoothing: Option<f64>,
    /// How the contrast threshold is chosen: auto, level or percentile; the project's when left out.
    #[serde(default)]
    threshold: Option<ThresholdMode>,
    /// The threshold's absolute level, from 0, for `"threshold": "level"`; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    level: Option<f64>,
    /// The share of the pixels to fill at least, in percent from 0 to 100, for `"threshold": "percentile"`; the
    /// project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 100.0))]
    percentile: Option<f64>,
}

impl DepthParams {
    fn overrides(&self) -> DepthOverrides {
        DepthOverrides { radius: self.radius, smoothing: self.smoothing, threshold: self.threshold, level: self.level, percentile: self.percentile }
    }
}

impl Params for DepthParams {
    fn check(&self) -> Result<(), Invalid> {
        if let Some(name) = &self.name {
            check_name("name", name)?;
        }
        self.overrides().check()
    }
}

/// A stacking method, by its name in outputs and commands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum MethodName {
    /// The weighted average of `stack.average`.
    #[default]
    Average,
    /// The pyramid method of `stack.pyramid`.
    Pyramid,
    /// The depth-map method of `stack.depthMap`.
    #[serde(rename = "depth-map")]
    DepthMap,
}

/// A stacking method and its settings for one stack, on top of the project's: what `stack.preview` takes, and each
/// stage of `stack.slabbed`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct MethodParams {
    /// The method, "average", "pyramid" or "depth-map"; the settings below are that method's. Left out, the command's
    /// own: "average" for `stack.preview`, "pyramid" for the slabs of `stack.slabbed` and for its final
    /// stack.
    #[serde(default)]
    method: Option<MethodName>,
    /// The weighted average's or the depth-map method's radius, in pixels of the full-size frames, from 0 to 64; the
    /// project's (`stack.get`) when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 64.0))]
    radius: Option<f64>,
    /// The weighted average's smoothing, in pixels of the full-size frames, from 0 to 64, or the depth-map method's,
    /// from 0 to 256; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 256.0))]
    smoothing: Option<f64>,
    /// The weighted average's power, from 0.5 to 4; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.5, max = 4.0))]
    power: Option<f64>,
    /// The pyramid's levels at full size, 0 to 10 (0 chooses); the project's when left out.
    #[serde(default)]
    #[schemars(range(max = 10))]
    levels: Option<u32>,
    /// The pyramid's grit suppression, 0 or from 0.25 to 1; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    grit: Option<f64>,
    /// The pyramid's decisions, luminance or channels; the project's when left out.
    #[serde(default)]
    decisions: Option<Decisions>,
    /// The pyramid's range limit, from 0 to 1; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    range_limit: Option<f64>,
    /// The pyramid's tone compression: compress, clip or keep; the project's when left out.
    #[serde(default)]
    tone: Option<Tone>,
    /// The pyramid's coarse limit in frames, or `null` for off; the project's when left out.
    #[serde(default, deserialize_with = "present")]
    #[schemars(range(max = 1000))]
    #[allow(clippy::option_option)] // Missing (the project's), `null` (off for this stack) or a limit.
    coarse_limit: Option<Option<u32>>,
    /// How the depth-map method's contrast threshold is chosen: auto, level or percentile; the project's when left out.
    #[serde(default)]
    threshold: Option<ThresholdMode>,
    /// The depth-map method's threshold level, from 0; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0))]
    level: Option<f64>,
    /// The depth-map method's threshold percentile, from 0 to 100; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 100.0))]
    percentile: Option<f64>,
}

impl MethodParams {
    fn average(&self) -> AverageParams {
        AverageParams { radius: self.radius, smoothing: self.smoothing, power: self.power }
    }

    fn pyramid(&self) -> PyramidOverrides {
        PyramidOverrides {
            levels: self.levels,
            grit: self.grit,
            decisions: self.decisions,
            range_limit: self.range_limit,
            tone: self.tone,
            coarse_limit: self.coarse_limit,
        }
    }

    fn depth(&self) -> DepthOverrides {
        DepthOverrides { radius: self.radius, smoothing: self.smoothing, threshold: self.threshold, level: self.level, percentile: self.percentile }
    }

    /// The method these parameters name, or `default`, with their settings on top of the project's `settings`.
    pub(super) fn chosen(&self, default: MethodName, settings: &StackSettings) -> Chosen {
        match self.method.unwrap_or(default) {
            MethodName::Average => Chosen::Average(self.average().settings(&settings.average)),
            MethodName::Pyramid => Chosen::Pyramid(self.pyramid().settings(&settings.pyramid)),
            MethodName::DepthMap => Chosen::DepthMap(self.depth().settings(&settings.depth_map)),
        }
    }

    /// Checks the parameters for the method they name, or `default`: settings of another method are errors.
    pub(super) fn check_for(&self, default: MethodName) -> Result<(), Invalid> {
        let threshold = self.depth().given_threshold();
        match self.method.unwrap_or(default) {
            MethodName::Average => {
                if let Some(name) = self.pyramid().given() {
                    return Err(Invalid::new(name, "is a setting of the pyramid method: pass `\"method\": \"pyramid\"`"));
                }
                if let Some(name) = threshold {
                    return Err(Invalid::new(name, "is a setting of the depth-map method: pass `\"method\": \"depth-map\"`"));
                }
                self.average().check()
            }
            MethodName::Pyramid => {
                if let Some(name) = self.average().given().or(threshold) {
                    return Err(Invalid::new(name, "is a setting of another method, not of the pyramid method"));
                }
                self.pyramid().check()
            }
            MethodName::DepthMap => {
                if let Some(name) = self.pyramid().given().or(self.power.map(|_| "power")) {
                    return Err(Invalid::new(name, "is a setting of another method, not of the depth-map method"));
                }
                self.depth().check()
            }
        }
    }
}

impl Params for MethodParams {
    fn check(&self) -> Result<(), Invalid> {
        self.check_for(MethodName::Average)
    }
}

/// A method and its settings for one stack.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Chosen {
    Average(AverageSettings),
    Pyramid(PyramidSettings),
    DepthMap(DepthSettings),
}

/// The settings a stack was made with, as its result reports them.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub(super) enum UsedSettings {
    Average(AverageSettings),
    Pyramid(PyramidSettings),
    DepthMap(DepthSettings),
}

impl Chosen {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Chosen::Average(_) => AVERAGE,
            Chosen::Pyramid(_) => PYRAMID,
            Chosen::DepthMap(_) => DEPTH_MAP,
        }
    }

    /// The method's name as outputs are named by default.
    pub(super) fn title(&self) -> &'static str {
        match self {
            Chosen::Average(_) => "Weighted average",
            Chosen::Pyramid(_) => "Pyramid",
            Chosen::DepthMap(_) => "Depth map",
        }
    }

    /// How many times the method reads every frame.
    pub(super) fn passes(&self) -> usize {
        if matches!(self, Chosen::DepthMap(_)) { 2 } else { 1 }
    }

    pub(super) fn validate(&self) -> Result<(), StackError> {
        match self {
            Chosen::Average(s) => s.validate(),
            Chosen::Pyramid(s) => s.validate(),
            Chosen::DepthMap(s) => s.validate(),
        }
    }

    /// The settings for frames reduced to a quarter of their size.
    fn reduced(&self) -> Chosen {
        let factor = PREVIEW_FACTOR as f64;
        match self {
            Chosen::Average(s) => Chosen::Average(s.reduced(factor)),
            Chosen::Pyramid(s) => Chosen::Pyramid(s.reduced(factor)),
            Chosen::DepthMap(s) => Chosen::DepthMap(s.reduced(factor)),
        }
    }

    pub(super) fn used(&self) -> UsedSettings {
        match self {
            Chosen::Average(s) => UsedSettings::Average(s.clone()),
            Chosen::Pyramid(s) => UsedSettings::Pyramid(s.clone()),
            Chosen::DepthMap(s) => UsedSettings::DepthMap(s.clone()),
        }
    }

    /// How the stack is brought into range: the weighted average's and the depth map's blends never leave their
    /// frames' range.
    pub(super) fn tone(&self) -> Tone {
        match self {
            Chosen::Average(_) | Chosen::DepthMap(_) => Tone::Keep,
            Chosen::Pyramid(s) => s.tone,
        }
    }

    /// The tone compression a stack is stored with ([`store`]): the pyramid's, `None` for the other methods.
    pub(super) fn ranged(&self) -> Option<Tone> {
        match self {
            Chosen::Pyramid(s) => Some(s.tone),
            Chosen::Average(_) | Chosen::DepthMap(_) => None,
        }
    }

    /// The pyramid's levels on a `width` x `height` canvas.
    pub(super) fn levels(&self, width: usize, height: usize) -> Option<usize> {
        match self {
            Chosen::Average(_) | Chosen::DepthMap(_) => None,
            Chosen::Pyramid(s) => Some(s.levels_for(width, height)),
        }
    }

    /// The parameters an output records.
    pub(super) fn parameters(&self, framing: Framing, width: usize, height: usize) -> serde_json::Value {
        match self {
            Chosen::Average(s) => serde_json::json!({ "radius": s.radius, "smoothing": s.smoothing, "power": s.power, "framing": framing }),
            Chosen::Pyramid(s) => serde_json::json!({
                "levels": s.levels_for(width, height),
                "grit": s.grit,
                "decisions": s.decisions,
                "rangeLimit": s.range_limit,
                "tone": s.tone,
                "coarseLimit": s.coarse_limit,
                "framing": framing,
            }),
            Chosen::DepthMap(s) => serde_json::json!({
                "radius": s.radius,
                "smoothing": s.smoothing,
                "threshold": s.threshold,
                "level": s.level,
                "percentile": s.percentile,
                "framing": framing,
            }),
        }
    }
}

/// A frame warped onto the canvas a rectangle at a time, as the stacker reads it, with its coverage, which ramps up over
/// `feather` of the frame's pixels inside its edge.
struct Warped<'a> {
    frame: &'a Image<f32>,
    transform: Affine,
    canvas: Canvas,
    feather: f64,
}

impl FrameSource for Warped<'_> {
    fn width(&self) -> usize {
        self.canvas.width
    }

    fn height(&self) -> usize {
        self.canvas.height
    }

    fn channels(&self) -> usize {
        self.frame.channels()
    }

    fn read(&self, rect: Rect) -> Result<Patch, StackError> {
        let covered = warp_to_canvas(self.frame, &self.transform, &self.canvas, rect, self.feather).map_err(|e| match e {
            AlignError::Image(e) => StackError::Image(e),
            AlignError::Op(e) => StackError::Op(e),
            // A rectangle outside the canvas is the only other way the warp fails.
            _ => StackError::Region {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                frame_width: self.canvas.width,
                frame_height: self.canvas.height,
            },
        })?;
        Ok(Patch { image: covered.image, coverage: Some(covered.coverage) })
    }
}

/// A size in pixels, rounded, at least 1.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // A canvas's side times a scale below 1.
fn to_size(v: f64) -> usize {
    if v.is_finite() { v.round().max(1.0) as usize } else { 1 }
}

/// The geometry of a frame reduced to `small` x `of` its `full` size: the canvas at the reduced frame's scale (with
/// the extents lined up, as in a resize) and the transform from the master's full-size pixels to the reduced frame's,
/// so that [`warp_to_canvas`] warps the reduced frame onto the reduced canvas as it would the frame onto the canvas.
fn reduced_geometry(canvas: &Canvas, transform: &Affine, full: (usize, usize), small: (usize, usize)) -> (Canvas, Affine, f64) {
    let (tx, ty) = (small.0 as f64 / full.0.max(1) as f64, small.1 as f64 / full.1.max(1) as f64);
    let scale = f64::midpoint(tx, ty);
    let (width, height) = (to_size(canvas.width as f64 * scale), to_size(canvas.height as f64 * scale));
    let (sx, sy) = (canvas.width as f64 / width as f64, canvas.height as f64 / height as f64);
    let to_canvas = Affine { m: [[sx, 0.0, 0.5 * sx - 0.5], [0.0, sy, 0.5 * sy - 0.5]] };
    let to_small = Affine { m: [[tx, 0.0, 0.5 * tx - 0.5], [0.0, ty, 0.5 * ty - 0.5]] };
    let reduced = Canvas { width, height, to_master: to_canvas.then(&canvas.to_master) };
    (reduced, transform.then(&to_small), DEFAULT_FEATHER * scale)
}

/// A frame to stack: its index in the plan's frames, and its alignment.
pub(super) struct ToStack {
    pub(super) index: usize,
    pub(super) id: FrameId,
    pub(super) transform: Affine,
    pub(super) gain: f32,
}

/// What a stacking job reads, taken from the project when it starts.
pub(super) struct StackPlan {
    command: &'static str,
    pub(super) reader: Plan,
    /// The frames to stack, in the order they are stacked.
    pub(super) frames: Vec<ToStack>,
    /// Enabled frames left out: their alignment was rejected.
    pub(super) skipped: Vec<FrameId>,
    pub(super) canvas: Canvas,
    pub(super) framing: Framing,
    /// The frames' size, which the transforms are in.
    size: (usize, usize),
    pub(super) settings: Chosen,
    pub(super) cache: CacheDir,
    /// Which device stacks, resolved on the worker when the stack starts (`crate::compute`).
    pub(super) compute: Compute,
    /// How the stack keeps its working data: the memory budget and the tile folder (`compute.set`).
    pub(super) tiling: Tiling,
}

/// The gain as the frames' samples take it.
#[allow(clippy::cast_possible_truncation)] // A gain near 1.
fn gain(g: f64) -> f32 {
    if g.is_finite() && g > 0.0 { g as f32 } else { 1.0 }
}

impl StackPlan {
    pub(super) fn new(session: &Session, command: &'static str, settings: Chosen, quality: Quality) -> Result<StackPlan, CommandError> {
        let framing = align_settings(session.project()?)?.framing;
        StackPlan::build(session, command, settings, quality, None, framing)
    }

    /// The plan for `frames` (an output's, enabled or not) on the canvas `framing` gives, rather than the enabled
    /// frames and the framing the settings choose.
    pub(crate) fn of_frames(
        session: &Session,
        command: &'static str,
        settings: Chosen,
        quality: Quality,
        frames: &[FrameId],
        framing: Framing,
    ) -> Result<StackPlan, CommandError> {
        StackPlan::build(session, command, settings, quality, Some(frames), framing)
    }

    fn build(
        session: &Session,
        command: &'static str,
        settings: Chosen,
        quality: Quality,
        frames: Option<&[FrameId]>,
        framing: Framing,
    ) -> Result<StackPlan, CommandError> {
        let project = session.project()?;
        settings.validate()?;
        let Some(stored) = stored_alignment(project)? else {
            return Err(session.unavailable("the stack isn't aligned: align it first with `align.run`"));
        };
        let canvas = stored.canvas(framing)?;
        let reader = match frames {
            Some(ids) => Plan::with_frames(session, command, ids)?,
            None => Plan::new(session, command, 1)?,
        }
        .with_quality(quality);
        let enabled = reader.ids();
        let missing: Vec<String> = enabled.iter().filter(|id| !stored.frames.iter().any(|f| f.frame == **id)).map(ToString::to_string).collect();
        if !missing.is_empty() {
            return Err(session.unavailable(format!(
                "{} {} added or enabled since the stack was aligned: align it again with `align.run`",
                missing.join(", "),
                if missing.len() == 1 { "was" } else { "were" }
            )));
        }
        // The run's processing order, then any frame of the run it doesn't list.
        let mut order: Vec<FrameId> = stored.report.order.iter().copied().filter(|id| enabled.contains(id)).collect();
        let unlisted: Vec<FrameId> = enabled.iter().copied().filter(|id| !order.contains(id)).collect();
        order.extend(unlisted);
        let (mut frames, mut skipped) = (Vec::new(), Vec::new());
        for id in order {
            let (Some(index), Some(f)) = (enabled.iter().position(|&e| e == id), stored.frames.iter().find(|f| f.frame == id)) else { continue };
            if f.report.as_ref().is_some_and(|r| r.status == Status::Rejected) {
                skipped.push(id);
            } else {
                frames.push(ToStack { index, id, transform: f.alignment.transform, gain: gain(f.alignment.brightness_gain) });
            }
        }
        if frames.is_empty() {
            return Err(session.unavailable("every enabled frame's alignment was rejected, so there is nothing to stack: see `align.info`"));
        }
        Ok(StackPlan {
            command,
            reader,
            frames,
            skipped,
            canvas,
            framing,
            size: (stored.report.width, stored.report.height),
            settings,
            cache: session.cache_dir()?,
            compute: session.compute(),
            tiling: session.tiling(),
        })
    }

    pub(super) fn internal(&self) -> CommandError {
        CommandError::Internal { command: self.command.to_owned() }
    }

    /// Reads the `k`-th frame to stack, developed and inpainted, and checks its size against the alignment's.
    fn read(&mut self, k: usize) -> Result<Image<f32>, CommandError> {
        let Some(f) = self.frames.get(k) else { return Err(self.internal()) };
        let (index, id) = (f.index, f.id);
        let image = self.reader.read(index)?;
        if (image.width(), image.height()) != self.size {
            return Err(CommandError::Unavailable {
                command: self.command.to_owned(),
                reason: format!(
                    "{id} is {}x{} pixels, but the stack was aligned with frames of {}x{}: align it again with `align.run`",
                    image.width(),
                    image.height(),
                    self.size.0,
                    self.size.1
                ),
            });
        }
        Ok(image)
    }

    /// Reads the `k`-th frame to stack, at full size or, `reduced`, at a quarter of it, with what puts it on the
    /// canvas: its alignment, gain and coverage's feather.
    pub(super) fn load(&mut self, k: usize, reduced: bool) -> Result<Loaded, CommandError> {
        let image = self.read(k)?;
        let Some(frame) = self.frames.get(k) else { return Err(self.internal()) };
        if reduced {
            let (w, h) = self.size;
            let small = thumbnail(&image, w.max(h).div_ceil(PREVIEW_FACTOR))?;
            drop(image);
            let (canvas, transform, feather) = reduced_geometry(&self.canvas, &frame.transform, (w, h), (small.width(), small.height()));
            Ok(Loaded::Warp { image: small, transform, canvas, feather, gain: frame.gain })
        } else {
            Ok(Loaded::Warp { image, transform: frame.transform, canvas: self.canvas, feather: DEFAULT_FEATHER, gain: frame.gain })
        }
    }

    /// Stacks every frame with the plan's method, at full size or, `reduced`, at a quarter of it, each frame read,
    /// folded in and dropped: stage 0 reads the frames for a one-pass method, stages 0 and 1 read them twice for the
    /// depth-map method, and the last stage is the end. The stack and, for the depth-map method, its depth map.
    fn stack_any(&mut self, ctx: &JobContext, reduced: bool) -> Result<(Stacked, Option<DepthMap>), CommandError> {
        let settings = if reduced { self.settings.reduced() } else { self.settings.clone() };
        self.reader.load_mask()?;
        let n = self.frames.len();
        let mut units = Units::new(n);
        let (compute, tiling) = (self.compute, self.tiling.clone());
        let mut feed = Warping { plan: self, range: 0..n, reduced };
        stack_feed(&mut feed, &settings, &tiling, &compute, ctx, ctx, &mut units, |stage, units| {
            ctx.stage(stage);
            *units = Units::new(n);
        })
    }

    pub(super) fn ids(&self) -> Vec<FrameId> {
        self.frames.iter().map(|f| f.id).collect()
    }
}

/// Progress through a stage: a unit for each frame a stacker reads.
pub(super) struct Units {
    pub(super) done: u64,
    pub(super) total: u64,
}

impl Units {
    pub(super) fn new(total: usize) -> Units {
        Units { done: 0, total: total as u64 }
    }
}

/// A frame read for stacking, before it is put on the canvas: what a [`Feed`] loads, on another thread while the
/// frame before it is stacked.
pub(super) enum Loaded {
    /// A decoded frame, warped onto the canvas as it is stacked: by florastack-align's `warp_to_canvas` a rectangle
    /// at a time on the CPU ([`Warped`]), by florastack-gpu's warp kernel a tile at a time on the GPU.
    Warp { image: Image<f32>, transform: Affine, canvas: Canvas, feather: f64, gain: f32 },
    /// A frame already on the canvas (a slab's output), with its coverage where it has one.
    Canvas { image: Image<f32>, coverage: Option<Image<f32>> },
}

/// A frame on the canvas with its coverage, read from borrowed planes (florastack-stack's `CoveredFrame` owns them).
struct OnCanvas<'a> {
    image: &'a Image<f32>,
    coverage: &'a Image<f32>,
}

impl FrameSource for OnCanvas<'_> {
    fn width(&self) -> usize {
        self.image.width()
    }

    fn height(&self) -> usize {
        self.image.height()
    }

    fn channels(&self) -> usize {
        self.image.channels()
    }

    fn read(&self, rect: Rect) -> Result<Patch, StackError> {
        if rect.is_empty() || !rect.fits_in(self.image.width(), self.image.height()) {
            return Err(StackError::Region {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                frame_width: self.image.width(),
                frame_height: self.image.height(),
            });
        }
        Ok(Patch { image: self.image.crop(rect)?, coverage: Some(self.coverage.crop(rect)?) })
    }
}

impl Loaded {
    /// The frame as florastack-stack's stackers read it.
    pub(super) fn source<R>(&self, f: impl FnOnce(&dyn FrameSource) -> R) -> R {
        match self {
            Loaded::Warp { image, transform, canvas, feather, gain } => {
                f(&Gained { frame: Warped { frame: image, transform: *transform, canvas: *canvas, feather: *feather }, gain: *gain })
            }
            Loaded::Canvas { image, coverage: Some(coverage) } => f(&OnCanvas { image, coverage }),
            Loaded::Canvas { image, coverage: None } => f(image),
        }
    }

    /// The frame as florastack-gpu's stackers read it.
    fn gpu(&self) -> GpuFrame<'_> {
        match self {
            Loaded::Warp { image, transform, canvas, feather, gain } => GpuFrame::Warp {
                image,
                geometry: WarpGeometry {
                    to_frame: canvas.to_frame(transform),
                    frame_width: image.width(),
                    frame_height: image.height(),
                    feather: *feather,
                    gain: *gain,
                },
            },
            Loaded::Canvas { image, coverage } => GpuFrame::Canvas { image, coverage: coverage.as_ref() },
        }
    }

    /// The canvas's width and height, and the frame's channels.
    fn shape(&self) -> (usize, usize, usize) {
        match self {
            Loaded::Warp { image, canvas, .. } => (canvas.width, canvas.height, image.channels()),
            Loaded::Canvas { image, .. } => (image.width(), image.height(), image.channels()),
        }
    }
}

/// Frames to stack, read one at a time.
pub(super) trait Feed: Send {
    fn len(&self) -> usize;

    /// Reads frame `i`.
    fn load(&mut self, i: usize) -> Result<Loaded, CommandError>;
}

/// The frames of a plan in `range` of its frames to stack, warped onto the canvas, at full size or, `reduced`, at a
/// quarter of it.
pub(super) struct Warping<'a> {
    pub(super) plan: &'a mut StackPlan,
    pub(super) range: Range<usize>,
    pub(super) reduced: bool,
}

impl Feed for Warping<'_> {
    fn len(&self) -> usize {
        self.range.len()
    }

    fn load(&mut self, i: usize) -> Result<Loaded, CommandError> {
        self.plan.load(self.range.start + i, self.reduced)
    }
}

/// Reads every frame of `feed`, in order, and hands it to `fold` with its progress, one of `units`, and whether it is
/// the first. While a frame is folded, the next one is read and decoded beside it (`rayon::join`), so that decoding
/// overlaps stacking: two frames are in memory at most, the one folded and the next. With a single thread nothing
/// would overlap, so each frame is read only once the one before is folded, and memory holds one frame.
pub(super) fn each<E>(
    feed: &mut dyn Feed,
    ctx: &dyn Monitor,
    units: &mut Units,
    mut fold: impl FnMut(&Loaded, &dyn Monitor, bool) -> Result<(), E> + Send,
) -> Result<(), E>
where
    E: From<CommandError> + Send,
{
    let n = feed.len();
    let overlap = rayon::current_num_threads() > 1;
    let mut next: Option<Loaded> = None;
    for i in 0..n {
        ctx.checkpoint().map_err(CommandError::from)?;
        let current = match next.take() {
            Some(frame) => frame,
            None => feed.load(i)?,
        };
        let progress = FrameProgress { inner: ctx, frame: units.done, frames: units.total };
        if overlap && i + 1 < n {
            let (folded, loaded) = rayon::join(|| fold(&current, &progress, i == 0), || feed.load(i + 1));
            folded?;
            next = Some(loaded?);
        } else {
            fold(&current, &progress, i == 0)?;
        }
        drop(current);
        units.done += 1;
    }
    Ok(())
}

/// Stacks `feed` with `settings`, in one pass, or in two for the depth-map method: its focus, then the composite,
/// with the depth map made between them. `stage(i, units)` comes before pass `i` and, with `i` the number of passes,
/// before the end, whose progress goes to `finishing`. On the GPU when `compute` resolves to one; when the GPU fails
/// part way, the stack starts again on the CPU. The stack and, for the depth-map method, its depth map.
#[allow(clippy::too_many_arguments)] // The stack's inputs and the three ways its progress is reported.
pub(super) fn stack_feed(
    feed: &mut dyn Feed,
    settings: &Chosen,
    tiling: &Tiling,
    compute: &Compute,
    ctx: &dyn Monitor,
    finishing: &dyn Monitor,
    units: &mut Units,
    mut stage: impl FnMut(usize, &mut Units),
) -> Result<(Stacked, Option<DepthMap>), CommandError> {
    if let Backend::Gpu { gpu, tiling: gpu_tiling } = compute.backend() {
        if let Some(reason) = cpu_only(settings) {
            tracing::info!("stacking on the CPU: {reason}");
        } else {
            let gpu_tiling = GpuTiling { host_budget: tiling.budget, spill_dir: tiling.spill_dir.clone(), ..gpu_tiling };
            let start = units.done;
            match stack_gpu(feed, settings, &gpu, &gpu_tiling, ctx, finishing, units, &mut stage) {
                Ok(done) => return Ok(done),
                Err(Failed::Command(e)) => return Err(e),
                Err(Failed::Gpu(e)) if e.is_cancelled() => return Err(CommandError::Cancelled),
                Err(Failed::Gpu(e)) => {
                    tracing::warn!("the GPU failed while stacking ({e}): stacking again on the CPU");
                    units.done = start;
                }
            }
        }
    }
    stack_cpu(feed, settings, tiling, ctx, finishing, units, stage)
}

/// Why `settings` stack on the CPU even with a GPU: the settings the GPU's kernels don't have.
fn cpu_only(settings: &Chosen) -> Option<&'static str> {
    match settings {
        Chosen::Pyramid(s) if s.coarse_limit.is_some() => Some("the pyramid's coarse limit isn't done on the GPU"),
        _ => None,
    }
}

/// [`stack_feed`] on the CPU.
fn stack_cpu(
    feed: &mut dyn Feed,
    settings: &Chosen,
    tiling: &Tiling,
    ctx: &dyn Monitor,
    finishing: &dyn Monitor,
    units: &mut Units,
    mut stage: impl FnMut(usize, &mut Units),
) -> Result<(Stacked, Option<DepthMap>), CommandError> {
    stage(0, units);
    let mut stacker = match settings {
        Chosen::Average(s) => s.stacker(tiling)?,
        Chosen::Pyramid(s) => s.stacker(tiling)?,
        Chosen::DepthMap(depth) => {
            let mut estimator = depth.estimator(tiling)?;
            each(feed, ctx, units, |loaded, progress, first| {
                loaded.source(|source| {
                    if first {
                        estimator.begin(StackShape::of(source))?;
                    }
                    Ok::<_, CommandError>(estimator.add_frame(source, progress)?)
                })
            })?;
            let stats = estimator.finish(finishing)?;
            drop(estimator);
            ctx.checkpoint()?;
            let map = stats.resolve(depth.contrast_threshold(), depth.smoothing, None)?;
            drop(stats);
            stage(1, units);
            let mut compositor = map.compositor(tiling)?;
            each(feed, ctx, units, |loaded, progress, first| {
                loaded.source(|source| {
                    if first {
                        compositor.begin(StackShape::of(source))?;
                    }
                    Ok::<_, CommandError>(compositor.add_frame(source, progress)?)
                })
            })?;
            stage(2, units);
            return Ok((compositor.finish(finishing)?, Some(map)));
        }
    };
    each(feed, ctx, units, |loaded, progress, first| {
        loaded.source(|source| {
            if first {
                stacker.begin(StackShape::of(source))?;
            }
            Ok::<_, CommandError>(stacker.add_frame(source, progress)?)
        })
    })?;
    stage(1, units);
    Ok((stacker.finish(finishing)?, None))
}

/// Why a stack on the GPU failed: the GPU (the stack starts again on the CPU), or anything else (it ends).
enum Failed {
    Gpu(GpuError),
    Command(CommandError),
}

impl From<GpuError> for Failed {
    fn from(e: GpuError) -> Failed {
        Failed::Gpu(e)
    }
}

impl From<CommandError> for Failed {
    fn from(e: CommandError) -> Failed {
        Failed::Command(e)
    }
}

impl From<StackError> for Failed {
    fn from(e: StackError) -> Failed {
        Failed::Command(e.into())
    }
}

/// florastack-stack's `Stacked` of a GPU stack.
fn stacked(s: GpuStacked) -> Stacked {
    Stacked { image: s.image, alpha: s.alpha, frames: s.frames, uncovered: s.uncovered }
}

/// The focus statistics of the GPU's pass 1, whose three channels are each pixel's depth, confidence and guide.
fn focus_stats(s: GpuStacked, radius: f64) -> Result<FocusStats, StackError> {
    let (w, h) = (s.image.width(), s.image.height());
    let mut planes = [(); 3].map(|()| Vec::with_capacity(w * h));
    for row in s.image.rows() {
        for pixel in row.as_chunks::<3>().0 {
            for (plane, &v) in planes.iter_mut().zip(pixel) {
                plane.push(v);
            }
        }
    }
    let [depth, confidence, guide] = planes.map(|p| Image::from_vec(w, h, 1, florastack_core::ColorSpace::LINEAR, p));
    Ok(FocusStats { frames: s.frames, depth: depth?, confidence: confidence?, guide: guide?, covered: s.alpha, uncovered: s.uncovered, radius })
}

/// Folds every frame of `feed` into `stacker`, which begins its stack on the first frame's canvas.
fn gpu_pass(stacker: &mut GpuStacker, feed: &mut dyn Feed, ctx: &dyn Monitor, units: &mut Units) -> Result<(), Failed> {
    each(feed, ctx, units, |loaded, progress, first| {
        if first {
            let (w, h, c) = loaded.shape();
            stacker.begin(w, h, c)?;
        }
        Ok(stacker.add_frame(&loaded.gpu(), progress)?)
    })
}

/// [`stack_feed`] on the GPU (florastack-gpu's stackers, with the depth map between the depth-map method's passes
/// made on the CPU).
#[allow(clippy::too_many_arguments)] // As stack_feed's.
fn stack_gpu(
    feed: &mut dyn Feed,
    settings: &Chosen,
    gpu: &Gpu,
    tiling: &GpuTiling,
    ctx: &dyn Monitor,
    finishing: &dyn Monitor,
    units: &mut Units,
    stage: &mut impl FnMut(usize, &mut Units),
) -> Result<(Stacked, Option<DepthMap>), Failed> {
    stage(0, units);
    let method = match settings {
        Chosen::Average(s) => GpuMethod::Average { radius: s.radius, smoothing: s.smoothing, power: s.power },
        Chosen::Pyramid(s) => {
            #[allow(clippy::cast_possible_truncation)] // A grit from 0.25 to 1, a range limit from 0 to 1.
            let (blend, range_limit) = ((s.grit > 0.0).then(|| (1.0 / s.grit) as f32), s.range_limit as f32);
            GpuMethod::Pyramid { levels: s.levels, blend, channel_decisions: s.decisions == Decisions::Channels, range_limit }
        }
        Chosen::DepthMap(depth) => {
            let mut estimator = GpuStacker::new(gpu, GpuMethod::Focus { radius: depth.radius }, tiling.clone())?;
            gpu_pass(&mut estimator, feed, ctx, units)?;
            let stats = focus_stats(estimator.finish(finishing)?, depth.radius)?;
            drop(estimator);
            ctx.checkpoint().map_err(CommandError::from)?;
            let map = stats.resolve(depth.contrast_threshold(), depth.smoothing, None)?;
            drop(stats);
            stage(1, units);
            let method = GpuMethod::Composite { depth: std::sync::Arc::clone(&map.depth), frames: map.frames };
            let mut compositor = GpuStacker::new(gpu, method, tiling.clone())?;
            gpu_pass(&mut compositor, feed, ctx, units)?;
            stage(2, units);
            return Ok((stacked(compositor.finish(finishing)?), Some(map)));
        }
    };
    let mut stacker = GpuStacker::new(gpu, method, tiling.clone())?;
    gpu_pass(&mut stacker, feed, ctx, units)?;
    stage(1, units);
    Ok((stacked(stacker.finish(finishing)?), None))
}

/// `image` with `alpha`, if there is one: an extra channel for gray and color images, the alpha channel they have
/// multiplied by it otherwise.
pub(super) fn with_alpha(image: &Image<f32>, alpha: Option<&Image<f32>>) -> Result<Image<f32>, CommandError> {
    let Some(alpha) = alpha else { return Ok(image.clone()) };
    let c = image.channels();
    let extra = usize::from(c == 1 || c == 3);
    let out_c = c + extra;
    let mut out = Image::new(image.width(), image.height(), out_c, image.color_space().clone()).map_err(StackError::from)?;
    for ((dst, src), a) in out.rows_mut().zip(image.rows()).zip(alpha.rows()) {
        for ((d, s), &a) in dst.chunks_exact_mut(out_c).zip(src.chunks_exact(c)).zip(a) {
            d[..c].copy_from_slice(s);
            if extra == 1 {
                d[c] = a;
            } else {
                d[c - 1] *= a;
            }
        }
    }
    Ok(out)
}

/// How a stack was brought into range.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct RangeSummary {
    /// The pixels the tone compression changed: 0 when every pixel was in [0, 1].
    out_of_range: usize,
    /// Whether the result before tone compression was kept as the output's `extended` map, for "retain extended
    /// range" files.
    extended: bool,
}

/// What stacking made.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct StackSummary {
    /// The new output.
    output: OutputId,
    /// Its size, in pixels.
    width: usize,
    height: usize,
    /// The frames stacked, in the order they were added.
    frames: Vec<FrameId>,
    /// The enabled frames left out, because their alignment was rejected.
    skipped: Vec<FrameId>,
    /// How many pixels no frame covers: transparent in the output (only wide framing has them).
    uncovered: usize,
    /// The settings it was stacked with, and the framing.
    settings: UsedSettings,
    framing: Framing,
    /// The pyramid's number of levels.
    #[serde(skip_serializing_if = "Option::is_none")]
    levels: Option<usize>,
    /// How the pyramid's result was brought into range.
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<RangeSummary>,
    /// The depth-map method's threshold and the pixels it filled.
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<DepthSummary>,
}

/// What the depth-map method's threshold did.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct DepthSummary {
    /// The threshold's level of confidence: the level given, or the percentile's.
    level: f32,
    /// How many covered pixels were below it, whose depth was filled from the confident pixels around.
    filled: usize,
}

/// What stacking reads: the frame list, the development, the alignment (and the defect mask). The settings aren't
/// locked: a stack runs with the settings it started with, so that they can change, and previews follow, meanwhile.
pub(super) fn locks() -> florastack_project::Aspects {
    florastack_project::Aspects::FRAMES.union(florastack_project::Aspects::DEVELOP).union(florastack_project::Aspects::ALIGN)
}

/// A stack in the cache: an output's files.
pub(super) struct Stored {
    pub(super) image: CacheRef,
    pub(super) preview: CacheRef,
    pub(super) maps: Maps,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) uncovered: usize,
    /// How a pyramid's result was brought into range.
    pub(super) range: Option<RangeSummary>,
    /// What the depth-map method's threshold did.
    pub(super) depth: Option<DepthSummary>,
}

/// The file a stack is read from when it is stacked again: its result before tone compression, kept as the `extended`
/// map when compression changed it, or else its image.
pub(super) fn unclipped<'a>(image: &'a CacheRef, maps: &'a Maps) -> &'a CacheRef {
    extended_map().and_then(|name| maps.get(&name)).unwrap_or(image)
}

/// Stores a stack in the cache under `stem`, with `key` on its image: the image, linear `f32`, with an alpha channel
/// when some pixels are covered by no frame; a small JPEG preview for lists; the depth-map method's depth and
/// confidence maps, 16-bit; and, when `tone` (a pyramid's: `None` for the methods whose blends never leave their
/// frames' range) changes any pixel, the result before it as the `extended` map.
pub(super) fn store(
    cache: &CacheDir,
    command: &str,
    stem: &str,
    stacked: Stacked,
    depth: Option<DepthMap>,
    tone: Option<Tone>,
    key: Option<String>,
) -> Result<Stored, CommandError> {
    let Stacked { mut image, alpha, uncovered, .. } = stacked;
    let internal = || CommandError::Internal { command: command.to_owned() };
    let (width, height) = (image.width(), image.height());
    let mut maps = Maps::new();
    let depth = match depth {
        Some(map) => {
            let stored = write_image(cache, &format!("{stem}-depth"), &AnyImage::U16(map.depth_u16()?), None)?;
            maps.insert(depth_map().ok_or_else(internal)?, stored);
            let stored = write_image(cache, &format!("{stem}-confidence"), &AnyImage::U16(map.confidence_u16()?), None)?;
            maps.insert(confidence_map().ok_or_else(internal)?, stored);
            Some(DepthSummary { level: map.level, filled: map.filled })
        }
        None => None,
    };
    let range = match tone {
        Some(tone) => {
            let mut summary = RangeSummary { out_of_range: 0, extended: false };
            if tone != Tone::Keep && out_of_range(&image) > 0 {
                let extended = write_image(cache, &format!("{stem}-extended"), &AnyImage::F32(with_alpha(&image, alpha.as_ref())?), None)?;
                maps.insert(extended_map().ok_or_else(internal)?, extended);
                summary.extended = true;
                summary.out_of_range = compress_range(&mut image, tone)?;
            }
            Some(summary)
        }
        None => None,
    };
    let reference = write_image(cache, stem, &AnyImage::F32(with_alpha(&image, alpha.as_ref())?), key)?;
    let preview = write_preview(cache, stem, &image, None)?;
    Ok(Stored { image: reference, preview, maps, width, height, uncovered, range, depth })
}

fn average(session: &Session, p: StackParams) -> Result<JobPlan<StackSummary>, CommandError> {
    let overrides = AverageParams { radius: p.radius, smoothing: p.smoothing, power: p.power };
    let settings = Chosen::Average(overrides.settings(&stack_settings(session.project()?)?.average));
    stack_job(session, "stack.average", settings, p.name.unwrap_or_else(|| "Weighted average".to_owned()))
}

fn pyramid(session: &Session, p: PyramidParams) -> Result<JobPlan<StackSummary>, CommandError> {
    let settings = Chosen::Pyramid(p.overrides().settings(&stack_settings(session.project()?)?.pyramid));
    stack_job(session, "stack.pyramid", settings, p.name.clone().unwrap_or_else(|| "Pyramid".to_owned()))
}

fn depth(session: &Session, p: DepthParams) -> Result<JobPlan<StackSummary>, CommandError> {
    let settings = Chosen::DepthMap(p.overrides().settings(&stack_settings(session.project()?)?.depth_map));
    stack_job(session, "stack.depthMap", settings, p.name.clone().unwrap_or_else(|| "Depth map".to_owned()))
}

/// A full stack with `settings` into a new output named `name`.
fn stack_job(session: &Session, command: &'static str, settings: Chosen, name: String) -> Result<JobPlan<StackSummary>, CommandError> {
    let mut plan = StackPlan::new(session, command, settings, Quality::Output)?;
    let passes = plan.settings.passes();
    let job = JobPlan::new(move |ctx| {
        let (stacked, depth) = plan.stack_any(ctx, false)?;
        ctx.checkpoint()?;
        let method = plan.settings.name();
        let Stored { image, preview, maps, width, height, uncovered, range, depth } =
            store(&plan.cache, command, method, stacked, depth, plan.settings.ranged(), None)?;
        let develop = match plan.reader.resolved_develop() {
            Some((frame, develop)) => Some(develop_record(frame, develop)?),
            None => None,
        };
        let ids = plan.ids();
        let parameters = plan.settings.parameters(plan.framing, width, height);
        let levels = plan.settings.levels(width, height);
        let (skipped, settings, framing) = (plan.skipped.clone(), plan.settings.used(), plan.framing);
        Ok(commit(move |session: &mut Session| {
            let mut edits = Vec::new();
            if let Some(develop) = develop {
                edits.push(Edit::SetDevelop(Some(develop)));
            }
            edits.push(Edit::AddOutput(Box::new(NewOutput {
                name,
                method: method.to_owned(),
                parameters,
                inputs: ids.iter().copied().map(Input::Frame).collect(),
                image,
                preview: Some(preview),
                maps,
            })));
            session.apply(Edit::Batch(edits))?;
            let output = session
                .project()?
                .outputs
                .last()
                .map(florastack_project::Output::id)
                .ok_or(CommandError::Internal { command: command.to_owned() })?;
            Ok(StackSummary { output, width, height, frames: ids, skipped, uncovered, settings, framing, levels, range, depth })
        }))
    });
    Ok(job.stages(stages(passes)).locks(locks()))
}

/// A stacking job's stages: one pass over the frames and the end, or the depth-map method's two passes.
fn stages(passes: usize) -> &'static [(&'static str, f32)] {
    if passes == 2 {
        &[("Measuring focus", 0.45), ("Compositing", 0.5), ("Finishing", 0.05)]
    } else {
        &[("Stacking frames", 0.95), ("Finishing", 0.05)]
    }
}

/// A stack's preview.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Previewed {
    /// The preview: a linear 32-bit float TIFF, with an alpha channel when some pixels are covered by no frame.
    path: String,
    /// Its size, in pixels: a quarter of the full stack's on each side.
    width: usize,
    height: usize,
    /// The frames stacked, in the order they were added.
    frames: Vec<FrameId>,
    /// The enabled frames left out, because their alignment was rejected.
    skipped: Vec<FrameId>,
    /// How many pixels no frame covers.
    uncovered: usize,
    /// The method, "average", "pyramid" or "depth-map".
    method: &'static str,
    /// The settings at full scale, and the framing.
    settings: UsedSettings,
    framing: Framing,
}

fn preview(session: &Session, p: MethodParams) -> Result<JobPlan<Previewed>, CommandError> {
    let settings = p.chosen(MethodName::Average, &stack_settings(session.project()?)?);
    let mut plan = StackPlan::new(session, "stack.preview", settings, Quality::Preview)?;
    let passes = plan.settings.passes();
    let job = JobPlan::new(move |ctx| {
        let (Stacked { mut image, alpha, uncovered, .. }, _) = plan.stack_any(ctx, true)?;
        ctx.checkpoint()?;
        let tone = plan.settings.tone();
        if tone != Tone::Keep {
            compress_range(&mut image, tone)?;
        }
        let reference = write_image_in(&plan.cache, Area::Previews, "stack-preview", &AnyImage::F32(with_alpha(&image, alpha.as_ref())?), None)?;
        let previewed = Previewed {
            path: reference.path.resolve(plan.cache.path()).display().to_string(),
            width: image.width(),
            height: image.height(),
            frames: plan.ids(),
            skipped: plan.skipped.clone(),
            uncovered,
            method: plan.settings.name(),
            settings: plan.settings.used(),
            framing: plan.framing,
        };
        Ok(commit(move |_: &mut Session| Ok(previewed)))
    });
    Ok(job.stages(stages(passes)))
}
