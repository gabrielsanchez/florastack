//! Adjusting an output (`adjust.*`): its adjustments and masks, each change one undo step, and the adjust session
//! that previews them (`adjust.open`, `adjust.preview`, `adjust.close`). What is stored and how it is rendered are in
//! `crate::adjust`.

use std::sync::Arc;

use florastack_core::{AnyImage, Image, Monitor};
use florastack_edit::{Adjustments, Component, Mask, Op, Request, Shape, Size, Stroke};
use florastack_project::{Area, CacheDir, CacheRef, Edit, Output, OutputId};
use florastack_raw::Quality;
use florastack_stack::{DepthSettings, PREVIEW_FACTOR, StackShape};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::stack::{Chosen, StackPlan, Units, Warping, each};
use crate::adjust::{AdjustChanges, AdjustSession, FOCUS_DEPTH, adjustments, renderer};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::registry::{CommandSpec, Meta, NoParams, Params, Requires};
use crate::session::Session;

/// The largest preview side, in pixels.
const MAX_PREVIEW_SIDE: usize = 16384;

#[allow(clippy::too_many_lines)] // The commands' help texts and examples.
pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::query(
            Meta::new("adjust.get", "Adjustments", "An output's adjustments: the whole image's and its masks'.")
                .help(
                    "Every setting is listed, neutral ones included; `neutral` says whether they change anything. The \
                     whole image's (`global`) are applied first, then each visible mask's (`masks`) where it selects.",
                )
                .requires(Requires::Project)
                .examples(&[r#"{ "output": 3 }"#]),
            get,
        ),
        CommandSpec::instant(
            Meta::new("adjust.set", "Set adjustments", "Replaces an output's adjustments, the whole image's and its masks', in one undo step.")
                .help(
                    "Settings left out take their neutral defaults. Sliders run from -100 to 100 (0 neutral) unless their \
                     description says otherwise: exposure in stops from -5 to 5, sharpening from 0 to 150, noise \
                     reduction from 0 to 100. Masks keep their ids; a new mask's id must be one no mask of the output \
                     has. Adjustments apply after the output's retouch strokes, to exports too.",
                )
                .undoable()
                .examples(&[
                    r#"{ "output": 3, "adjustments": { "global": { "exposure": 0.3, "clarity": 20, "vibrance": 15 } } }"#,
                    r#"{ "output": 3, "adjustments": { "masks": [ { "id": 1, "name": "Sky", "components": [ { "shape": { "kind": "linear", "start": [0.5, 0], "end": [0.5, 0.4] } } ], "adjust": { "exposure": -0.5 } } ] } }"#,
                ]),
            set,
        ),
        CommandSpec::instant(
            Meta::new("adjust.reset", "Reset adjustments", "Removes an output's adjustments and masks, in one undo step.")
                .undoable()
                .examples(&[r#"{ "output": 3 }"#]),
            reset,
        ),
        CommandSpec::instant(
            Meta::new("adjust.addMask", "Add mask", "Adds a mask on top of an output's, as one undo step.")
                .help(
                    "The mask is given a new id, which the result returns; an `id` given is ignored. Its components are \
                     combined in order: the first one's region, then each next one added (`add`), taken away \
                     (`subtract`) or intersected (`intersect`). Shapes: `whole`, `linear` (`start`, `end`), `radial` \
                     (`center`, `radius_x`, `radius_y`, `angle`, `feather`), `brush` (`strokes`), `luminance` (`low`, \
                     `high`, `low_feather`, `high_feather`), `color` (`samples`, from `adjust.sampleColor`, and \
                     `refine`), and from the stack's focus `subject`, `background` and `depth` (`from`, `to`, \
                     `feather`). Coordinates are fractions of the output's width and height.",
                )
                .undoable()
                .examples(&[
                    r#"{ "output": 3, "mask": { "name": "Center", "components": [ { "shape": { "kind": "radial", "center": [0.5, 0.5], "radius_x": 0.25, "radius_y": 0.2, "angle": 0, "feather": 60 } } ], "adjust": { "exposure": 0.4 } } }"#,
                    r#"{ "output": 3, "mask": { "components": [ { "shape": { "kind": "whole" } } ], "adjust": { "saturation": -30 } } }"#,
                ]),
            add_mask,
        ),
        CommandSpec::instant(
            Meta::new("adjust.setMask", "Set mask", "Replaces one of an output's masks, the one with the same id, in one undo step.")
                .undoable()
                .examples(&[
                    r#"{ "output": 3, "mask": { "id": 1, "name": "Center", "visible": false, "components": [ { "shape": { "kind": "whole" } } ], "adjust": { "clarity": 30 } } }"#,
                ]),
            set_mask,
        ),
        CommandSpec::instant(
            Meta::new("adjust.removeMask", "Remove mask", "Removes one of an output's masks, in one undo step.")
                .undoable()
                .examples(&[r#"{ "output": 3, "mask": 1 }"#]),
            remove_mask,
        ),
        CommandSpec::instant(
            Meta::new("adjust.moveMask", "Move mask", "Moves one of an output's masks to another place in their order, in one undo step.")
                .help("Masks apply in order, the first one first; `index` 0 is the first place, and an index past the end the last.")
                .undoable()
                .examples(&[r#"{ "output": 3, "mask": 2, "index": 0 }"#]),
            move_mask,
        ),
        CommandSpec::instant(
            Meta::new("adjust.stroke", "Brush stroke", "Paints a brush stroke onto a mask, as one undo step.")
                .help(
                    "The stroke goes on top of the mask's brush component `component` (an index into its components), or \
                     of its last brush component, or of a new one added when it has none. Its `points` are fractions \
                     of the output's width and height, its `size` a fraction of its long edge; `erase` takes away what \
                     earlier strokes of the component painted.",
                )
                .undoable()
                .examples(&[
                    r#"{ "output": 3, "mask": 1, "stroke": { "points": [[0.2, 0.3], [0.25, 0.32], [0.3, 0.36]], "size": 0.04, "feather": 60, "flow": 100 } }"#,
                    r#"{ "output": 3, "mask": 1, "stroke": { "points": [[0.22, 0.31]], "size": 0.02, "erase": true }, "component": 0 }"#,
                ]),
            stroke,
        ),
        CommandSpec::job(
            Meta::new("adjust.open", "Open for adjusting", "Opens an output for adjusting, so that `adjust.preview` can show its adjustments.")
                .help(
                    "Reads the output with its visible retouch strokes (the output open for retouching as it is, or the \
                     result cached) and keeps it in memory, ready to render. One output is open for adjusting at a time; \
                     opening another closes the one open. Runs beside other jobs.",
                )
                .requires(Requires::Project)
                .examples(&[r#"{ "output": 3 }"#]),
            Lane::Preview,
            open,
        ),
        CommandSpec::job(
            Meta::new("adjust.mapDepth", "Map the stack's depth", "Measures the depth of the output open for adjusting from its frames' focus, for its depth masks.")
                .help(
                    "Measures the output's frames' focus at a quarter of their size, as the depth-map method's first pass \
                     does, and keeps the depth map it makes as the output's `focus-depth` map: at each point, which frame \
                     was in focus there. A depth-map output needs none: its own depth map tells. Exports that need it and \
                     find none measure it themselves. The subject and background masks need no map: the subject is found \
                     in the output itself. Runs beside other jobs.",
                )
                .requires(Requires::Project),
            Lane::Preview,
            map_depth,
        ),
        CommandSpec::instant(
            Meta::new("adjust.close", "Close adjusting", "Closes the output open for adjusting.").requires(Requires::Project),
            close,
        ),
        CommandSpec::job(
            Meta::new("adjust.preview", "Preview adjustments", "Renders the output open for adjusting with its adjustments, reduced to fit a view.")
                .help(
                    "The preview fits in `width` by `height` pixels (never enlarged). `draft` renders quickly while a \
                     slider moves, skipping the most expensive refinements; `overlay` shows a mask's region tinted red; \
                     `before` also renders the output without its adjustments at the same size, for comparing. \
                     Apps draw the result; the job's result gives its size. Runs beside other jobs; a newer call \
                     replaces one still running.",
                )
                .requires(Requires::Project)
                .supersede()
                .examples(&[
                    r#"{ "width": 1600, "height": 1000 }"#,
                    r#"{ "width": 800, "height": 600, "draft": true, "overlay": 2 }"#,
                    r#"{ "width": 1600, "height": 1000, "before": true }"#,
                ]),
            Lane::Preview,
            preview,
        ),
        CommandSpec::query(
            Meta::new("adjust.sampleColor", "Sample color", "The color of the output open for adjusting at a point, for a color-range mask.")
                .help(
                    "`x` and `y` are fractions of the output's width and height. The color is in `OkLab`, after the whole \
                     image's adjustments, averaged over a few pixels: what a `color` shape's `samples` hold. `null` \
                     outside the image.",
                )
                .requires(Requires::Project)
                .examples(&[r#"{ "x": 0.42, "y": 0.6 }"#]),
            sample_color,
        ),
    ]
}

/// An output.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OutputParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
}

impl Params for OutputParams {}

/// An output's adjustments.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct AdjustInfo {
    output: OutputId,
    adjustments: Adjustments,
    /// Whether they leave the output as it is.
    neutral: bool,
}

impl AdjustInfo {
    fn new(output: OutputId, adjustments: Adjustments) -> AdjustInfo {
        let neutral = adjustments.is_neutral();
        AdjustInfo { output, adjustments, neutral }
    }
}

fn find(session: &Session, id: OutputId) -> Result<&florastack_project::Output, CommandError> {
    session.project()?.output(id).ok_or(CommandError::NotFound { what: id.to_string() })
}

/// Stores `adjustments` as output `id`'s, as one undo step: none at all when they are the defaults.
fn store(session: &mut Session, id: OutputId, adjustments: &Adjustments) -> Result<(), CommandError> {
    let value = if *adjustments == Adjustments::default() { None } else { Some(florastack_project::to_data("adjustments", adjustments)?) };
    if find(session, id)?.adjustments != value {
        session.apply(Edit::SetAdjustments { output: id, adjustments: value })?;
    }
    Ok(())
}

fn invalid(e: &florastack_edit::EditError) -> Invalid {
    match e {
        florastack_edit::EditError::Invalid { reason } => Invalid::new("adjustments", reason.clone()),
        other => Invalid::new("adjustments", other.to_string()),
    }
}

fn get(session: &Session, p: OutputParams) -> Result<AdjustInfo, CommandError> {
    Ok(AdjustInfo::new(p.output, adjustments(find(session, p.output)?)?))
}

/// An output and its new adjustments.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
    /// The adjustments: the whole image's (`global`) and the masks (`masks`). What is left out is neutral.
    adjustments: Adjustments,
}

impl Params for SetParams {
    fn check(&self) -> Result<(), Invalid> {
        self.adjustments.validate().map_err(|e| invalid(&e))
    }
}

fn set(session: &mut Session, p: SetParams) -> Result<AdjustInfo, CommandError> {
    find(session, p.output)?;
    store(session, p.output, &p.adjustments)?;
    Ok(AdjustInfo::new(p.output, p.adjustments))
}

fn reset(session: &mut Session, p: OutputParams) -> Result<AdjustInfo, CommandError> {
    find(session, p.output)?;
    store(session, p.output, &Adjustments::default())?;
    Ok(AdjustInfo::new(p.output, Adjustments::default()))
}

/// An output and a mask.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaskParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
    /// The mask: its name, components and adjustments.
    mask: Mask,
}

impl Params for MaskParams {
    fn check(&self) -> Result<(), Invalid> {
        Adjustments { masks: vec![self.mask.clone()], ..Adjustments::default() }.validate().map_err(|e| invalid(&e))
    }
}

/// The mask added or changed, and the output's adjustments after it.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct MaskChanged {
    /// The mask's id.
    mask: u32,
    #[serde(flatten)]
    info: AdjustInfo,
}

/// The output's adjustments with `change` made to them, checked and stored as one undo step.
fn change(
    session: &mut Session,
    id: OutputId,
    change: impl FnOnce(&mut Adjustments) -> Result<(), CommandError>,
) -> Result<Adjustments, CommandError> {
    let mut adjustments = adjustments(find(session, id)?)?;
    change(&mut adjustments)?;
    adjustments.validate().map_err(|e| {
        let Invalid { field, reason } = invalid(&e);
        session.invalid(field, reason)
    })?;
    store(session, id, &adjustments)?;
    Ok(adjustments)
}

fn add_mask(session: &mut Session, p: MaskParams) -> Result<MaskChanged, CommandError> {
    let mut id = 0;
    let adjustments = change(session, p.output, |a| {
        id = a.next_mask_id();
        a.masks.push(Mask { id, ..p.mask });
        Ok(())
    })?;
    Ok(MaskChanged { mask: id, info: AdjustInfo::new(p.output, adjustments) })
}

fn no_mask(output: OutputId, mask: u32) -> CommandError {
    CommandError::NotFound { what: format!("mask {mask} on output {output}") }
}

fn set_mask(session: &mut Session, p: MaskParams) -> Result<MaskChanged, CommandError> {
    let id = p.mask.id;
    let adjustments = change(session, p.output, |a| {
        let mask = a.mask_mut(id).ok_or_else(|| no_mask(p.output, id))?;
        *mask = p.mask;
        Ok(())
    })?;
    Ok(MaskChanged { mask: id, info: AdjustInfo::new(p.output, adjustments) })
}

/// An output and one of its masks.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MaskIdParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
    /// The mask's id, as `adjust.get` lists it.
    mask: u32,
}

impl Params for MaskIdParams {}

fn remove_mask(session: &mut Session, p: MaskIdParams) -> Result<MaskChanged, CommandError> {
    let adjustments = change(session, p.output, |a| {
        let index = a.masks.iter().position(|m| m.id == p.mask).ok_or_else(|| no_mask(p.output, p.mask))?;
        a.masks.remove(index);
        Ok(())
    })?;
    Ok(MaskChanged { mask: p.mask, info: AdjustInfo::new(p.output, adjustments) })
}

/// An output, one of its masks and its new place.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MoveParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
    /// The mask's id, as `adjust.get` lists it.
    mask: u32,
    /// Its new place: 0 is the first, and an index past the end the last.
    index: usize,
}

impl Params for MoveParams {}

fn move_mask(session: &mut Session, p: MoveParams) -> Result<MaskChanged, CommandError> {
    let adjustments = change(session, p.output, |a| {
        let from = a.masks.iter().position(|m| m.id == p.mask).ok_or_else(|| no_mask(p.output, p.mask))?;
        let mask = a.masks.remove(from);
        a.masks.insert(p.index.min(a.masks.len()), mask);
        Ok(())
    })?;
    Ok(MaskChanged { mask: p.mask, info: AdjustInfo::new(p.output, adjustments) })
}

/// A stroke and the mask it goes onto.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StrokeParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
    /// The mask's id, as `adjust.get` lists it.
    mask: u32,
    /// The stroke.
    stroke: Stroke,
    /// The brush component it goes onto, by its index among the mask's components; its last brush component when
    /// left out, or a new one when it has none.
    #[serde(default)]
    component: Option<usize>,
}

impl Params for StrokeParams {
    fn check(&self) -> Result<(), Invalid> {
        let shape = Shape::Brush { strokes: vec![self.stroke.clone()] };
        let mask = Mask { components: vec![Component { op: Op::Add, invert: false, shape }], ..Mask::default() };
        Adjustments { masks: vec![mask], ..Adjustments::default() }.validate().map_err(|e| invalid(&e))
    }
}

/// The stroke painted.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Stroked {
    /// The mask's id, and the index of the component the stroke went onto.
    mask: u32,
    component: usize,
    /// How many strokes the component has now.
    strokes: usize,
}

fn stroke(session: &mut Session, p: StrokeParams) -> Result<Stroked, CommandError> {
    let (mut component, mut strokes) = (0, 0);
    change(session, p.output, |a| {
        let mask = a.mask_mut(p.mask).ok_or_else(|| no_mask(p.output, p.mask))?;
        let brush = |c: &Component| matches!(c.shape, Shape::Brush { .. });
        let index = match p.component {
            Some(i) if mask.components.get(i).is_some_and(brush) => i,
            Some(i) => return Err(CommandError::NotFound { what: format!("brush component {i} of mask {}", p.mask) }),
            None => {
                if let Some(i) = mask.components.iter().rposition(brush) {
                    i
                } else {
                    mask.components.push(Component { op: Op::Add, invert: false, shape: Shape::Brush { strokes: Vec::new() } });
                    mask.components.len() - 1
                }
            }
        };
        if let Some(Component { shape: Shape::Brush { strokes: list }, .. }) = mask.components.get_mut(index) {
            list.push(p.stroke);
            (component, strokes) = (index, list.len());
        }
        Ok(())
    })?;
    Ok(Stroked { mask: p.mask, component, strokes })
}

/// Where an output's depth map comes from: the map it has, or its frames, measured. See `crate::adjust`.
pub(super) enum DepthSource {
    /// A depth map: 0 to 65535 over the frames, a 16-bit gray image.
    Map(CacheRef),
    /// The output's frames, to measure at a quarter of their size.
    Measure(Box<Measuring>),
}

/// What measuring an output's depth needs.
pub(super) struct Measuring {
    plan: StackPlan,
    depth: DepthSettings,
    output: OutputId,
}

impl DepthSource {
    /// Where the output's depth map comes from, for `command`.
    pub(super) fn of(session: &Session, output: &Output, command: &'static str) -> Result<DepthSource, CommandError> {
        let map = |name: &str| crate::retouch::map_name(name).ok().and_then(|n| output.maps.get(&n).cloned());
        if output.method == "depth-map"
            && let Some(depth) = map("depth")
        {
            return Ok(DepthSource::Map(depth));
        }
        if let Some(depth) = map(FOCUS_DEPTH) {
            return Ok(DepthSource::Map(depth));
        }
        let project = session.project()?;
        let frames = crate::retouch::output_frames(project, output);
        if frames.is_empty() {
            return Err(session.unavailable("the output has no frames to measure the depth of"));
        }
        let depth = crate::project::stack_settings(project)?.depth_map;
        let plan = StackPlan::of_frames(
            session,
            command,
            Chosen::DepthMap(depth.clone()),
            Quality::Preview,
            &frames,
            crate::retouch::output_framing(output),
        )?;
        Ok(DepthSource::Measure(Box::new(Measuring { plan, depth, output: output.id() })))
    }

    /// The depth map, read or measured for an output of `size`, with the map newly written to the cache when it was
    /// measured.
    pub(super) fn read(self, cache: &CacheDir, ctx: &dyn Monitor, size: (usize, usize)) -> Result<(Image<u16>, Option<CacheRef>), CommandError> {
        match self {
            DepthSource::Map(depth) => Ok((read_u16(cache, &depth)?, None)),
            DepthSource::Measure(m) => {
                let Measuring { mut plan, depth, output } = *m;
                if (plan.canvas.width, plan.canvas.height) != size {
                    return Err(CommandError::Unavailable {
                        command: "adjust".to_owned(),
                        reason: "the frames, as aligned now, don't make this output's canvas, so its depth can't be measured: they were aligned or framed again since it was stacked".to_owned(),
                    });
                }
                plan.reader.load_mask()?;
                let n = plan.frames.len();
                let mut units = Units::new(n);
                #[allow(clippy::cast_precision_loss)] // A small constant.
                let reduced = depth.reduced(PREVIEW_FACTOR as f64);
                let mut estimator = reduced.estimator(&plan.tiling)?;
                let mut feed = Warping { plan: &mut plan, range: 0..n, reduced: true };
                each(&mut feed, ctx, &mut units, |loaded, progress, first| {
                    loaded.source(|source| {
                        if first {
                            estimator.begin(StackShape::of(source))?;
                        }
                        Ok::<_, CommandError>(estimator.add_frame(source, progress)?)
                    })
                })?;
                let stats = estimator.finish(ctx)?;
                let map = stats.resolve(reduced.contrast_threshold(), reduced.smoothing, None)?.depth_u16()?;
                let name = format!("{FOCUS_DEPTH}-{}", output.get());
                let written = crate::project::write_image_in(&plan.cache, Area::Cache, &name, &AnyImage::U16(map.clone()), None)?;
                Ok((map, Some(written)))
            }
        }
    }
}

/// A 16-bit gray map from the cache.
fn read_u16(cache: &CacheDir, reference: &CacheRef) -> Result<Image<u16>, CommandError> {
    match crate::project::read_image(cache, reference)? {
        AnyImage::U16(image) if image.channels() == 1 => Ok(image),
        other => Err(CommandError::Unavailable {
            command: "adjust".to_owned(),
            reason: format!("a depth map has {} channels of {:?} samples, not one of 16 bits", other.channels(), other.format()),
        }),
    }
}

/// The depth mapped.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct DepthMapped {
    output: OutputId,
    /// The map's size, a quarter of the output's for a measured one.
    width: usize,
    height: usize,
    /// Whether the frames were measured (`false`: the output's map was there already).
    measured: bool,
}

