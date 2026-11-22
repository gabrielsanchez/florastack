//! The retouch session: an output open for retouching, held in memory while it is retouched (`retouch.open` to
//! `retouch.close`). It keeps the output's own pixels (the base), the result with its visible strokes painted (the
//! canvas), the sources loaded, the stroke being drawn, and a map of each point's sharpest frame. Strokes go into the
//! project one undo step each (florastack-project's `Edit::PushStroke`); after any change of the output's layers (a
//! stroke, an undo, a layer hidden or removed) the session compares the strokes it painted with the project's and
//! paints again only the region where they differ, which florastack-retouch makes exact.
//!
//! - **Sources**: another output of the same size is read from the cache; a frame is read and developed as stacking
//!   reads it, its alignment's gain applied, warped onto the output's canvas with its coverage, and kept in the cache
//!   as a 32-bit TIFF (the frame's `aligned` image, keyed by what it is made from), so that it is read rather than
//!   decoded the next time.
//! - **No recomputation**: the result is cached (the output's `retouched` map, keyed by the output and its strokes)
//!   when the project is saved and when the session closes, and read back when the output is opened again.
//! - **Memory**: the output, the result and the sources are kept tile by tile within a share of the memory budget,
//!   the rest spilled to temporary files (`tiles.rs`); painting reads a window around what it paints, large regions a
//!   piece at a time, and florastack-retouch paints a window to the same bits as the whole image.
//! - **The sharpest frame**: a depth-map output's own depth map, or the output's `sharpest` map, made once by
//!   `retouch.mapFocus` from the frames' focus at a quarter of their size.
//!
//! Apps draw the session through [`RetouchView`] and learn what changed with [`crate::Engine::take_retouch_changes`].

use std::sync::Arc;

use florastack_align::{Canvas, DEFAULT_FEATHER, Framing, warp_to_canvas};
use florastack_core::{Affine, AnyImage, Image, Rect};
use florastack_project::{Area, CacheDir, CacheRef, ContentHash, Edit, FrameId, Input, MapName, Output, OutputId, Project};
use florastack_retouch::{Fields, LiveStroke, Painted, Source, Stroke};

use crate::commands::align::Plan;
use crate::error::CommandError;
use crate::project::{read_image, stored_alignment, write_image_in};
use crate::session::Session;
use crate::view::{DisplayImage, ViewError, display_region};

mod tiles;
pub(crate) use tiles::{Shared, Store, Tiled};

/// The output map that caches the retouched result, and the one that maps each point's sharpest frame.
pub(crate) const RETOUCHED: &str = "retouched";
pub(crate) const SHARPEST: &str = "sharpest";
/// How many sources stay in memory.
const KEPT_SOURCES: usize = 3;
/// How much disk the frames warped for retouching may take in a project's cache, by default: 4 GiB, about 13 frames
/// of 24 MP (each is kept at 32 bits a sample, with its coverage).
pub const DEFAULT_SOURCE_LIMIT: u64 = 4 << 30;
/// In the sharpest map, a point no frame covers.
pub(crate) const NO_FRAME: u16 = u16::MAX;

/// A source on the output's canvas: its pixels and, for a frame, how much of each pixel the frame covers.
pub(crate) struct SourceImage {
    pub image: Image<f32>,
    pub coverage: Option<Image<f32>>,
}

/// A source in a session's tiles.
pub(crate) struct SourceTiles {
    pub image: Tiled,
    pub coverage: Option<Tiled>,
}

impl SourceTiles {
    pub(crate) fn of(store: &Shared, source: &SourceImage) -> Result<SourceTiles, CommandError> {
        let coverage = source.coverage.as_ref().map(|c| Tiled::from_image(store, c)).transpose()?;
        Ok(SourceTiles { image: Tiled::from_image(store, &source.image)?, coverage })
    }
}

/// The sources a session holds, the most recently used first.
pub(crate) type Sources = Vec<(Source, Arc<SourceTiles>)>;

/// Each point's sharpest frame: a map of indices into `frames`, `scale` map pixels to an output pixel.
pub(crate) struct Sharpest {
    pub map: Image<u16>,
    pub scale: f64,
    pub frames: Vec<FrameId>,
    /// The map's value for the last frame: a depth map spans 0 to 65535 over the frames, the `sharpest` map holds
    /// indices.
    pub full_scale: Option<f64>,
}

impl Sharpest {
    /// The sharpest frame at output pixel `(x, y)`, with its index among the output's frames.
    pub fn at(&self, x: f64, y: f64) -> Option<(usize, FrameId)> {
        if !(x.is_finite() && y.is_finite()) || x < 0.0 || y < 0.0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Checked non-negative, and the map's get bounds it.
        let (mx, my) = ((x * self.scale) as usize, (y * self.scale) as usize);
        let v = self.map.get(mx, my, 0)?;
        if v == NO_FRAME && self.full_scale.is_none() {
            return None;
        }
        let index = match self.full_scale {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Rounded within the frames.
            Some(full) => (f64::from(v) / full * (self.frames.len().saturating_sub(1)) as f64).round() as usize,
            None => usize::from(v),
        };
        self.frames.get(index).map(|&f| (index, f))
    }
}

