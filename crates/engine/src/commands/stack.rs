//! Stacking: the stacking settings, a preference group of the project (`stack.get`, `stack.set`, `stack.reset`), the
//! weighted average of the aligned frames as a new output (`stack.average`, a job), and its quarter-scale preview
//! (`stack.preview`, a preview job).
//!
//! Both jobs stack the enabled frames of the stored alignment run, in the run's processing order, on the canvas of the
//! framing the alignment settings choose, as `align.info` shows it; frames whose alignment was rejected are left out
//! (their transform is only their reference's). Every enabled frame must be in the run: a frame added or enabled
//! since is an error that asks for `align.run`. Each frame is read and developed as alignment read it, with the
//! stack's defect mask inpainted (`align::Plan`), its brightness matched to the master's with the gain alignment
//! measured, and handed to florastack-stack's stacker, then dropped: memory holds one frame and the accumulators,
//! whatever the depth of the stack.
//!
//! - **The full stack** (`stack.average`) warps each frame onto the canvas a tile at a time, as the stacker reads it
//!   ([`Warped`]: florastack-align's `warp_to_canvas`, bicubic, with the frame's coverage, so that in wide framing a
//!   frame adds nothing where it has no pixels), and RAW frames get the settings' demosaic. Its commit adds the output
//!   (`Edit::AddOutput`, one undo step): the stacked image in the cache, linear `f32` TIFF, with an alpha channel when
//!   some pixels are covered by no frame (only wide framing has them), and a small JPEG preview for lists, with the
//!   method's parameters and the frames it was made from.
//! - **The preview** (`stack.preview`) reduces each frame to a quarter of its size as soon as it is read
//!   (florastack-align's `thumbnail`, which the filmstrip uses too: block averages, then Lanczos3) and warps the small
//!   copy onto the quarter-size canvas as the full stack warps the frame (tile by tile, bicubic, with its feathered
//!   coverage), with the settings scaled to match (`Method::reduced`); RAW frames get the fast demosaic. It
//!   runs in the preview lane, beside a full stack, and a newer call cancels an older one still running, so that a
//!   preview follows every change of a setting. Its result is a lossless TIFF in `previews/`, which nothing in the
//!   project refers to: no undo step, and the file goes when the project is next closed or opened.

use std::borrow::Cow;

use florastack_align::{AlignError, Canvas, DEFAULT_FEATHER, Framing, Status, thumbnail, warp_to_canvas};
use florastack_core::{Affine, AnyImage, Image, Monitor, Rect};
use florastack_project::{Area, CacheDir, Edit, FrameId, Group, Input, Maps, NewOutput, OutputId, Preferences, Table};
use florastack_raw::Quality;
use florastack_stack::{
    AVERAGE, AverageSettings, FrameProgress, FrameSource, Gained, MAX_RADIUS, Method, PREVIEW_FACTOR, Patch, StackError, StackSettings, StackShape,
    Stacked, Tiling,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize};