fn map_depth(session: &Session, _: NoParams) -> Result<JobPlan<DepthMapped>, CommandError> {
    let open = open_session(session)?;
    let id = open.output;
    let size = (open.renderer.width(), open.renderer.height());
    let output = find(session, id)?;
    let source = DepthSource::of(session, output, "adjust.mapDepth")?;
    let measured = matches!(source, DepthSource::Measure(_));
    let renderer = Arc::clone(&open.renderer);
    let cache = session.cache_dir()?;
    let job = JobPlan::new(move |ctx| {
        let (map, written) = source.read(&cache, ctx, size)?;
        ctx.stage(1);
        let with_depth = Arc::new(renderer.with_depth(&crate::adjust::depth(&map)?)?);
        let mapped = DepthMapped { output: id, width: map.width(), height: map.height(), measured };
        Ok(commit(move |session: &mut Session| {
            if let Some(depth) = written {
                session.apply_system(Edit::SetOutputMap { output: id, name: crate::retouch::map_name(FOCUS_DEPTH)?, map: Some(depth) })?;
            }
            if let Some(open) = session.adjust.as_mut().filter(|s| s.output == id) {
                open.renderer = with_depth;
            }
            Ok(mapped)
        }))
    });
    Ok(job.stages(&[("Measuring focus", 0.95), ("Mapping", 0.05)]))
}