/// See the module documentation.
pub(crate) struct RetouchSession {
    pub output: OutputId,
    /// The tiles of the images below.
    pub store: Shared,
    pub base: Tiled,
    pub canvas: Tiled,
    /// The visible strokes painted on the canvas, the bottom layer's first.
    pub painted: Vec<Stroke>,
    /// The sources loaded, the most recently used first.
    pub sources: Sources,
    /// The source strokes paint from now.
    pub current: Option<Source>,
    pub live: Option<LiveStroke>,
    pub sharpest: Option<Sharpest>,
    /// What changed since the app last asked.
    pub changes: RetouchChanges,
    /// Whether the canvas holds strokes the cached result doesn't.
    pub unsaved: bool,
    /// The visible strokes whose source is gone from the project, by layer ([`left_out`]).
    pub left_out: Vec<LeftOut>,
}

impl RetouchSession {
    pub fn source(&self, source: Source) -> Option<Arc<SourceTiles>> {
        self.sources.iter().find(|(s, _)| *s == source).map(|(_, image)| Arc::clone(image))
    }

    /// Keeps `image` as `source`, the most recently used, letting the oldest go beyond [`KEPT_SOURCES`].
    pub fn keep_source(&mut self, source: Source, image: Arc<SourceTiles>) {
        self.sources.retain(|(s, _)| *s != source);
        self.sources.insert(0, (source, image));
        self.sources.truncate(KEPT_SOURCES);
    }

    /// Notes `rect` as changed.
    pub fn touched(&mut self, rect: Option<Rect>) {
        self.changes.region = union(self.changes.region, rect);
    }

    /// The output's size.
    pub fn size(&self) -> (usize, usize) {
        (self.canvas.width(), self.canvas.height())
    }

    fn images(&self) -> Images<'_> {
        Images { base: &self.base, canvas: &self.canvas, sources: &self.sources }
    }

    /// Paints `rect` of the result again from the output with `strokes` (those whose source is loaded), piece by
    /// piece.
    pub fn replay(&self, strokes: &[Stroke], rect: Rect) -> Result<(), CommandError> {
        replay_tiled(self.images(), strokes, rect)
    }

    /// Paints `stroke` over the result within `clip`, piece by piece. Returns the region it may have changed.
    pub fn apply(&self, stroke: &Stroke, clip: Rect) -> Result<Option<Rect>, CommandError> {
        let (w, h) = self.size();
        let Some(reach) = Stroke::reach(&stroke.dabs(), 1.0, w, h).and_then(|r| florastack_retouch::intersect(r, clip)) else { return Ok(None) };
        for piece in pieces(reach, self.canvas.piece()) {
            let at = union(Some(piece), Some(florastack_retouch::reads(stroke.brush, stroke.size, piece, w, h))).unwrap_or(piece);
            let fields = self.images().window(at, stroke.source.as_slice())?;
            let mut canvas = self.canvas.read(at)?;
            florastack_retouch::apply(&mut canvas, Painted { stroke, fields: fields.fields(stroke.source) }, piece)?;
            self.canvas.write(&canvas, at, piece)?;
        }
        Ok(Some(reach))
    }

    /// Adds `samples` to the stroke being drawn and paints its new dabs. Returns the region it may have changed.
    pub fn extend_live(&mut self, live: &mut LiveStroke, samples: &[[f64; 3]]) -> Result<Option<Rect>, CommandError> {
        let (w, h) = self.size();
        // With no new dab reaching the output, nothing is read: a pixel's window does for the checks.
        let at = live.window_for(samples, w, h).unwrap_or(Rect::new(0, 0, w.min(1), h.min(1)));
        let source = live.stroke().source;
        let fields = self.images().window(at, source.as_slice())?;
        let mut canvas = self.canvas.read(at)?;
        let changed = live.extend(&mut canvas, &fields.fields(source), samples)?;
        if let Some(changed) = changed {
            self.canvas.write(&canvas, at, changed)?;
        }
        Ok(changed)
    }

    /// Puts back the result from before the stroke being drawn. Returns the region it changed.
    pub fn cancel_live(&mut self, live: LiveStroke) -> Result<Option<Rect>, CommandError> {
        let (w, h) = self.size();
        let Some(at) = live.extent(w, h) else { return Ok(None) };
        let mut canvas = self.canvas.read(at)?;
        let changed = live.cancel(&mut canvas, florastack_retouch::Window::at(at, w, h))?;
        self.canvas.write(&canvas, at, at)?;
        Ok(changed)
    }
}

/// A session's images, for painting in pieces.
#[derive(Clone, Copy)]
pub(crate) struct Images<'a> {
    pub base: &'a Tiled,
    pub canvas: &'a Tiled,
    pub sources: &'a [(Source, Arc<SourceTiles>)],
}

/// The fields painting reads in a window of the output: the base's and the sources' pixels there.
pub(crate) struct WindowFields {
    window: florastack_retouch::Window,
    base: Image<f32>,
    sources: Vec<(Source, Image<f32>, Option<Image<f32>>)>,
}

impl WindowFields {
    /// The fields of a stroke from `source`.
    fn fields(&self, source: Option<Source>) -> Fields<'_> {
        let found = source.and_then(|s| self.sources.iter().find(|(k, ..)| *k == s));
        Fields { base: &self.base, source: found.map(|(_, image, _)| image), coverage: found.and_then(|(.., c)| c.as_ref()), window: self.window }
    }
}

