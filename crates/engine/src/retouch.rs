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

/// The output map that caches the retouched result, and the one that maps each point's sharpest frame.
pub(crate) const RETOUCHED: &str = "retouched";
pub(crate) const SHARPEST: &str = "sharpest";
/// How many sources stay in memory.
const KEPT_SOURCES: usize = 3;
/// In the sharpest map, a point no frame covers.
pub(crate) const NO_FRAME: u16 = u16::MAX;

/// A source on the output's canvas: its pixels and, for a frame, how much of each pixel the frame covers.
pub(crate) struct SourceImage {
    pub image: Image<f32>,
    pub coverage: Option<Image<f32>>,
}

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
    pub base: Arc<Image<f32>>,
    pub canvas: Image<f32>,
    /// The visible strokes painted on the canvas, the bottom layer's first.
    pub painted: Vec<Stroke>,
    /// The sources loaded, the most recently used first.
    pub sources: Vec<(Source, Arc<SourceImage>)>,
    /// The source strokes paint from now.
    pub current: Option<Source>,
    pub live: Option<LiveStroke>,
    pub sharpest: Option<Sharpest>,
    /// What changed since the app last asked.
    pub changes: RetouchChanges,
    /// Whether the canvas holds strokes the cached result doesn't.
    pub unsaved: bool,
}

impl RetouchSession {
    pub fn source(&self, source: Source) -> Option<Arc<SourceImage>> {
        self.sources.iter().find(|(s, _)| *s == source).map(|(_, image)| Arc::clone(image))
    }

    /// Keeps `image` as `source`, the most recently used, letting the oldest go beyond [`KEPT_SOURCES`].
    pub fn keep_source(&mut self, source: Source, image: Arc<SourceImage>) {
        self.sources.retain(|(s, _)| *s != source);
        self.sources.insert(0, (source, image));
        self.sources.truncate(KEPT_SOURCES);
    }

    /// Notes `rect` as changed.
    pub fn touched(&mut self, rect: Option<Rect>) {
        self.changes.region = union(self.changes.region, rect);
    }
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
        (self.session.canvas.width(), self.session.canvas.height())
    }

    /// The source strokes paint from now.
    pub fn source(&self) -> Option<Source> {
        self.session.current
    }

    /// Whether the session can tell each point's sharpest frame (`retouch.sharpest`).
    pub fn has_sharpest(&self) -> bool {
        self.session.sharpest.is_some()
    }

    /// The sharpest frame at output pixel `(x, y)`, when the session can tell.
    pub fn sharpest(&self, x: f64, y: f64) -> Option<FrameId> {
        self.session.sharpest.as_ref().and_then(|s| s.at(x, y)).map(|(_, f)| f)
    }

    /// `rect` of the result for display, reduced by `factor` ([`display_region`]).
    pub fn result(&self, rect: Rect, factor: usize) -> Result<(DisplayImage, Rect), ViewError> {
        display_region(&self.session.canvas, rect, factor)
    }

    /// `rect` of the source for display, reduced by `factor`; `None` without a source.
    pub fn source_pixels(&self, rect: Rect, factor: usize) -> Option<Result<(DisplayImage, Rect), ViewError>> {
        let source = self.session.source(self.session.current?)?;
        Some(display_region(&source.image, rect, factor))
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

/// A frame retouching can paint from: its alignment's transform and gain, and its warped image cached, if any.
type AlignedSource = (FrameId, Affine, f32, Option<CacheRef>);

/// The key of a frame's warped image: its file and development, its alignment and the canvas.
fn aligned_key(frame_key: &str, transform: &Affine, gain: f32, canvas: &Canvas) -> String {
    let text = format!(
        "aligned, FloraStack {}\n{frame_key}\n{:?} {gain}\ncanvas {} {} {:?}",
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
                    (f.frame, f.alignment.transform, gain, project.frame(f.frame).and_then(|fr| fr.aligned.clone()))
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
                let cached = self.frames.as_ref().ok().and_then(|(_, _, aligned)| aligned.iter().find(|a| a.0 == id).and_then(|a| a.3.clone()));
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
        let Some(&(_, transform, gain, _)) = aligned.iter().find(|a| a.0 == id) else {
            return Err(CommandError::Unavailable {
                command: "retouch".to_owned(),
                reason: format!("{id} isn't aligned: align the stack with it first"),
            });
        };
        let index = plan.ids().iter().position(|f| *f == id).ok_or(CommandError::NotFound { what: id.to_string() })?;
        Ok(aligned_key(&plan.frame_key(index)?, &transform, gain, canvas))
    }

    /// Frame `id`, read and developed as stacking reads it, its gain applied, and warped onto the canvas of an output of
    /// `size` with its coverage; not cached (what exporting the aligned frames writes).
    pub fn warp(&mut self, id: FrameId, size: (usize, usize)) -> Result<SourceImage, CommandError> {
        let load_mask = !self.mask_loaded;
        let (plan, canvas, aligned) = self.frames(size)?;
        let Some(&(_, transform, gain, _)) = aligned.iter().find(|a| a.0 == id) else {
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
        // A gain of exactly 1 changes nothing, the common case, as stacking skips it too.
        #[allow(clippy::float_cmp)] // Exactly 1: the gain of a frame not corrected.
        if gain != 1.0 {
            use rayon::prelude::*;
            image.par_rows_mut().for_each(|row| row.iter_mut().for_each(|v| *v *= gain));
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
            Painted { stroke, fields: Fields { base, source: source.map(|s| &s.image), coverage: source.and_then(|s| s.coverage.as_ref()) } }
        })
        .collect()
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
pub(crate) fn paintable(strokes: &[Stroke], sources: &[(Source, Arc<SourceImage>)]) -> Vec<Stroke> {
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
        let common = session.painted.iter().zip(&strokes).take_while(|(a, b)| a == b).count();
        let (w, h) = (session.canvas.width(), session.canvas.height());
        let reach = |s: &Stroke| Stroke::reach(&s.dabs(), 1.0, w, h);
        let region = session.painted[common..].iter().chain(&strokes[common..]).fold(None, |r, s| union(r, reach(s)));
        if let Some(region) = region {
            let mut cached = Vec::new();
            if strokes.iter().any(|s| s.source.is_some_and(|src| session.source(src).is_none())) {
                match SourceLoader::new(self, "retouch", session.output) {
                    Ok(mut loader) => cached = load_sources(&strokes, &mut session.sources, &mut loader, (w, h)),
                    Err(e) => tracing::warn!("the strokes' sources can't be loaded: {e}"),
                }
            }
            let usable = paintable(&strokes, &session.sources);
            let base = Arc::clone(&session.base);
            if let Err(e) = florastack_retouch::replay(&mut session.canvas, &base, &painted(&usable, &base, &session.sources), region) {
                tracing::warn!("the strokes can't be painted again: {e}");
            }
            session.touched(Some(region));
            session.unsaved = true;
            for (frame, reference) in cached {
                if let Err(e) = self.apply_system(Edit::SetAlignedImage { frame, aligned: Some(reference) }) {
                    tracing::debug!("the warped frame isn't kept: {e}");
                }
            }
        }
        session.painted = strokes;
        self.retouch = Some(session);
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
            let reference = write_image_in(
                &self.cache_dir()?,
                Area::Cache,
                &format!("retouched-{}", output.get()),
                &AnyImage::F32(session.canvas.clone()),
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

/// A map's name, known to be valid.
pub(crate) fn map_name(name: &str) -> Result<MapName, CommandError> {
    MapName::new(name).map_err(|_| CommandError::Internal { command: "retouch".to_owned() })
}