fn open_session(session: &Session) -> Result<&AdjustSession, CommandError> {
    session.adjust.as_ref().ok_or_else(|| session.unavailable("no output is open for adjusting: open one with `adjust.open`"))
}

/// The output opened.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Opened {
    output: OutputId,
    width: usize,
    height: usize,
    /// Whether the stack's depth map is known, for the depth masks.
    depth: bool,
}

fn open(session: &Session, p: OutputParams) -> Result<JobPlan<Opened>, CommandError> {
    let output = find(session, p.output)?;
    let base = crate::retouch::render_key(output, &crate::retouch::visible_strokes(output));
    let mut notes = Vec::new();
    let pixels = super::output::Pixels::base(session, p.output, &mut notes)?;
    let map = match DepthSource::of(session, output, "adjust.open") {
        Ok(DepthSource::Map(depth)) => Some(depth),
        _ => None,
    };
    let cache = session.cache_dir()?;
    let id = p.output;
    let job = JobPlan::new(move |ctx| {
        let result = pixels.read(&cache, &mut notes)?;
        for note in &notes {
            tracing::info!("adjust.open: {note}");
        }
        ctx.checkpoint()?;
        let (renderer, working) = renderer(result)?;
        let renderer = match map.map(|d| read_u16(&cache, &d).and_then(|m| crate::adjust::depth(&m))) {
            Some(Ok(depth)) => renderer.with_depth(&depth)?,
            Some(Err(e)) => {
                tracing::warn!("the output's depth map can't be read, so its depth masks wait for `adjust.mapDepth`: {e}");
                renderer
            }
            None => renderer,
        };
        let opened = Opened { output: id, width: renderer.width(), height: renderer.height(), depth: renderer.has_depth() };
        Ok(commit(move |session: &mut Session| {
            session.adjust = Some(AdjustSession {
                output: id,
                renderer: Arc::new(renderer),
                working: Arc::new(working),
                base,
                preview: None,
                before: None,
                shown: None,
                changes: AdjustChanges { opened: true, ..AdjustChanges::default() },
            });
            Ok(opened)
        }))
    });
    Ok(job.stages(&[("Reading", 1.0)]))
}