impl Images<'_> {
    /// The fields in window `at` for strokes from `sources` (those loaded).
    fn window(&self, at: Rect, sources: &[Source]) -> Result<WindowFields, CommandError> {
        let mut read: Vec<(Source, Image<f32>, Option<Image<f32>>)> = Vec::new();
        for source in sources {
            if read.iter().any(|(s, ..)| s == source) {
                continue;
            }
            if let Some((_, tiles)) = self.sources.iter().find(|(s, _)| s == source) {
                let coverage = tiles.coverage.as_ref().map(|c| c.read(at)).transpose()?;
                read.push((*source, tiles.image.read(at)?, coverage));
            }
        }
        let window = florastack_retouch::Window::at(at, self.base.width(), self.base.height());
        Ok(WindowFields { window, base: self.base.read(at)?, sources: read })
    }
}

/// Paints `rect` of the result again from the output with `strokes`, as [`florastack_retouch::replay`] does, a piece
/// at a time, each through the window of the images it reads; strokes whose source isn't loaded are left out.
pub(crate) fn replay_tiled(images: Images<'_>, strokes: &[Stroke], rect: Rect) -> Result<(), CommandError> {
    let (w, h) = (images.base.width(), images.base.height());
    let loaded = |s: &Stroke| s.source.is_none_or(|src| images.sources.iter().any(|(k, _)| *k == src));
    let strokes: Vec<(&Stroke, Rect)> =
        strokes.iter().filter(|s| loaded(s)).filter_map(|s| Some((s, Stroke::reach(&s.dabs(), 1.0, w, h)?))).collect();
    let Some(rect) = florastack_retouch::intersect(rect, images.base.rect()) else { return Ok(()) };
    for piece in pieces(rect, images.canvas.piece()) {
        let involved: Vec<(&Stroke, Rect)> = strokes.iter().filter_map(|(s, r)| Some((*s, florastack_retouch::intersect(*r, piece)?))).collect();
        let mut at = piece;
        for (s, part) in &involved {
            at = union(Some(at), Some(florastack_retouch::reads(s.brush, s.size, *part, w, h))).unwrap_or(at);
        }
        let sources: Vec<Source> = involved.iter().filter_map(|(s, _)| s.source).collect();
        let fields = images.window(at, &sources)?;
        // Replaying starts from the output's pixels, so the window of the output does for the result's.
        let mut canvas = fields.base.clone();
        let painted: Vec<Painted<'_>> = involved.iter().map(|(stroke, _)| Painted { stroke, fields: fields.fields(stroke.source) }).collect();
        florastack_retouch::replay(&mut canvas, &fields.base, &painted, piece, fields.window)?;
        images.canvas.write(&canvas, at, piece)?;
    }
    Ok(())
}

/// `rect` in pieces of at most `side` pixels a side, row by row: large regions are painted and shown a piece at a
/// time, each reading a window of the images around it.
fn pieces(rect: Rect, side: usize) -> impl Iterator<Item = Rect> {
    let side = side.max(1);
    (rect.y..rect.y + rect.height).step_by(side).flat_map(move |y| {
        (rect.x..rect.x + rect.width)
            .step_by(side)
            .map(move |x| Rect::new(x, y, side.min(rect.x + rect.width - x), side.min(rect.y + rect.height - y)))
    })
}

/// `rect` of `image` for display, reduced by `factor`, as [`display_region`] gives it from the whole image: read a
/// band of rows at a time.
pub(crate) fn display_tiled(image: &Tiled, rect: Rect, factor: usize) -> Result<(DisplayImage, Rect), ViewError> {
    let f = factor.max(1);
    let (w, h) = (image.width(), image.height());
    let (x0, y0) = (rect.x.min(w) / f, rect.y.min(h) / f);
    let (x1, y1) = ((rect.x + rect.width).min(w).div_ceil(f), (rect.y + rect.height).min(h).div_ceil(f));
    if x1 <= x0 || y1 <= y0 {
        return display_region(&image.read(Rect::new(0, 0, w.min(1), h.min(1))).map_err(|e| ViewError::Retouch(Box::new(e)))?, rect, f);
    }
    // Bands whole blocks high, on the blocks' grid, so that each block is averaged as from the whole image.
    let band = (image.piece() / f).max(1);
    let mut rgba = Vec::new();
    let mut alpha = false;
    let mut width = 0;
    for by in (y0..y1).step_by(band) {
        let rows = band.min(y1 - by);
        let pixels = Rect::new(x0 * f, by * f, (x1 * f).min(w) - x0 * f, ((by + rows) * f).min(h) - by * f);
        let part = image.read(pixels).map_err(|e| ViewError::Retouch(Box::new(e)))?;
        let (shown, _) = display_region(&part, Rect::new(0, 0, part.width(), part.height()), f)?;
        (width, alpha) = (shown.width, shown.alpha);
        rgba.extend_from_slice(&shown.rgba);
    }
    Ok((DisplayImage { width, height: y1 - y0, rgba, alpha }, Rect::new(x0, y0, x1 - x0, y1 - y0)))
}

/// Strokes of a layer that paint from a source the project no longer has: they are left out when the strokes are
/// painted again (when the output opens without its cached result, after an undo, on export), though the result shown
/// may still hold them, painted while the source was there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeftOut {
    pub layer: florastack_project::LayerId,
    pub layer_name: String,
    /// How many of the layer's strokes paint from `source`.
    pub strokes: usize,
    pub source: Source,
    pub why: Gone,
}

/// Why a stroke's source is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gone {
    /// The frame was removed from the project.
    FrameRemoved,
    /// The frame is in the project, but the alignment has none of it (it was added or rejected since).
    FrameNotAligned,
    /// The output was removed.
    OutputRemoved,
}

