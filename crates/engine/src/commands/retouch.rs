//! Retouching an output (`retouch.*`): opening it for retouching and closing it, the source strokes paint from, the
//! sharpest frame at a point and the map that tells it, painting a stroke live and committing it as one undo step, and
//! the output's layers. The session itself, and why replay is exact, are in `crate::retouch`.

use std::sync::Arc;

use florastack_core::{AnyImage, Image, Monitor};
use florastack_project::{Area, CacheRef, Edit, FrameId, LayerId, OutputId};
use florastack_raw::Quality;
use florastack_retouch::{Fields, LiveStroke, Painted, Source, Stroke, union};
use florastack_stack::{PREVIEW_FACTOR, StackShape};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::check_name;
use super::stack::{Chosen, StackPlan, Units, Warping, each};
use crate::error::{CommandError, Invalid};
use crate::jobs::{JobPlan, Lane, commit};
use crate::project::{stack_settings, write_image_in};
use crate::registry::{CommandSpec, Meta, NoParams, Params, Requires};
use crate::retouch::{
    NO_FRAME, RETOUCHED, RetouchChanges, RetouchSession, SHARPEST, Sharpest, SourceLoader, load_sources, map_name, output_frames, output_framing,
    paintable, painted, read_f32, render_key, visible_strokes,
};
use crate::session::Session;