/// Adjusting closed.
#[derive(Debug, Serialize, JsonSchema)]
struct Closed {
    /// Whether an output was open for adjusting.
    closed: bool,
}

fn close(session: &mut Session, _: NoParams) -> Result<Closed, CommandError> {
    let closed = session.adjust.take().is_some();
    session.adjust_closed |= closed;
    Ok(Closed { closed })
}

/// What to preview.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PreviewParams {
    /// The largest width, in pixels, from 1 to 16384.
    #[schemars(range(min = 1, max = 16384))]
    width: usize,
    /// The largest height, in pixels, from 1 to 16384.
    #[schemars(range(min = 1, max = 16384))]
    height: usize,
    /// A quick render while a slider moves.
    #[serde(default)]
    draft: bool,
    /// A mask whose region is shown tinted red, by id.
    #[serde(default)]
    overlay: Option<u32>,
    /// Also renders the output without its adjustments, at the same size, for comparing.
    #[serde(default)]
    before: bool,
}

impl Params for PreviewParams {
    fn check(&self) -> Result<(), Invalid> {
        for (field, v) in [("width", self.width), ("height", self.height)] {
            if !(1..=MAX_PREVIEW_SIDE).contains(&v) {
                return Err(Invalid::new(field, format!("must be from 1 to {MAX_PREVIEW_SIDE} pixels")));
            }
        }
        Ok(())
    }
}