/// The visible strokes of `output` whose source is gone from `project`, by layer and source, bottom layer first.
pub(crate) fn left_out(project: &Project, output: &Output) -> Vec<LeftOut> {
    let aligned = stored_alignment(project).ok().flatten().map(|a| a.frames.iter().map(|f| f.frame).collect::<Vec<_>>());
    let gone = |source: Source| match source {
        Source::Frame(id) if project.frame(id).is_none() => Some(Gone::FrameRemoved),
        Source::Frame(id) if aligned.as_ref().is_none_or(|a| !a.contains(&id)) => Some(Gone::FrameNotAligned),
        Source::Output(id) if project.output(id).is_none() => Some(Gone::OutputRemoved),
        _ => None,
    };
    let mut found: Vec<LeftOut> = Vec::new();
    for layer in output.retouch.iter().filter(|l| l.visible) {
        for value in &layer.strokes {
            let Some(source) = value.get("source").cloned().and_then(|v| serde_json::from_value::<Source>(v).ok()) else { continue };
            let Some(why) = gone(source) else { continue };
            match found.iter_mut().find(|l| l.layer == layer.id() && l.source == source) {
                Some(entry) => entry.strokes += 1,
                None => found.push(LeftOut { layer: layer.id(), layer_name: layer.name.clone(), strokes: 1, source, why }),
            }
        }
    }
    found
}

/// The rectangle that holds both `a` and `b` (florastack-retouch's), for apps folding changes together.
pub use florastack_retouch::union;

/// What changed in the retouch session since an app last asked ([`crate::Engine::take_retouch_changes`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetouchChanges {
    /// The region of the result painted again, in output pixels.
    pub region: Option<Rect>,
    /// Whether the source changed.
    pub source: bool,
    /// Whether an output was opened (everything is new) or the session closed.
    pub opened: bool,
    pub closed: bool,
}

/// The retouch session, as an app draws it.
pub struct RetouchView<'a> {
    pub(crate) session: &'a RetouchSession,
}

impl RetouchView<'_> {
    pub fn output(&self) -> OutputId {
        self.session.output
    }

    /// The output's size, in pixels.
    pub fn size(&self) -> (usize, usize) {
        self.session.size()
    }

    /// The source strokes paint from now.
    pub fn source(&self) -> Option<Source> {
        self.session.current
    }

    /// Whether the session can tell each point's sharpest frame (`retouch.sharpest`).
    pub fn has_sharpest(&self) -> bool {
        self.session.sharpest.is_some()
    }

    /// The visible strokes whose source is gone from the project, by layer and source: they are left out when the
    /// strokes are painted again.
    pub fn left_out(&self) -> &[LeftOut] {
        &self.session.left_out
    }

    /// The sharpest frame at output pixel `(x, y)`, when the session can tell.
    pub fn sharpest(&self, x: f64, y: f64) -> Option<FrameId> {
        self.session.sharpest.as_ref().and_then(|s| s.at(x, y)).map(|(_, f)| f)
    }

    /// `rect` of the result for display, reduced by `factor` ([`display_region`]).
    pub fn result(&self, rect: Rect, factor: usize) -> Result<(DisplayImage, Rect), ViewError> {
        display_tiled(&self.session.canvas, rect, factor)
    }

    /// `rect` of the source for display, reduced by `factor`; `None` without a source.
    pub fn source_pixels(&self, rect: Rect, factor: usize) -> Option<Result<(DisplayImage, Rect), ViewError>> {
        let source = self.session.source(self.session.current?)?;
        Some(display_tiled(&source.image, rect, factor))
    }
}

/// An image read from the cache as linear `f32`.
pub(crate) fn read_f32(cache: &CacheDir, reference: &CacheRef) -> Result<Image<f32>, CommandError> {
    let image = read_image(cache, reference)?;
    let image = match image {
        AnyImage::F32(image) => image,
        AnyImage::U16(image) => image.to_linear_f32().map_err(florastack_stack::StackError::from)?,
        AnyImage::U8(image) => image.to_linear_f32().map_err(florastack_stack::StackError::from)?,
    };
    Ok(image)
}

/// The output's visible strokes, bottom layer first; a stroke this version can't read is left out, with a warning.
pub(crate) fn visible_strokes(output: &Output) -> Vec<Stroke> {
    let mut strokes = Vec::new();
    for layer in output.retouch.iter().filter(|l| l.visible) {
        for value in &layer.strokes {
            match serde_json::from_value::<Stroke>(value.clone())
                .map_err(|e| e.to_string())
                .and_then(|s| s.validate().map(|()| s).map_err(|e| e.to_string()))
            {
                Ok(stroke) => strokes.push(stroke),
                Err(e) => tracing::warn!("a stroke of {} can't be painted, and is left out: {e}", output.id()),
            }
        }
    }
    strokes
}

/// The key of the result of `output` with `strokes`: what the cached result is made from.
pub(crate) fn render_key(output: &Output, strokes: &[Stroke]) -> String {
    let text = serde_json::to_string(&(&output.image.hash, strokes)).unwrap_or_default();
    format!("retouched {}", ContentHash::of(text.as_bytes()))
}