#[allow(clippy::too_many_lines)] // The commands' help texts and examples.
pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::job(
            Meta::new("retouch.open", "Open for retouching", "Opens an output for retouching, with its visible strokes painted.")
                .help(
                    "Reads the output and paints its visible retouch strokes over it, or reads the result cached when the \
                     project was last saved, and the map of each point's sharpest frame when the output has one (a \
                     depth map's own, or `retouch.mapFocus`'s). Then `retouch.setSource` picks what strokes paint from, \
                     `retouch.stroke` paints one, and `edit.undo` takes it back. One output is open for retouching at a \
                     time; opening another caches and closes the one open.",
                )
                .requires(Requires::Project)
                .examples(&[r#"{ "output": 3 }"#]),
            Lane::Heavy,
            open,
        ),
        CommandSpec::instant(
            Meta::new("retouch.close", "Close retouching", "Closes the output open for retouching, caching its result.")
                .help("The result is cached as the output's `retouched` map, so that opening it again needs no recomputation.")
                .requires(Requires::Project),
            close,
        ),
        CommandSpec::job(
            Meta::new("retouch.setSource", "Set retouch source", "Picks the frame or output that strokes paint from.")
                .help(
                    "A frame is read and developed as stacking reads it, its brightness gain applied, and warped onto the \
                     output's canvas, then kept in the cache, so that it is read rather than decoded the next time; another \
                     output must have the output's size. Give `frame` or `output`. Runs beside other jobs; a newer call \
                     replaces one still running.",
                )
                .requires(Requires::Project)
                .supersede()
                .examples(&[r#"{ "frame": 7 }"#, r#"{ "output": 2 }"#]),
            Lane::Preview,
            set_source,
        ),
        CommandSpec::query(
            Meta::new("retouch.sharpest", "Sharpest frame", "The frame where a point of the output is sharpest.")
                .help("From the depth map of a depth-map output, or the map `retouch.mapFocus` made. `x` and `y` are in pixels of the output.")
                .requires(Requires::Project)
                .examples(&[r#"{ "x": 1200.5, "y": 840 }"#]),
            sharpest,
        ),
        CommandSpec::job(
            Meta::new("retouch.mapFocus", "Map sharpest frames", "Maps each point of the output open for retouching to its sharpest frame.")
                .help(
                    "Measures the output's frames' focus at a quarter of their size, as the depth-map method's first pass \
                     does, and keeps which frame is sharpest at each point as the output's `sharpest` map, for \
                     `retouch.sharpest`. A depth-map output needs none: its depth map tells. Runs beside other jobs.",
                )
                .requires(Requires::Project),
            Lane::Preview,
            map_focus,
        ),
        CommandSpec::instant(
            Meta::new("retouch.paint", "Paint live", "Paints part of a stroke as it is drawn, without committing it.")
                .help(
                    "`begin` starts a stroke with the settings and samples given; later calls add their samples to it. \
                     The result shows the stroke so far exactly as `retouch.stroke` will paint it, which commits it; \
                     `retouch.cancelPaint` takes it back. Returns the region that changed, in output pixels.",
                )
                .requires(Requires::WritableProject)
                .examples(&[
                    r#"{ "begin": true, "stroke": { "brush": "details", "source": { "frame": 7 }, "size": 40, "hardness": 0.5, "opacity": 1, "samples": [[100, 120, 1]] } }"#,
                ]),
            paint,
        ),
        CommandSpec::instant(
            Meta::new("retouch.cancelPaint", "Cancel live stroke", "Takes back the stroke being drawn.").requires(Requires::WritableProject),
            cancel_paint,
        ),
        CommandSpec::instant(
            Meta::new("retouch.stroke", "Retouch stroke", "Paints a stroke onto the output open for retouching, as one undo step.")
                .help(
                    "The stroke goes on top of `layer`'s strokes, or of the top layer's, in a new layer when the output has \
                     none. Its samples are `[x, y, pressure]` in pixels of the output, rounded to a hundredth of a pixel. \
                     Brushes: `details` (the source's detail, its brightness and color matched to the output's, with a \
                     hard edge where there is fine detail), `pixels` (the source as it is), `darken` and `lighten` (only \
                     where the source is darker or lighter), and `eraser` (back to the output as stacked; no source). When \
                     it is the stroke `retouch.paint` drew, it is committed as painted.",
                )
                .undoable()
                .requires(Requires::WritableProject)
                .examples(&[
                    r#"{ "stroke": { "brush": "details", "source": { "frame": 7 }, "size": 40, "hardness": 0.5, "opacity": 1, "samples": [[100, 120, 1], [140, 130, 0.8]] } }"#,
                    r#"{ "stroke": { "brush": "eraser", "size": 80, "hardness": 0, "opacity": 0.5, "samples": [[300, 200, 1]] }, "layer": 5 }"#,
                ]),
            stroke,
        ),
        CommandSpec::instant(
            Meta::new("retouch.addLayer", "Add retouch layer", "Adds an empty retouch layer on top of an output's.")
                .undoable()
                .requires(Requires::WritableProject)
                .examples(&[r#"{ "output": 3, "name": "Antennae" }"#]),
            add_layer,
        ),
        CommandSpec::instant(
            Meta::new("retouch.removeLayer", "Remove retouch layer", "Removes a retouch layer and its strokes.")
                .undoable()
                .requires(Requires::WritableProject)
                .examples(&[r#"{ "output": 3, "layer": 5 }"#]),
            remove_layer,
        ),
        CommandSpec::instant(
            Meta::new("retouch.setLayerVisible", "Show retouch layer", "Shows or hides a retouch layer's strokes.")
                .undoable()
                .requires(Requires::WritableProject)
                .examples(&[r#"{ "output": 3, "layer": 5, "visible": false }"#]),
            set_layer_visible,
        ),
    ]
}

fn open_session(session: &Session) -> Result<&RetouchSession, CommandError> {
    session.retouch.as_ref().ok_or_else(|| session.unavailable("no output is open for retouching: open one with `retouch.open`"))
}

/// An output.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OpenParams {
    /// The output's id, as `output.list` lists it.
    output: OutputId,
}

impl Params for OpenParams {}

/// The output opened.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Opened {
    output: OutputId,
    width: usize,
    height: usize,
    /// How its strokes got onto it: `cached` (read back), `painted` (painted again) or `none` (it has none).
    rendered: &'static str,
    /// Its visible strokes, and how many of them couldn't be painted (their source is gone).
    strokes: usize,
    skipped: usize,
    /// Whether it can tell each point's sharpest frame.
    sharpest: bool,
}

fn open(session: &Session, p: OpenParams) -> Result<JobPlan<Opened>, CommandError> {
    let project = session.project()?;
    let output = project.output(p.output).ok_or(CommandError::NotFound { what: p.output.to_string() })?;
    let strokes = visible_strokes(output);
    let key = render_key(output, &strokes);
    let cached = output.maps.get(&map_name(RETOUCHED)?).filter(|r| r.key.as_deref() == Some(key.as_str())).cloned();
    let frames = output_frames(project, output);
    let depth = (output.method == "depth-map").then(|| output.maps.get(&map_name("depth").ok()?).cloned()).flatten();
    let focus = output.maps.get(&map_name(SHARPEST)?).cloned();
    let (base_ref, id) = (output.image.clone(), output.id());
    let cache = session.cache_dir()?;
    // The loader is made once the output's size is known, from what it needs of the project now: to paint the strokes
    // when there is no cached result, or when it can't be read.
    let mut loader = if strokes.is_empty() { None } else { Some(SourceLoader::new(session, "retouch.open", id)?) };
    let job = JobPlan::new(move |ctx| {
        let base = Arc::new(read_f32(&cache, &base_ref)?);
        ctx.checkpoint()?;
        let mut sources = Vec::new();
        let mut aligned = Vec::new();
        let read = cached.as_ref().and_then(|reference| {
            read_f32(&cache, reference)
                .map_err(|e| tracing::warn!("the cached retouched result can't be read, so the strokes are painted again: {e}"))
                .ok()
        });
        let (canvas, rendered, skipped) = match (read, &mut loader) {
            (Some(canvas), _) => (canvas, "cached", 0),
            (None, Some(loader)) => {
                aligned = load_sources(&strokes, &mut sources, loader, (base.width(), base.height()));
                let usable = paintable(&strokes, &sources);
                let canvas = florastack_retouch::render(&base, &painted(&usable, &base, &sources))?;
                (canvas, "painted", strokes.len() - usable.len())
            }
            (None, None) => ((*base).clone(), "none", 0),
        };
        ctx.checkpoint()?;
        let size = (base.width(), base.height());
        let sharpest = match (&depth, &focus) {
            (Some(reference), _) => sharpest_map(&cache, reference, size, &frames, Some(65535.0)),
            (None, Some(reference)) => sharpest_map(&cache, reference, size, &frames, None),
            _ => None,
        };
        let opened = Opened { output: id, width: size.0, height: size.1, rendered, strokes: strokes.len(), skipped, sharpest: sharpest.is_some() };
        Ok(commit(move |session: &mut Session| {
            session.flush_retouch()?;
            for (frame, reference) in aligned {
                session.apply_system(Edit::SetAlignedImage { frame, aligned: Some(reference) })?;
            }
            session.retouch = Some(RetouchSession {
                output: id,
                base,
                canvas,
                painted: strokes,
                sources,
                current: None,
                live: None,
                sharpest,
                changes: RetouchChanges { opened: true, ..RetouchChanges::default() },
                unsaved: false,
            });
            Ok(opened)
        }))
    });
    Ok(job.stages(&[("Painting strokes", 1.0)]).locks(florastack_project::Aspects::RETOUCH))
}

/// The sharpest map in `reference`, for an output of `size` whose frames are `frames`; `None` when it can't be read.
fn sharpest_map(
    cache: &florastack_project::CacheDir,
    reference: &CacheRef,
    size: (usize, usize),
    frames: &[FrameId],
    full: Option<f64>,
) -> Option<Sharpest> {
    let read = || -> Result<Image<u16>, CommandError> {
        match crate::project::read_image(cache, reference)? {
            AnyImage::U16(image) => Ok(image),
            other => {
                Err(CommandError::Unavailable { command: "retouch".to_owned(), reason: format!("the sharpest map has {:?} samples", other.format()) })
            }
        }
    };
    match read() {
        Ok(map) => {
            let scale = map.width() as f64 / size.0.max(1) as f64;
            Some(Sharpest { map, scale, frames: frames.to_vec(), full_scale: full })
        }
        Err(e) => {
            tracing::warn!("the sharpest map can't be read: {e}");
            None
        }
    }
}

/// Retouching closed.
#[derive(Debug, Serialize, JsonSchema)]
struct Closed {
    /// Whether an output was open for retouching.
    closed: bool,
}

fn close(session: &mut Session, _: NoParams) -> Result<Closed, CommandError> {
    if session.retouch.is_none() {
        return Ok(Closed { closed: false });
    }
    session.flush_retouch()?;
    session.retouch = None;
    session.retouch_closed = true;
    Ok(Closed { closed: true })
}

/// A source: a frame or an output.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SourceParams {
    /// A frame, as `project.frames` lists it.
    #[serde(default)]
    frame: Option<FrameId>,
    /// Another output of the same size, as `output.list` lists it.
    #[serde(default)]
    output: Option<OutputId>,
}

impl Params for SourceParams {
    fn check(&self) -> Result<(), Invalid> {
        match (self.frame, self.output) {
            (Some(_), None) | (None, Some(_)) => Ok(()),
            _ => Err(Invalid::new("frame", "give either `frame` or `output`")),
        }
    }
}

/// The source set.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SourceSet {
    source: Source,
    /// How it was loaded: `memory` (kept since it was last used), `cache` or `decoded`.
    loaded: &'static str,
}

fn set_source(session: &Session, p: SourceParams) -> Result<JobPlan<SourceSet>, CommandError> {
    let open = open_session(session)?;
    let source = match (p.frame, p.output) {
        (Some(f), _) => Source::Frame(f),
        (None, Some(o)) => Source::Output(o),
        (None, None) => return Err(session.invalid("frame", "give either `frame` or `output`")),
    };
    let output = open.output;
    let size = (open.canvas.width(), open.canvas.height());
    let in_memory = open.source(source);
    let mut loader = if in_memory.is_some() { None } else { Some(SourceLoader::new(session, "retouch.setSource", output)?) };
    let job = JobPlan::new(move |_ctx| {
        let (image, cached, how) = match (in_memory, &mut loader) {
            (Some(image), _) => (image, None, "memory"),
            (None, Some(loader)) => {
                let (image, cached) = loader.load(source, size)?;
                // A frame's warped image newly cached was decoded; otherwise it was read from the cache.
                let from = if cached.is_some() { "decoded" } else { "cache" };
                (Arc::new(image), cached, from)
            }
            (None, None) => return Err(CommandError::Internal { command: "retouch.setSource".to_owned() }),
        };
        Ok(commit(move |session: &mut Session| {
            if let Some((frame, reference)) = cached {
                session.apply_system(Edit::SetAlignedImage { frame, aligned: Some(reference) })?;
            }
            let open = session.retouch.as_mut().filter(|s| s.output == output).ok_or_else(|| CommandError::Unavailable {
                command: "retouch.setSource".to_owned(),
                reason: "the output was closed for retouching meanwhile".to_owned(),
            })?;
            open.keep_source(source, image);
            open.current = Some(source);
            open.changes.source = true;
            Ok(SourceSet { source, loaded: how })
        }))
    });
    Ok(job.stages(&[("Loading the source", 1.0)]))
}

/// A point of the output.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PointParams {
    /// In pixels of the output, from its left edge.
    x: f64,
    /// In pixels of the output, from its top edge.
    y: f64,
}

impl Params for PointParams {
    fn check(&self) -> Result<(), Invalid> {
        if !self.x.is_finite() {
            return Err(Invalid::new("x", "must be a finite number"));
        }
        if !self.y.is_finite() {
            return Err(Invalid::new("y", "must be a finite number"));
        }
        Ok(())
    }
}

/// The sharpest frame at a point.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SharpestFrame {
    /// The frame; none where no frame covers the point, or beyond the output.
    frame: Option<FrameId>,
    /// Its position among the output's frames, in the order they were stacked.
    index: Option<usize>,
}

fn sharpest(session: &Session, p: PointParams) -> Result<SharpestFrame, CommandError> {
    let open = open_session(session)?;
    let map = open
        .sharpest
        .as_ref()
        .ok_or_else(|| session.unavailable("the output has no map of its sharpest frames: make one with `retouch.mapFocus`"))?;
    let found = map.at(p.x, p.y);
    Ok(SharpestFrame { frame: found.map(|(_, f)| f), index: found.map(|(i, _)| i) })
}

/// The sharpest frames mapped.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Mapped {
    /// The map's size, a quarter of the output's on each side.
    width: usize,
    height: usize,
    /// The frames it maps to.
    frames: usize,
}

fn map_focus(session: &Session, _: NoParams) -> Result<JobPlan<Mapped>, CommandError> {
    let open = open_session(session)?;
    let project = session.project()?;
    let id = open.output;
    let output = project.output(id).ok_or(CommandError::NotFound { what: id.to_string() })?;
    let frames = output_frames(project, output);
    let depth = stack_settings(project)?.depth_map;
    let mut plan =
        StackPlan::of_frames(session, "retouch.mapFocus", Chosen::DepthMap(depth.clone()), Quality::Preview, &frames, output_framing(output))?;
    let size = (open.canvas.width(), open.canvas.height());
    if (plan.canvas.width, plan.canvas.height) != size {
        return Err(session
            .unavailable("the frames, as aligned now, don't make this output's canvas: they were aligned or framed again since it was stacked"));
    }
    let job = JobPlan::new(move |ctx| {
        plan.reader.load_mask()?;
        let n = plan.frames.len();
        let mut units = Units::new(n);
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
        ctx.stage(1);
        let stats = estimator.finish(ctx)?;
        // The plan's order to the output's: each point's nearest frame, as its index among the output's frames.
        let order: Vec<u16> =
            plan.ids().iter().map(|id| frames.iter().position(|f| f == id).and_then(|i| u16::try_from(i).ok()).unwrap_or(NO_FRAME)).collect();
        let (w, h) = (stats.width(), stats.height());
        let mut map = Vec::with_capacity(w * h);
        for y in 0..h {
            let (Some(depth), covered) = (stats.depth.row(y), stats.covered.as_ref().and_then(|c| c.row(y))) else { continue };
            for (x, &d) in depth.iter().enumerate() {
                let covers = covered.is_none_or(|c| c.get(x).is_some_and(|&k| k > 0.0));
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Rounded within the frames.
                let nearest = if d.is_finite() { d.round().max(0.0) as usize } else { usize::MAX };
                map.push(if covers { order.get(nearest).copied().unwrap_or(NO_FRAME) } else { NO_FRAME });
            }
        }
        let image = Image::from_vec(w, h, 1, florastack_core::ColorSpace::LINEAR, map).map_err(florastack_stack::StackError::from)?;
        let reference = write_image_in(&plan.cache, Area::Cache, &format!("sharpest-{}", id.get()), &AnyImage::U16(image.clone()), None)?;
        let mapped = Mapped { width: w, height: h, frames: frames.len() };
        Ok(commit(move |session: &mut Session| {
            session.apply_system(Edit::SetOutputMap { output: id, name: map_name(SHARPEST)?, map: Some(reference) })?;
            if let Some(open) = session.retouch.as_mut().filter(|s| s.output == id) {
                let scale = w as f64 / size.0.max(1) as f64;
                open.sharpest = Some(Sharpest { map: image, scale, frames, full_scale: None });
            }
            Ok(mapped)
        }))
    });
    Ok(job.stages(&[("Measuring focus", 0.95), ("Mapping", 0.05)]))
}

/// Part of a stroke, as it is drawn.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PaintParams {
    /// The stroke: with `begin`, its settings and first samples; later, the samples to add (its settings must stay
    /// the same).
    stroke: Stroke,
    /// Starts a new stroke, in place of one being drawn.
    #[serde(default)]
    begin: bool,
}

impl Params for PaintParams {
    fn check(&self) -> Result<(), Invalid> {
        self.stroke.validate().map_err(|e| Invalid::new("stroke", e.to_string()))
    }
}

/// What painting changed.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct PaintedRegion {
    /// The region painted, `[x, y, width, height]` in output pixels; none when nothing changed.
    region: Option<[usize; 4]>,
}

/// The source a stroke from `source` reads in the open session: none for the eraser.
fn source_of(open: &RetouchSession, source: Option<Source>) -> Result<Option<Arc<crate::retouch::SourceImage>>, CommandError> {
    let Some(source) = source else { return Ok(None) };
    let image = open.source(source).ok_or_else(|| CommandError::Unavailable {
        command: "retouch".to_owned(),
        reason: format!("{source:?} isn't loaded: set it with `retouch.setSource` first"),
    })?;
    Ok(Some(image))
}

fn region(rect: Option<florastack_core::Rect>) -> Option<[usize; 4]> {
    rect.map(|r| [r.x, r.y, r.width, r.height])
}

fn paint(session: &mut Session, p: PaintParams) -> Result<PaintedRegion, CommandError> {
    let settings_changed = session.invalid("stroke", "changes its settings part way: begin a new stroke");
    let open = session.retouch.as_mut().ok_or_else(|| CommandError::Unavailable {
        command: "retouch.paint".to_owned(),
        reason: "no output is open for retouching: open one with `retouch.open`".to_owned(),
    })?;
    let stroke = p.stroke.quantized();
    let source = source_of(open, stroke.source)?;
    let base = Arc::clone(&open.base);
    let fields = Fields { base: &base, source: source.as_ref().map(|s| &s.image), coverage: source.as_ref().and_then(|s| s.coverage.as_ref()) };
    let changed = if let (Some(live), false) = (&mut open.live, p.begin) {
        let same = Stroke { samples: Vec::new(), ..live.stroke().clone() } == Stroke { samples: Vec::new(), ..stroke.clone() };
        if !same {
            return Err(settings_changed);
        }
        live.extend(&mut open.canvas, &fields, &stroke.samples)?
    } else {
        let undone = open.live.take().and_then(|live| live.cancel(&mut open.canvas));
        let first = Stroke { samples: stroke.samples.first().copied().into_iter().collect(), ..stroke.clone() };
        let mut live = LiveStroke::new(first)?;
        let mut changed = live.extend(&mut open.canvas, &fields, &[])?;
        if let Some(rest) = stroke.samples.get(1..).filter(|r| !r.is_empty()) {
            changed = union(changed, live.extend(&mut open.canvas, &fields, rest)?);
        }
        open.live = Some(live);
        union(undone, changed)
    };
    open.touched(changed);
    Ok(PaintedRegion { region: region(changed) })
}

/// Nothing was being drawn, or what was is taken back.
#[derive(Debug, Serialize, JsonSchema)]
struct Cancelled {
    cancelled: bool,
}

fn cancel_paint(session: &mut Session, _: NoParams) -> Result<Cancelled, CommandError> {
    let Some(open) = session.retouch.as_mut() else { return Ok(Cancelled { cancelled: false }) };
    let Some(live) = open.live.take() else { return Ok(Cancelled { cancelled: false }) };
    let changed = live.cancel(&mut open.canvas);
    open.touched(changed);
    Ok(Cancelled { cancelled: true })
}

/// A stroke to commit.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StrokeParams {
    /// The stroke: its brush, source, size, hardness, opacity and samples.
    stroke: Stroke,
    /// The layer it goes into, which must be visible; by default the top layer, or a new one when there is none.
    #[serde(default)]
    layer: Option<LayerId>,
}