/// The preview rendered.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Previewed {
    output: OutputId,
    width: usize,
    height: usize,
}

fn preview(session: &Session, p: PreviewParams) -> Result<JobPlan<Previewed>, CommandError> {
    let open = open_session(session)?;
    let output = find(session, open.output)?;
    let current = crate::retouch::render_key(output, &crate::retouch::visible_strokes(output));
    if current != open.base {
        return Err(session.unavailable("the output's retouch strokes changed since it was opened for adjusting: open it again with `adjust.open`"));
    }
    let adjustments = adjustments(output)?;
    let (renderer, working, id) = (Arc::clone(&open.renderer), Arc::clone(&open.working), open.output);
    let request = Request { size: Size::Fit { width: p.width, height: p.height }, draft: p.draft, overlay: p.overlay };
    let with_before = p.before;
    let job = JobPlan::new(move |ctx| {
        let shown = |adjustments: &Adjustments, request: &Request| -> Result<crate::view::DisplayImage, CommandError> {
            let mut image = renderer.render(adjustments, request)?;
            working.back(&mut image)?;
            crate::view::display(&image, None).map_err(|e| CommandError::Unavailable { command: "adjust.preview".to_owned(), reason: e.to_string() })
        };
        let display = shown(&adjustments, &request)?;
        ctx.checkpoint()?;
        let before = if with_before { Some(Arc::new(shown(&Adjustments::default(), &Request { overlay: None, ..request })?)) } else { None };
        let previewed = Previewed { output: id, width: display.width, height: display.height };
        Ok(commit(move |session: &mut Session| {
            if let Some(open) = session.adjust.as_mut().filter(|s| s.output == id) {
                open.preview = Some(Arc::new(display));
                if before.is_some() {
                    open.before = before;
                }
                open.shown = Some(adjustments);
                open.changes.preview = true;
            }
            Ok(previewed)
        }))
    });
    Ok(job.stages(&[("Rendering", 1.0)]))
}

/// A point of the output, as fractions of its width and height.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PointParams {
    /// From 0 (the left edge) to 1 (the right edge).
    #[schemars(range(min = -10.0, max = 10.0))]
    x: f64,
    /// From 0 (the top edge) to 1 (the bottom edge).
    #[schemars(range(min = -10.0, max = 10.0))]
    y: f64,
}

impl Params for PointParams {
    fn check(&self) -> Result<(), Invalid> {
        for (field, v) in [("x", self.x), ("y", self.y)] {
            if !(v.is_finite() && (-10.0..=10.0).contains(&v)) {
                return Err(Invalid::new(field, "must be a fraction of the output's size, from -10 to 10"));
            }
        }
        Ok(())
    }
}

/// A color, in `OkLab`.
#[derive(Debug, Serialize, JsonSchema)]
struct Sampled {
    /// Lightness, then the green-red and blue-yellow axes; `null` outside the image.
    color: Option<[f64; 3]>,
}

fn sample_color(session: &Session, p: PointParams) -> Result<Sampled, CommandError> {
    let open = open_session(session)?;
    let adjustments = adjustments(find(session, open.output)?)?;
    Ok(Sampled { color: open.renderer.sample_color(&adjustments, [p.x, p.y])? })
}