/// An output's frames, in the order it was stacked: its own, and its source outputs' (a slabbed stack's slabs).
pub(crate) fn output_frames(project: &Project, output: &Output) -> Vec<FrameId> {
    let mut frames = Vec::new();
    for input in &output.inputs {
        match input {
            Input::Frame(f) => frames.push(*f),
            Input::Output(o) => {
                if let Some(inner) = project.output(*o) {
                    for f in output_frames(project, inner) {
                        if !frames.contains(&f) {
                            frames.push(f);
                        }
                    }
                }
            }
        }
    }
    frames
}

/// The framing an output was stacked with.
pub(crate) fn output_framing(output: &Output) -> Framing {
    serde_json::from_value(output.parameters["framing"].clone()).unwrap_or(Framing::Narrow)
}

/// What loading sources for an output needs, taken from the project: a reader for its aligned frames, their
/// alignment, the output's canvas, the other outputs and the cache.
pub(crate) struct SourceLoader {
    output: OutputId,
    /// The reader of the aligned frames, the output's canvas and each frame's alignment; `Err` with why frames can't
    /// be sources.
    frames: Result<(Plan, Canvas, Vec<AlignedSource>), String>,
    mask_loaded: bool,
    outputs: Vec<(OutputId, CacheRef)>,
    pub cache: CacheDir,
}

/// A frame retouching can paint from: its alignment's transform, gain and white balance, and its warped image cached,
/// if any.
type AlignedSource = (FrameId, Affine, f32, [f32; 3], Option<CacheRef>);

/// The key of a frame's warped image: its file and development, its alignment and the canvas.
fn aligned_key(frame_key: &str, transform: &Affine, gain: f32, white_balance: [f32; 3], canvas: &Canvas) -> String {
    let text = format!(
        "aligned, FloraStack {}\n{frame_key}\n{:?} {gain} {white_balance:?}\ncanvas {} {} {:?}",
        env!("CARGO_PKG_VERSION"),
        transform.m,
        canvas.width,
        canvas.height,
        canvas.to_master.m
    );
    format!("aligned {}", ContentHash::of(text.as_bytes()))
}

impl SourceLoader {
    pub fn new(session: &Session, command: &'static str, output: OutputId) -> Result<SourceLoader, CommandError> {
        let project = session.project()?;
        let target = project.output(output).ok_or_else(|| CommandError::NotFound { what: output.to_string() })?;
        let mut source = crate::commands::scientific::source(project, target)?;
        // Snapshots omit disposable aligned caches. Reuse a current reference only when its original input still
        // matches; load() also checks the full development/transform/canvas key before using the pixels.
        for frame in &mut source.frames {
            frame.aligned = project.frame(frame.id()).filter(|f| f.hash == frame.hash).and_then(|f| f.aligned.clone());
        }
        let frames = (|| {
            let project = &source;
            let stored = stored_alignment(project).map_err(|e| e.to_string())?.ok_or("the stack isn't aligned")?;
            let canvas = stored.canvas(output_framing(target)).map_err(|e| e.to_string())?;
            let aligned: Vec<AlignedSource> = stored
                .frames
                .iter()
                .map(|f| {
                    #[allow(clippy::cast_possible_truncation)] // A gain near 1.
                    let gain = if f.alignment.brightness_gain.is_finite() && f.alignment.brightness_gain > 0.0 {
                        f.alignment.brightness_gain as f32
                    } else {
                        1.0
                    };
                    let white_balance = crate::project::white_balance(&f.alignment);
                    (f.frame, f.alignment.transform, gain, white_balance, project.frame(f.frame).and_then(|fr| fr.aligned.clone()))
                })
                .collect();
            let ids: Vec<FrameId> = aligned.iter().map(|a| a.0).collect();
            let plan = Plan::with_state(session, command, project, &ids).map_err(|e| e.info().message)?.with_quality(florastack_raw::Quality::Output);
            Ok((plan, canvas, aligned))
        })();
        let outputs = project.outputs.iter().map(|o| (o.id(), o.image.clone())).collect();
        Ok(SourceLoader { output, frames, mask_loaded: false, outputs, cache: session.cache_dir()? })
    }

    /// Loads `source` onto the canvas of the output, which is `size` pixels. For a frame decoded rather than read
    /// from the cache, also the cache file now holding its warped image, for the project to keep.
    pub fn load(&mut self, source: Source, size: (usize, usize)) -> Result<(SourceImage, Option<(FrameId, CacheRef)>), CommandError> {
        let unavailable = |reason: String| CommandError::Unavailable { command: "retouch".to_owned(), reason };
        match source {
            Source::Output(id) => {
                if id == self.output {
                    return Err(unavailable("an output can't be its own source: pick a frame or another output".to_owned()));
                }
                let reference =
                    self.outputs.iter().find(|(o, _)| *o == id).map(|(_, r)| r.clone()).ok_or(CommandError::NotFound { what: id.to_string() })?;
                let image = read_f32(&self.cache, &reference)?;
                if (image.width(), image.height()) != size {
                    return Err(unavailable(format!(
                        "{id} is {}x{}, not the output's {}x{}: stacked with another framing",
                        image.width(),
                        image.height(),
                        size.0,
                        size.1
                    )));
                }
                Ok((SourceImage { image, coverage: None }, None))
            }
            Source::Frame(id) => {
                let key = self.frame_key(id, size)?;
                let cached = self.frames.as_ref().ok().and_then(|(_, _, aligned)| aligned.iter().find(|a| a.0 == id).and_then(|a| a.4.clone()));
                if let Some(reference) = cached.filter(|r| r.key.as_deref() == Some(key.as_str()))
                    && let Ok(image) = read_f32(&self.cache, &reference)
                {
                    return Ok((split_coverage(image)?, None));
                }
                let warped = self.warp(id, size)?;
                let Some(coverage) = &warped.coverage else { return Ok((warped, None)) };
                let joined = join_coverage(&warped.image, coverage)?;
                let reference = write_image_in(&self.cache, Area::Cache, &format!("retouch-frame-{}", id.get()), &AnyImage::F32(joined), Some(key))?;
                Ok((warped, Some((id, reference))))
            }
        }
    }