impl Params for StrokeParams {
    fn check(&self) -> Result<(), Invalid> {
        self.stroke.validate().map_err(|e| Invalid::new("stroke", e.to_string()))
    }
}

/// The stroke committed.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Stroked {
    output: OutputId,
    layer: LayerId,
    /// The layer's strokes now.
    strokes: usize,
    /// The region painted, in output pixels.
    region: Option<[usize; 4]>,
}

fn stroke(session: &mut Session, p: StrokeParams) -> Result<Stroked, CommandError> {
    let stroke = p.stroke.quantized();
    let output = open_session(session)?.output;
    // The layer: the one given, which must be visible, the top one, or a new one.
    let project = session.project()?;
    let target = project.output(output).ok_or(CommandError::NotFound { what: output.to_string() })?;
    let top = target.retouch.last().map(|l| (l.id(), l.visible));
    let layer = match p.layer {
        Some(id) => match target.layer(id) {
            Some(l) if l.visible => Some(id),
            Some(_) => return Err(session.unavailable(format!("{id} is hidden: show it with `retouch.setLayerVisible` before painting into it"))),
            None => return Err(CommandError::NotFound { what: id.to_string() }),
        },
        None => match top {
            Some((id, true)) => Some(id),
            Some((id, false)) => return Err(session.unavailable(format!("the top layer, {id}, is hidden: show it or pick a visible layer"))),
            None => None,
        },
    };
    let on_top = layer.is_none() || layer == top.map(|t| t.0);
    let open = session.retouch.as_mut().ok_or(CommandError::Internal { command: "retouch.stroke".to_owned() })?;
    stroke.validate_for(open.canvas.width(), open.canvas.height())?;
    let source = source_of(open, stroke.source)?;
    let base = Arc::clone(&open.base);
    // The stroke drawn live is committed as painted; any other is painted now.
    let changed = match open.live.take() {
        Some(live) if *live.stroke() == stroke => None,
        live => {
            let undone = live.and_then(|l| l.cancel(&mut open.canvas));
            let fields =
                Fields { base: &base, source: source.as_ref().map(|s| &s.image), coverage: source.as_ref().and_then(|s| s.coverage.as_ref()) };
            let whole = florastack_core::Rect::new(0, 0, open.canvas.width(), open.canvas.height());
            union(undone, florastack_retouch::apply(&mut open.canvas, Painted { stroke: &stroke, fields }, whole)?)
        }
    };
    let reach = Stroke::reach(&stroke.dabs(), 1.0, open.canvas.width(), open.canvas.height());
    open.touched(union(changed, reach));
    // On the top layer the canvas is already right; under other layers, the sync paints the region again in order.
    if on_top {
        open.painted.push(stroke.clone());
        open.unsaved = true;
    }
    let value = serde_json::to_value(&stroke).map_err(|_| CommandError::Internal { command: "retouch.stroke".to_owned() })?;
    let layer = if let Some(layer) = layer {
        layer
    } else {
        session.apply(Edit::AddLayer { output, name: "Layer 1".to_owned() })?;
        let project = session.project()?;
        project
            .output(output)
            .and_then(|o| o.retouch.last())
            .map(florastack_project::RetouchLayer::id)
            .ok_or(CommandError::Internal { command: "retouch.stroke".to_owned() })?
    };
    session.apply(Edit::PushStroke { output, layer, stroke: value })?;
    let strokes = session.project()?.output(output).and_then(|o| o.layer(layer)).map_or(0, |l| l.strokes.len());
    Ok(Stroked { output, layer, strokes, region: region(reach) })
}