use super::align::Plan;
use super::check_name;
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobContext, JobPlan, Lane, commit};
use crate::project::{align_settings, develop_record, stack_settings, stored_alignment, write_image, write_image_in, write_preview};
use crate::registry::{CommandSpec, Meta, NoParams, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("stack.get", "Stacking settings", "The stacking methods' settings.")
                .help(
                    "Every setting, with the keys the preferences don't set at their defaults: for the weighted average \
                     (`average`), the radius sharpness is measured over, the smoothing of the blend and the power that \
                     sets how strongly the sharpest frame wins.",
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
                .examples(&[r#"{ "average": { "radius": 2.0 } }"#, r#"{ "average": { "smoothing": 2.0, "power": 3.0 } }"#]),
            set,
        ),
        CommandSpec::instant(
            Meta::new("stack.reset", "Reset stacking settings", "Puts every stacking setting back at its default.").undoable(),
            reset,
        ),
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
            Meta::new("stack.preview", "Preview stack", "Stacks the aligned frames at a quarter of their size, as a quick preview.")
                .help(
                    "The weighted average of `stack.average`, on frames reduced to a quarter of their size on each side \
                     with the settings scaled to match, so that it shows in seconds what the full stack will look \
                     like. Takes the same settings. The result is a linear 32-bit float TIFF in the project's \
                     `previews/` folder, as a path, kept until the project is closed: no output, no undo step. Runs \
                     beside other jobs as a preview; a newer call replaces an older one still running, so call it \
                     after every change of a setting.",
                )
                .requires(Requires::Project)
                .supersede()
                .examples(&["{}", r#"{ "smoothing": 3.0 }"#]),
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

/// A preview of the weighted average.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PreviewParams {
    /// The radius sharpness is measured over, in pixels of the full-size frames, from 0 to 64; the project's
    /// (`stack.get`) when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 64.0))]
    radius: Option<f64>,
    /// How smoothly the blend moves from one frame to the next, in pixels of the full-size frames, from 0 to 64; the
    /// project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 64.0))]
    smoothing: Option<f64>,
    /// How strongly the sharpest frame wins, from 0.5 to 4; the project's when left out.
    #[serde(default)]
    #[schemars(range(min = 0.5, max = 4.0))]
    power: Option<f64>,
}

impl Params for PreviewParams {
    fn check(&self) -> Result<(), Invalid> {
        AverageParams { radius: self.radius, smoothing: self.smoothing, power: self.power }.check()
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
struct ToStack {
    index: usize,
    id: FrameId,
    transform: Affine,
    gain: f32,
}

/// What a stacking job reads, taken from the project when it starts.
struct StackPlan {
    command: &'static str,
    reader: Plan,
    frames: Vec<ToStack>,
    /// Enabled frames left out: their alignment was rejected.
    skipped: Vec<FrameId>,
    canvas: Canvas,
    framing: Framing,
    /// The frames' size, which the transforms are in.
    size: (usize, usize),
    settings: AverageSettings,
    cache: CacheDir,
}

/// The gain as the frames' samples take it.
#[allow(clippy::cast_possible_truncation)] // A gain near 1.
fn gain(g: f64) -> f32 {
    if g.is_finite() && g > 0.0 { g as f32 } else { 1.0 }
}

impl StackPlan {
    fn new(session: &Session, command: &'static str, params: &AverageParams, quality: Quality) -> Result<StackPlan, CommandError> {
        let project = session.project()?;
        let settings = params.settings(&stack_settings(project)?.average);
        settings.validate()?;
        let Some(stored) = stored_alignment(project)? else {
            return Err(session.unavailable("the stack isn't aligned: align it first with `align.run`"));
        };
        let framing = align_settings(project)?.framing;
        let canvas = stored.canvas(framing)?;
        let reader = Plan::new(session, command, 1)?.with_quality(quality);
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
        })
    }

    /// Reads the `k`-th frame to stack, developed and inpainted, and checks its size against the alignment's.
    fn read(&mut self, k: usize) -> Result<Image<f32>, CommandError> {
        let Some(f) = self.frames.get(k) else { return Err(CommandError::Internal { command: self.command.to_owned() }) };
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

    /// Stacks every frame, at full size or, `reduced`, at a quarter of it. Each frame is read, folded in and dropped.
    fn stack(&mut self, ctx: &JobContext, reduced: bool) -> Result<Stacked, CommandError> {
        ctx.stage(0);
        self.reader.load_mask()?;
        let settings = if reduced { self.settings.reduced(PREVIEW_FACTOR as f64) } else { self.settings.clone() };
        let mut stacker = settings.stacker(&Tiling::default())?;
        let n = self.frames.len();
        let (w, h) = self.size;
        let side = w.max(h).div_ceil(PREVIEW_FACTOR);
        for k in 0..n {
            ctx.checkpoint()?;
            let image = self.read(k)?;
            let Some(f) = self.frames.get(k) else { break };
            let progress = FrameProgress { inner: ctx, frame: k as u64, frames: n as u64 };
            if reduced {
                let small = thumbnail(&image, side)?;
                drop(image);
                let (canvas, transform, feather) = reduced_geometry(&self.canvas, &f.transform, (w, h), (small.width(), small.height()));
                let source = Gained { frame: Warped { frame: &small, transform, canvas, feather }, gain: f.gain };
                if k == 0 {
                    stacker.begin(StackShape::of(&source))?;
                }
                stacker.add_frame(&source, &progress)?;
            } else {
                let source =
                    Gained { frame: Warped { frame: &image, transform: f.transform, canvas: self.canvas, feather: DEFAULT_FEATHER }, gain: f.gain };
                if k == 0 {
                    stacker.begin(StackShape::of(&source))?;
                }
                stacker.add_frame(&source, &progress)?;
            }
        }
        ctx.stage(1);
        Ok(stacker.finish(ctx)?)
    }

    fn ids(&self) -> Vec<FrameId> {
        self.frames.iter().map(|f| f.id).collect()
    }
}

/// The stacked image with its alpha, if it has one: an extra channel for gray and color images, the alpha channel they
/// have multiplied by it otherwise.
fn with_alpha(stacked: &Stacked) -> Result<Image<f32>, CommandError> {
    let image = &stacked.image;
    let Some(alpha) = &stacked.alpha else { return Ok(image.clone()) };
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
    settings: AverageSettings,
    framing: Framing,
}

/// What stacking reads: the frame list, the development, the alignment (and the defect mask). The settings aren't
/// locked: a stack runs with the settings it started with, so that they can change, and previews follow, meanwhile.
fn locks() -> florastack_project::Aspects {
    florastack_project::Aspects::FRAMES.union(florastack_project::Aspects::DEVELOP).union(florastack_project::Aspects::ALIGN)
}

fn average(session: &Session, p: StackParams) -> Result<JobPlan<StackSummary>, CommandError> {
    let overrides = AverageParams { radius: p.radius, smoothing: p.smoothing, power: p.power };
    let mut plan = StackPlan::new(session, "stack.average", &overrides, Quality::Output)?;
    let name = p.name.unwrap_or_else(|| "Weighted average".to_owned());
    let job = JobPlan::new(move |ctx| {
        let stacked = plan.stack(ctx, false)?;
        ctx.checkpoint()?;
        let image = with_alpha(&stacked)?;
        let reference = write_image(&plan.cache, AVERAGE, &AnyImage::F32(image), None)?;
        let preview = write_preview(&plan.cache, AVERAGE, &stacked.image, None)?;
        let develop = match plan.reader.resolved_develop() {
            Some((frame, develop)) => Some(develop_record(frame, develop)?),
            None => None,
        };
        let ids = plan.ids();
        let parameters = serde_json::json!({
            "radius": plan.settings.radius,
            "smoothing": plan.settings.smoothing,
            "power": plan.settings.power,
            "framing": plan.framing,
        });
        let (width, height, uncovered) = (stacked.image.width(), stacked.image.height(), stacked.uncovered);
        drop(stacked);
        let (skipped, settings, framing) = (plan.skipped.clone(), plan.settings.clone(), plan.framing);
        Ok(commit(move |session: &mut Session| {
            let mut edits = Vec::new();
            if let Some(develop) = develop {
                edits.push(Edit::SetDevelop(Some(develop)));
            }
            edits.push(Edit::AddOutput(Box::new(NewOutput {
                name,
                method: AVERAGE.to_owned(),
                parameters,
                inputs: ids.iter().copied().map(Input::Frame).collect(),
                image: reference,
                preview: Some(preview),
                maps: Maps::new(),
            })));
            session.apply(Edit::Batch(edits))?;
            let output = session
                .project()?
                .outputs
                .last()
                .map(florastack_project::Output::id)
                .ok_or(CommandError::Internal { command: "stack.average".to_owned() })?;
            Ok(StackSummary { output, width, height, frames: ids, skipped, uncovered, settings, framing })
        }))
    });
    Ok(job.stages(&[("Stacking frames", 0.95), ("Finishing", 0.05)]).locks(locks()))
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
    /// The settings at full scale, and the framing.
    settings: AverageSettings,
    framing: Framing,
}

fn preview(session: &Session, p: PreviewParams) -> Result<JobPlan<Previewed>, CommandError> {
    let overrides = AverageParams { radius: p.radius, smoothing: p.smoothing, power: p.power };
    let mut plan = StackPlan::new(session, "stack.preview", &overrides, Quality::Preview)?;
    let job = JobPlan::new(move |ctx| {
        let stacked = plan.stack(ctx, true)?;
        ctx.checkpoint()?;
        let reference = write_image_in(&plan.cache, Area::Previews, "stack-preview", &AnyImage::F32(with_alpha(&stacked)?), None)?;
        let previewed = Previewed {
            path: reference.path.resolve(plan.cache.path()).display().to_string(),
            width: stacked.image.width(),
            height: stacked.image.height(),
            frames: plan.ids(),
            skipped: plan.skipped.clone(),
            uncovered: stacked.uncovered,
            settings: plan.settings.clone(),
            framing: plan.framing,
        };
        Ok(commit(move |_: &mut Session| Ok(previewed)))
    });
    Ok(job.stages(&[("Stacking frames", 0.95), ("Finishing", 0.05)]))
}