    /// The frames, the canvas and the frames' alignment, when frames can be loaded onto an output of `size`.
    fn frames(&mut self, size: (usize, usize)) -> Result<(&mut Plan, &Canvas, &[AlignedSource]), CommandError> {
        let unavailable = |reason: String| CommandError::Unavailable { command: "retouch".to_owned(), reason };
        let (plan, canvas, aligned) = self.frames.as_mut().map_err(|reason| unavailable(format!("frames can't be sources: {reason}")))?;
        if (canvas.width, canvas.height) != size {
            return Err(unavailable(format!(
                "frames can't be sources: as aligned now, they make a {}x{} canvas, but the output is {}x{}, so they were aligned or framed again since it was stacked",
                canvas.width, canvas.height, size.0, size.1
            )));
        }
        Ok((plan, canvas, aligned))
    }

    /// The key of frame `id`'s warped image on the canvas of an output of `size`.
    fn frame_key(&mut self, id: FrameId, size: (usize, usize)) -> Result<String, CommandError> {
        let (plan, canvas, aligned) = self.frames(size)?;
        let Some(&(_, transform, gain, white_balance, _)) = aligned.iter().find(|a| a.0 == id) else {
            return Err(CommandError::Unavailable {
                command: "retouch".to_owned(),
                reason: format!("{id} isn't aligned: align the stack with it first"),
            });
        };
        let index = plan.ids().iter().position(|f| *f == id).ok_or(CommandError::NotFound { what: id.to_string() })?;
        Ok(aligned_key(&plan.frame_key(index)?, &transform, gain, white_balance, canvas))
    }

    /// Frame `id`, read and developed as stacking reads it, its gain and white balance applied, and warped onto the
    /// canvas of an output of `size` with its coverage; not cached (what exporting the aligned frames writes).
    pub fn warp(&mut self, id: FrameId, size: (usize, usize)) -> Result<SourceImage, CommandError> {
        let load_mask = !self.mask_loaded;
        let (plan, canvas, aligned) = self.frames(size)?;
        let Some(&(_, transform, gain, white_balance, _)) = aligned.iter().find(|a| a.0 == id) else {
            return Err(CommandError::Unavailable {
                command: "retouch".to_owned(),
                reason: format!("{id} isn't aligned: align the stack with it first"),
            });
        };
        let canvas = *canvas;
        let index = plan.ids().iter().position(|f| *f == id).ok_or(CommandError::NotFound { what: id.to_string() })?;
        if load_mask {
            plan.load_mask()?;
        }
        let mut image = plan.read(index)?;
        self.mask_loaded = true;
        // Gains of exactly 1 change nothing, the common case, as stacking skips them too; otherwise each sample is
        // multiplied as stacking multiplies it (florastack-stack's `Gained`).
        let c = image.channels().max(1);
        let gained = florastack_stack::Gained { frame: (), gain, white_balance };
        let factors: Vec<f32> = (0..c).map(|k| gained.factor(k, c)).collect();
        #[allow(clippy::float_cmp)] // Exactly 1: a channel not corrected.
        if factors.iter().any(|&f| f != 1.0) {
            use rayon::prelude::*;
            image.par_rows_mut().for_each(|row| {
                for pixel in row.chunks_exact_mut(c) {
                    for (v, f) in pixel.iter_mut().zip(&factors) {
                        *v *= f;
                    }
                }
            });
        }
        let whole = Rect::new(0, 0, canvas.width, canvas.height);
        let covered = warp_to_canvas(&image, &transform, &canvas, whole, DEFAULT_FEATHER)?;
        Ok(SourceImage { image: covered.image, coverage: Some(covered.coverage) })
    }
}

/// `image` with `coverage` as one more channel, as a frame's warped image is cached.
fn join_coverage(image: &Image<f32>, coverage: &Image<f32>) -> Result<Image<f32>, CommandError> {
    let (w, h, c) = (image.width(), image.height(), image.channels());
    let mut data = Vec::with_capacity(w * h * (c + 1));
    for (row, cov) in image.rows().zip(coverage.rows()) {
        for (pixel, &k) in row.chunks_exact(c.max(1)).zip(cov) {
            data.extend_from_slice(pixel);
            data.push(k);
        }
    }
    Ok(Image::from_vec(w, h, c + 1, image.color_space().clone(), data).map_err(florastack_stack::StackError::from)?)
}

/// A cached warped frame split back into its pixels and its coverage, the last channel.
fn split_coverage(joined: Image<f32>) -> Result<SourceImage, CommandError> {
    let (w, h, n) = (joined.width(), joined.height(), joined.channels());
    if !matches!(n, 2 | 4) {
        return Ok(SourceImage { image: joined, coverage: None });
    }
    let mut pixels = Vec::with_capacity(w * h * (n - 1));
    let mut coverage = Vec::with_capacity(w * h);
    for row in joined.rows() {
        for pixel in row.chunks_exact(n) {
            pixels.extend_from_slice(&pixel[..n - 1]);
            coverage.push(pixel[n - 1]);
        }
    }
    let color = joined.color_space().clone();
    let image = Image::from_vec(w, h, n - 1, color, pixels).map_err(florastack_stack::StackError::from)?;
    let coverage = Image::from_vec(w, h, 1, florastack_core::ColorSpace::LINEAR, coverage).map_err(florastack_stack::StackError::from)?;
    Ok(SourceImage { image, coverage: Some(coverage) })
}