/// A new layer.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AddLayerParams {
    /// The output, as `output.list` lists it.
    output: OutputId,
    /// Its name; "Layer n" when left out.
    #[serde(default)]
    name: Option<String>,
}

impl Params for AddLayerParams {
    fn check(&self) -> Result<(), Invalid> {
        self.name.as_ref().map_or(Ok(()), |n| check_name("name", n))
    }
}

/// A layer.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct LayerDone {
    output: OutputId,
    layer: LayerId,
}

fn add_layer(session: &mut Session, p: AddLayerParams) -> Result<LayerDone, CommandError> {
    let project = session.project()?;
    let output = project.output(p.output).ok_or(CommandError::NotFound { what: p.output.to_string() })?;
    let name = p.name.unwrap_or_else(|| format!("Layer {}", output.retouch.len() + 1));
    session.apply(Edit::AddLayer { output: p.output, name })?;
    let layer = session.project()?.output(p.output).and_then(|o| o.retouch.last()).map(florastack_project::RetouchLayer::id);
    Ok(LayerDone { output: p.output, layer: layer.ok_or(CommandError::Internal { command: "retouch.addLayer".to_owned() })? })
}

/// A layer of an output.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LayerParams {
    /// The output, as `output.list` lists it.
    output: OutputId,
    /// The layer, as `output.list` lists the output's layers.
    layer: LayerId,
}

impl Params for LayerParams {}

fn remove_layer(session: &mut Session, p: LayerParams) -> Result<LayerDone, CommandError> {
    session.apply(Edit::RemoveLayer { output: p.output, layer: p.layer })?;
    Ok(LayerDone { output: p.output, layer: p.layer })
}

/// A layer shown or hidden.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct VisibleParams {
    /// The output, as `output.list` lists it.
    output: OutputId,
    /// The layer, as `output.list` lists the output's layers.
    layer: LayerId,
    /// Whether its strokes show.
    visible: bool,
}

impl Params for VisibleParams {}

fn set_layer_visible(session: &mut Session, p: VisibleParams) -> Result<LayerDone, CommandError> {
    let project = session.project()?;
    let layer = project.output(p.output).and_then(|o| o.layer(p.layer)).ok_or(CommandError::NotFound { what: p.layer.to_string() })?;
    if layer.visible != p.visible {
        session.apply(Edit::SetLayerVisible { output: p.output, layer: p.layer, visible: p.visible })?;
    }
    Ok(LayerDone { output: p.output, layer: p.layer })
}