/// The strokes with the images they read: each stroke's source from `sources`.
pub(crate) fn painted<'a>(strokes: &'a [Stroke], base: &'a Image<f32>, sources: &'a [(Source, Arc<SourceImage>)]) -> Vec<Painted<'a>> {
    strokes
        .iter()
        .map(|stroke| {
            let source = stroke.source.and_then(|s| sources.iter().find(|(k, _)| *k == s)).map(|(_, image)| &**image);
            Painted { stroke, fields: Fields::whole(base, source.map(|s| &s.image), source.and_then(|s| s.coverage.as_ref())) }
        })
        .collect()
}

/// The sources `strokes` paint from that aren't in `sources` yet, loaded with `loader` into `store`'s tiles (strokes
/// whose source can't be loaded are skipped when painted, with a warning). Returns the frames' new cache files, for
/// the project.
pub(crate) fn load_tiled(
    strokes: &[Stroke],
    sources: &mut Sources,
    loader: &mut SourceLoader,
    size: (usize, usize),
    store: &Shared,
) -> Result<Vec<(FrameId, CacheRef)>, CommandError> {
    let mut cached = Vec::new();
    for source in strokes.iter().filter_map(|s| s.source) {
        if sources.iter().any(|(s, _)| *s == source) {
            continue;
        }
        match loader.load(source, size) {
            Ok((image, new)) => {
                sources.push((source, Arc::new(SourceTiles::of(store, &image)?)));
                cached.extend(new);
            }
            Err(e) => tracing::warn!("strokes from {source:?} can't be painted: {e}"),
        }
    }
    Ok(cached)
}

/// The sources `strokes` paint from that aren't in `sources` yet, loaded with `loader` (strokes whose source can't be
/// loaded are skipped when painted, with a warning). Returns the frames' new cache files, for the project.
pub(crate) fn load_sources(
    strokes: &[Stroke],
    sources: &mut Vec<(Source, Arc<SourceImage>)>,
    loader: &mut SourceLoader,
    size: (usize, usize),
) -> Vec<(FrameId, CacheRef)> {
    let mut cached = Vec::new();
    for source in strokes.iter().filter_map(|s| s.source) {
        if sources.iter().any(|(s, _)| *s == source) {
            continue;
        }
        match loader.load(source, size) {
            Ok((image, new)) => {
                sources.push((source, Arc::new(image)));
                cached.extend(new);
            }
            Err(e) => tracing::warn!("strokes from {source:?} can't be painted: {e}"),
        }
    }
    cached
}

/// The strokes whose source is among `sources` (the eraser's always): what can be painted.
pub(crate) fn paintable<T>(strokes: &[Stroke], sources: &[(Source, T)]) -> Vec<Stroke> {
    strokes.iter().filter(|s| s.source.is_none_or(|src| sources.iter().any(|(k, _)| *k == src))).cloned().collect()
}

impl Session {
    /// Brings the retouch session up to date with the project after a change of its output's layers or of the
    /// outputs: closes it when its output is gone, and paints again the region where the visible strokes changed.
    pub(crate) fn sync_retouch(&mut self) {
        let Some(mut session) = self.retouch.take() else { return };
        let Ok(project) = self.project() else {
            return;
        };
        let Some(output) = project.output(session.output) else {
            tracing::debug!("the output being retouched is gone: the retouch session closes");
            self.retouch_closed = true;
            return;
        };
        let strokes = visible_strokes(output);
        session.left_out = left_out(project, output);
        let common = session.painted.iter().zip(&strokes).take_while(|(a, b)| a == b).count();
        let (w, h) = session.size();
        let reach = |s: &Stroke| Stroke::reach(&s.dabs(), 1.0, w, h);
        let region = session.painted[common..].iter().chain(&strokes[common..]).fold(None, |r, s| union(r, reach(s)));
        if let Some(region) = region {
            let mut cached = Vec::new();
            if strokes.iter().any(|s| s.source.is_some_and(|src| session.source(src).is_none())) {
                let store = Arc::clone(&session.store);
                match SourceLoader::new(self, "retouch", session.output)
                    .and_then(|mut loader| load_tiled(&strokes, &mut session.sources, &mut loader, (w, h), &store))
                {
                    Ok(new) => cached = new,
                    Err(e) => tracing::warn!("the strokes' sources can't be loaded: {e}"),
                }
            }
            if let Err(e) = session.replay(&strokes, region) {
                tracing::warn!("the strokes can't be painted again: {e}");
            }
            session.touched(Some(region));
            session.unsaved = true;
            for (frame, reference) in cached {
                if let Err(e) = self.keep_aligned(frame, reference) {
                    tracing::debug!("the warped frame isn't kept: {e}");
                }
            }
        }
        session.painted = strokes;
        self.retouch = Some(session);
    }

    /// Keeps `reference` as frame `frame`'s warped image, then evicts the least recently used others beyond the
    /// limit ([`Session::evict_sources`]).
    pub(crate) fn keep_aligned(&mut self, frame: FrameId, reference: CacheRef) -> Result<(), CommandError> {
        self.apply_system(Edit::SetAlignedImage { frame, aligned: Some(reference) })?;
        self.used_source(frame);
        self.evict_sources();
        Ok(())
    }

    /// Notes that frame `frame`'s warped image was used: the last to be evicted.
    pub(crate) fn used_source(&mut self, frame: FrameId) {
        self.source_use.retain(|f| *f != frame);
        self.source_use.insert(0, frame);
    }

    /// Removes frames' warped images from the cache, the least recently used first (those not used since the project
    /// opened before any, in the frame list's order), until they take no more than the limit, keeping the one used
    /// last whatever its size. An evicted frame is warped again when a stroke or the source needs it, to the same
    /// pixels, so nothing painted changes. Returns the frames evicted.
    pub(crate) fn evict_sources(&mut self) -> Vec<FrameId> {
        let limit = self.host.source_limit;
        let Ok(project) = self.project() else { return Vec::new() };
        let kept: Vec<(FrameId, CacheRef)> = project.frames.iter().filter_map(|f| Some((f.id(), f.aligned.clone()?))).collect();
        let mut total: u64 = kept.iter().map(|(_, r)| r.size).fold(0, u64::saturating_add);
        if total <= limit {
            return Vec::new();
        }
        let rank = |frame: &FrameId| self.source_use.iter().position(|f| f == frame);
        let mut order: Vec<&(FrameId, CacheRef)> = kept.iter().filter(|(f, _)| self.source_use.first() != Some(f)).collect();
        // Never used this session first, then from the least recently used; a stable sort keeps the frame order.
        order.sort_by_key(|(f, _)| std::cmp::Reverse(rank(f).unwrap_or(usize::MAX)));
        let mut evicted = Vec::new();
        let root = self.cache_dir().ok().map(|c| c.path().to_path_buf());
        for (frame, reference) in order {
            if total <= limit {
                break;
            }
            evicted.push((*frame, reference.clone()));
            total = total.saturating_sub(reference.size);
        }
        let mut frames = Vec::new();
        for (frame, reference) in evicted {
            if let Err(e) = self.apply_system(Edit::SetAlignedImage { frame, aligned: None }) {
                tracing::debug!("{frame}'s warped image isn't evicted: {e}");
                continue;
            }
            // The file goes at once: a reference to it that comes back (an undo restoring the frame) only fails to
            // read, and the frame is warped again.
            if let Some(root) = &root
                && let Err(e) = florastack_core::fs::remove_file(reference.path.resolve(root))
            {
                tracing::debug!("{frame}'s evicted warped image can't be removed now: {e}");
            }
            self.source_use.retain(|f| *f != frame);
            frames.push(frame);
        }
        if !frames.is_empty() {
            tracing::debug!("evicted the warped images of {} frames, beyond the limit of {limit} bytes", frames.len());
        }
        frames
    }

    /// The limit of the frames' warped images in the cache, in bytes.
    pub(crate) fn source_limit(&self) -> u64 {
        self.host.source_limit
    }

    /// Changes the limit of the frames' warped images in the cache, evicting what is beyond it now.
    pub(crate) fn set_source_limit(&mut self, bytes: u64) -> Vec<FrameId> {
        self.host.source_limit = bytes;
        self.evict_sources()
    }

    /// Caches the retouched result, if the session has strokes the cache doesn't: when the project is saved and when
    /// the session closes, so that the output opens again with no recomputation.
    pub(crate) fn flush_retouch(&mut self) -> Result<(), CommandError> {
        let Some(session) = &self.retouch else { return Ok(()) };
        if !session.unsaved || session.live.is_some() {
            return Ok(());
        }
        let output = session.output;
        let project = self.project()?;
        let Some(target) = project.output(output) else { return Ok(()) };
        let key = render_key(target, &session.painted);
        let edit = if session.painted.is_empty() {
            Edit::SetOutputMap { output, name: map_name(RETOUCHED)?, map: None }
        } else {
            // Written a band at a time from the tiles.
            let reference = crate::project::write_float_rows(
                &self.cache_dir()?,
                &format!("retouched-{}", output.get()),
                &mut TiledRows(&session.canvas),
                false,
                Some(key),
            )?;
            Edit::SetOutputMap { output, name: map_name(RETOUCHED)?, map: Some(reference) }
        };
        self.apply_system(edit)?;
        if let Some(session) = &mut self.retouch {
            session.unsaved = false;
        }
        Ok(())
    }
}

/// A tiled image as rows for an encoder.
pub(crate) struct TiledRows<'a>(pub &'a Tiled);

impl florastack_codecs::RowSource for TiledRows<'_> {
    fn width(&self) -> usize {
        self.0.width()
    }

    fn height(&self) -> usize {
        self.0.height()
    }

    fn rows(&mut self, y: usize, rows: usize) -> Result<AnyImage, florastack_codecs::CodecError> {
        self.0.read(Rect::new(0, y, self.0.width(), rows)).map(AnyImage::F32).map_err(|e| florastack_codecs::CodecError::Read {
            path: std::path::PathBuf::from("the retouch session's tiles"),
            source: std::io::Error::other(e.to_string()),
        })
    }
}

/// A map's name, known to be valid.
pub(crate) fn map_name(name: &str) -> Result<MapName, CommandError> {
    MapName::new(name).map_err(|_| CommandError::Internal { command: "retouch".to_owned() })
}
