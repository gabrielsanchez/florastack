//! Rendering adjustments with LightCraft's develop pipeline.
//!
//! A [`Renderer`] holds one image (linear light in Rec. 2020 primaries) and, when the output has one, the stack's depth
//! map ([`Renderer::with_depth`]); it renders any [`Adjustments`] at full size or reduced to fit a view ([`Request`]),
//! reusing the stages of its last renders that the change left alone (dragging a tone slider reruns only the
//! per-pixel stage).
//!
//! The image is handed to the pipeline as a rendered (display-referred) source, whose neutral rendering is the image
//! itself: a stacked output has already been brought into range (its tone compression), so it must not get a camera
//! look or a filmic curve. White balance is then relative, 6500 K being the image as it is.
//!
//! The masks of the stack's focus (subject, background, depth ranges) reach the pipeline as segmentation mattes, the
//! ones a file's semantic masks would be: the subject ([`crate::subject`], found in the image itself) as the person
//! matte, which the pipeline's subject and background shapes use, and each depth range, the subject's part whose depth
//! lies in it, as one of the other mattes (skin, hair, teeth, glasses, sky), which a people or sky shape selects. That
//! is why an output's masks select at most [`MAX_DEPTH_RANGES`] different depth ranges.

use std::sync::{Arc, OnceLock};

use florastack_core::{ColorSpace, Image};
use lightcraft_develop::{BrushStroke, DevelopSettings, LocalAdjustments, Mask as LcMask, MaskComponent, MaskOp, MaskShape, WbMode, WhiteBalance};
use lightcraft_geom::Point;
use lightcraft_pipeline::masks::{MatteKind, Mattes};
use lightcraft_pipeline::{OutputDepth, OutputSpace, Overlay, Quality, RenderRequest, SourceInfo, StageCache};
use lightcraft_raster::resample::{Filter, resize};
use lightcraft_raster::{Plane, Rgb32f};

use crate::error::EditError;
use crate::settings::{Adjustments, Component, Global, Local, MAX_DEPTH_RANGES, Mask, Op, Shape, Stroke};

/// How many stops white is above middle gray (0.18), from which LightCraft's luminance ranges count: their 0 to 1 spans
/// 8 stops below middle gray to 4 above it.
const WHITE_OVER_GREY: f64 = 2.473_931_188_332_412;

/// The mattes that hold depth ranges, in order, with the pipeline shape that selects each.
const DEPTH_SLOTS: [MatteKind; MAX_DEPTH_RANGES] = [MatteKind::Skin, MatteKind::Hair, MatteKind::Teeth, MatteKind::Glasses, MatteKind::Sky];

/// How large a render is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// The image's own size.
    Full,
    /// Reduced to fit in `width` by `height`, keeping the aspect ratio (never enlarged).
    Fit { width: usize, height: usize },
}

/// What to render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub size: Size,
    /// A quick render while a slider is dragged: the most expensive refinements are skipped.
    pub draft: bool,
    /// Shows this mask's region tinted red over the adjusted image.
    pub overlay: Option<u32>,
}

impl Request {
    /// The full-size render, as an export writes it.
    pub const FULL: Request = Request { size: Size::Full, draft: false, overlay: None };
}

/// One image to adjust, with what its renders reuse.
pub struct Renderer {
    source: Arc<Rgb32f>,
    /// The coverage channel of an RGBA image, which adjustments leave as it is.
    alpha: Option<Plane>,
    color: ColorSpace,
    /// The subject, found when a mask first needs it; shared with the renderers made from this one.
    subject: Arc<OnceLock<Plane>>,
    /// The stack's depth map, from 0 (the first frame) to 1 (the last).
    depth: Option<Arc<Plane>>,
    cache: StageCache,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("width", &self.source.width)
            .field("height", &self.source.height)
            .field("alpha", &self.alpha.is_some())
            .field("depth", &self.depth.is_some())
            .finish_non_exhaustive()
    }
}

impl Renderer {
    /// A renderer for `image`: linear light in Rec. 2020 primaries, with three channels or four (the fourth coverage).
    /// Its color-space tag is given to every render.
    pub fn new(image: &Image<f32>) -> Result<Renderer, EditError> {
        let channels = image.channels();
        if channels != 3 && channels != 4 {
            return Err(EditError::image(format!("adjustments need an RGB image, not one with {channels} channels")));
        }
        let (width, height) = (image.width(), image.height());
        let mut source = Rgb32f::new(width, height);
        let mut alpha = (channels == 4).then(|| Plane::new(width, height));
        for (y, row) in image.rows().enumerate() {
            let start = y * width;
            let (Some(out), Some(px)) = (source.data.get_mut(start..start + width), Some(row.chunks_exact(channels))) else {
                return Err(EditError::image("the image's rows don't match its size"));
            };
            for (x, (o, p)) in out.iter_mut().zip(px).enumerate() {
                let finite = |v: Option<&f32>| v.copied().filter(|v| v.is_finite()).unwrap_or(0.0);
                *o = [finite(p.first()), finite(p.get(1)), finite(p.get(2))];
                if let Some(a) = alpha.as_mut().and_then(|a| a.data.get_mut(start + x)) {
                    *a = finite(p.get(3));
                }
            }
        }
        Ok(Renderer {
            source: Arc::new(source),
            alpha,
            color: image.color_space().clone(),
            subject: Arc::default(),
            depth: None,
            cache: StageCache::default(),
        })
    }

    /// A renderer of the same image with the stack's depth map, for the depth masks: one channel with the image's
    /// aspect ratio, at any size (a quarter of the image's is plenty), from 0 (in focus in the stack's first frame) to
    /// 1 (its last). The image is shared, not copied.
    pub fn with_depth(&self, depth: &Image<f32>) -> Result<Renderer, EditError> {
        let (w, h) = (depth.width(), depth.height());
        // The map must cover the image: the same aspect ratio within a pixel of the map's rounding.
        let expected = h as f64 * self.source.width as f64 / self.source.height as f64;
        if depth.channels() != 1 || (w as f64 - expected).abs() > 1.0 {
            return Err(EditError::Guide { width: w, height: h });
        }
        let mut plane = Plane::new(w, h);
        for (dst, src) in plane.data.chunks_exact_mut(w).zip(depth.rows()) {
            for (d, s) in dst.iter_mut().zip(src) {
                *d = if s.is_finite() { s.clamp(0.0, 1.0) } else { 0.0 };
            }
        }
        Ok(Renderer {
            source: Arc::clone(&self.source),
            alpha: self.alpha.clone(),
            color: self.color.clone(),
            subject: Arc::clone(&self.subject),
            depth: Some(Arc::new(plane)),
            cache: StageCache::default(),
        })
    }

    /// How much each pixel belongs to the subject, from 0 to 1, at most [`crate::subject::WORKING_SIDE`] pixels on its
    /// long side ([`crate::subject`]).
    pub fn subject(&self) -> Result<Image<f32>, EditError> {
        let plane = self.subject_plane();
        Ok(Image::from_vec(plane.width, plane.height, 1, ColorSpace::LINEAR, plane.data.clone())?)
    }

    fn subject_plane(&self) -> &Plane {
        self.subject.get_or_init(|| crate::subject::subject(&self.source))
    }

    pub fn width(&self) -> usize {
        self.source.width
    }

    pub fn height(&self) -> usize {
        self.source.height
    }

    /// Whether the renderer has the stack's depth map, which the depth masks need.
    pub fn has_depth(&self) -> bool {
        self.depth.is_some()
    }

    /// Renders `adjustments`: linear light in Rec. 2020 primaries, with the source's channels and color-space tag.
    /// Neutral adjustments at full size give the image back exactly as it is.
    pub fn render(&self, adjustments: &Adjustments, request: &Request) -> Result<Image<f32>, EditError> {
        adjustments.validate()?;
        let (w, h) = self.output_size(request.size);
        if request.overlay.is_none() && (w, h) == (self.source.width, self.source.height) && adjustments.is_neutral() {
            return self.assemble(&self.source.data.iter().flatten().copied().collect::<Vec<_>>(), w, h);
        }
        let (settings, info) = self.lightcraft(adjustments)?;
        let overlay = request
            .overlay
            .and_then(|id| adjustments.masks.iter().position(|m| m.id == id))
            .and_then(|i| u16::try_from(i + 1).ok())
            .map_or(Overlay::None, |id| Overlay::Mask { id, view: lightcraft_pipeline::MaskView::Color, color: [230, 40, 40], opacity: 60 });
        let req = RenderRequest {
            max_w: w,
            max_h: h,
            quality: if request.draft { Quality::Draft } else { Quality::Full },
            apply_crop: true,
            overlay,
            space: OutputSpace::Rec2020,
            depth: if matches!(overlay, Overlay::None) { OutputDepth::F32Linear } else { OutputDepth::U8 },
            proof: None,
            window: None,
            display: None,
        };
        let rendered = lightcraft_pipeline::render_cached(&self.source, &info, &settings, &req, &self.cache);
        if let Some(deep) = rendered.deep {
            let lightcraft_pipeline::DeepSamples::F32(samples) = deep.samples else {
                return Err(EditError::image("the pipeline gave integer samples for a float render"));
            };
            return self.assemble(&samples, deep.width, deep.height);
        }
        // An overlay is drawn on the 8-bit rendition, in Rec. 2020 with its curve: back to linear light.
        let trc = OutputSpace::Rec2020.trc();
        let image = rendered.image;
        let samples: Vec<f32> = image.data.iter().flat_map(|p| [p[0], p[1], p[2]].map(|v| trc.decode(f32::from(v) / 255.0))).collect();
        self.assemble(&samples, image.width, image.height)
    }

    /// The color a color-range mask samples at `point` (fractions of the image's width and height), in `OkLab`, as
    /// [`Shape::Color`]'s samples hold it: the image's color there after the whole image's adjustments. `None`
    /// outside the image.
    pub fn sample_color(&self, adjustments: &Adjustments, point: [f64; 2]) -> Result<Option<[f64; 3]>, EditError> {
        adjustments.validate()?;
        let (settings, info) = self.lightcraft(adjustments)?;
        let (w, h) = self.output_size(Size::Fit { width: 1024, height: 1024 });
        let req = RenderRequest {
            max_w: w,
            max_h: h,
            quality: Quality::Full,
            apply_crop: true,
            overlay: Overlay::None,
            space: OutputSpace::Rec2020,
            depth: OutputDepth::U8,
            proof: None,
            window: None,
            display: None,
        };
        Ok(lightcraft_pipeline::color_range_sample(&self.source, &info, &settings, &req, Point::new(point[0], point[1])))
    }

    /// The size a render of `size` has.
    pub fn output_size(&self, size: Size) -> (usize, usize) {
        let (sw, sh) = (self.source.width, self.source.height);
        match size {
            Size::Full => (sw, sh),
            Size::Fit { width, height } => {
                let scale = (width as f64 / sw as f64).min(height as f64 / sh as f64).min(1.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Positive and at most the image's size.
                let fit = |v: usize| ((v as f64 * scale).round() as usize).clamp(1, v);
                (fit(sw), fit(sh))
            }
        }
    }

    /// An image of `w` by `h` from interleaved RGB `samples`, with the source's coverage resized to it.
    fn assemble(&self, samples: &[f32], w: usize, h: usize) -> Result<Image<f32>, EditError> {
        let Some(alpha) = &self.alpha else {
            return Ok(Image::from_vec(w, h, 3, self.color.clone(), samples.to_vec())?);
        };
        let resized;
        let alpha = if (alpha.width, alpha.height) == (w, h) {
            alpha
        } else {
            resized = resize(alpha, w, h, Filter::Bilinear);
            &resized
        };
        let mut data = Vec::new();
        data.try_reserve_exact(w.saturating_mul(h).saturating_mul(4)).map_err(|_| EditError::image("out of memory"))?;
        for (rgb, a) in samples.as_chunks::<3>().0.iter().zip(&alpha.data) {
            data.extend_from_slice(rgb);
            data.push(*a);
        }
        Ok(Image::from_vec(w, h, 4, self.color.clone(), data)?)
    }

    /// The pipeline's settings and source description for `adjustments`.
    fn lightcraft(&self, adjustments: &Adjustments) -> Result<(DevelopSettings, SourceInfo), EditError> {
        let ranges = adjustments.depth_ranges();
        let mut info = SourceInfo { raw: false, ..SourceInfo::default() };
        if adjustments.uses_subject() {
            let depth = match (&self.depth, ranges.is_empty()) {
                (Some(depth), _) => Some(depth.as_ref()),
                (None, true) => None,
                (None, false) => return Err(EditError::NoDepth),
            };
            info.mattes = Some(Arc::new(mattes(self.subject_plane(), depth, &ranges)));
        }
        let mut settings = global(&adjustments.global);
        settings.masks = adjustments
            .masks
            .iter()
            .enumerate()
            .filter(|(_, m)| m.visible)
            .map(|(i, m)| mask(m, u32::try_from(i + 1).unwrap_or(u32::MAX), &ranges))
            .collect();
        Ok((settings, info))
    }
}

/// The subject as the person matte, and each depth range, the subject's part in it, as the matte of its slot.
fn mattes(subject: &Plane, depth: Option<&Plane>, ranges: &[[f64; 3]]) -> Mattes {
    let to_u8 = |f: &dyn Fn(usize, f32) -> f32| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped to [0, 255] first.
        let data = subject.data.iter().enumerate().map(|(i, v)| (f(i, *v).clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
        lightcraft_raster::Image { width: subject.width, height: subject.height, data }
    };
    let mut mattes = Mattes::default();
    mattes.push(MatteKind::Person, to_u8(&|_, v| v));
    let Some(depth) = depth else { return mattes };
    for (range, kind) in ranges.iter().zip(DEPTH_SLOTS) {
        #[allow(clippy::cast_possible_truncation)] // Fractions from 0 to 1.
        let [from, to, feather] = range.map(|v| v as f32);
        // The depth map's resolution can differ from the subject's: sample it at the same fraction of the image.
        let (sw, sh, dw, dh) = (subject.width, subject.height, depth.width, depth.height);
        let matte = to_u8(&|i, s| {
            let (x, y) = (i % sw, i / sw);
            let d = depth.data.get((y * dh / sh.max(1)) * dw + x * dw / sw.max(1)).copied().unwrap_or(0.0);
            s * window(d, from, to, feather)
        });
        mattes.push(kind, matte);
    }
    mattes
}

/// How much `depth` lies within `[from, to]`: 1 inside, fading smoothly to 0 over `feather` beyond either end.
fn window(depth: f32, from: f32, to: f32, feather: f32) -> f32 {
    let fade = |d: f32| if feather > 0.0 { 1.0 - smoothstep((d / feather).clamp(0.0, 1.0)) } else { f32::from(u8::from(d <= 0.0)) };
    if depth < from {
        fade(from - depth)
    } else if depth > to {
        fade(depth - to)
    } else {
        1.0
    }
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Relative temperature (a mired shift around 6500 K, the image as it is) in Kelvin: the scale LightCraft gives
/// rendered images (its `rel_to_kelvin`).
fn kelvin(relative: f64) -> f64 {
    1e6 / (1e6 / 6500.0 - relative.clamp(-100.0, 100.0) * 0.8)
}

/// The pipeline's settings for the whole image's adjustments.
fn global(g: &Global) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    #[allow(clippy::float_cmp)] // Neutral means exactly 0.
    if g.temperature != 0.0 || g.tint != 0.0 {
        s.wb = WhiteBalance { mode: WbMode::Custom, temp: kelvin(g.temperature), tint: g.tint, ..s.wb };
    }
    s.light.exposure = g.exposure;
    s.light.contrast = g.contrast;
    s.light.highlights = g.highlights;
    s.light.shadows = g.shadows;
    s.light.whites = g.whites;
    s.light.blacks = g.blacks;
    s.effects.texture = g.texture;
    s.effects.clarity = g.clarity;
    s.effects.dehaze = g.dehaze;
    s.color.vibrance = g.vibrance;
    s.color.saturation = g.saturation;
    s.detail.sharpen_amount = g.sharpening;
    s.detail.sharpen_radius = g.sharpen_radius;
    s.detail.sharpen_detail = g.sharpen_detail;
    s.detail.sharpen_masking = g.sharpen_masking;
    s.detail.nr_luminance = g.noise_reduction;
    s.detail.nr_detail = g.noise_detail;
    s.detail.nr_color = g.color_noise;
    s
}

fn local(l: &Local) -> LocalAdjustments {
    LocalAdjustments {
        temp: l.temperature,
        tint: l.tint,
        exposure: l.exposure,
        contrast: l.contrast,
        highlights: l.highlights,
        shadows: l.shadows,
        whites: l.whites,
        blacks: l.blacks,
        texture: l.texture,
        clarity: l.clarity,
        dehaze: l.dehaze,
        saturation: l.saturation,
        sharpness: l.sharpness,
        noise: l.noise,
        amount: l.amount,
        ..LocalAdjustments::default()
    }
}

fn mask(m: &Mask, id: u32, ranges: &[[f64; 3]]) -> LcMask {
    LcMask {
        id,
        name: m.name.clone(),
        visible: true,
        invert: m.invert,
        components: m.components.iter().map(|c| component(c, ranges)).collect(),
        adjust: local(&m.adjust),
        refine: m.refine,
    }
}

fn point(p: [f64; 2]) -> Point {
    Point::new(p[0], p[1])
}

fn component(c: &Component, ranges: &[[f64; 3]]) -> MaskComponent {
    let op = match c.op {
        Op::Add => MaskOp::Add,
        Op::Subtract => MaskOp::Subtract,
        Op::Intersect => MaskOp::Intersect,
    };
    let shape = match &c.shape {
        // An ellipse far larger than the image, with a hard edge: everything.
        Shape::Whole => MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 100.0, ry: 100.0, angle: 0.0, feather: 0.0, invert: false },
        Shape::Linear { start, end } => MaskShape::Linear { start: point(*start), end: point(*end) },
        Shape::Radial { center, radius_x, radius_y, angle, feather } => {
            MaskShape::Radial { center: point(*center), rx: *radius_x, ry: *radius_y, angle: *angle, feather: *feather, invert: false }
        }
        Shape::Brush { strokes } => MaskShape::Brush { strokes: strokes.iter().map(stroke).collect() },
        Shape::Luminance { low, high, low_feather, high_feather } => {
            // LightCraft's scale runs from 8 stops below middle gray to 4 above it; FloraStack's from 8 stops below
            // white to white, `high` at white taking in everything brighter.
            let level = |v: f64| (8.0 * v + WHITE_OVER_GREY) / 12.0;
            let width = |v: f64| 8.0 * v / 12.0;
            let hi = if *high >= 1.0 { 1.0 } else { level(*high) };
            MaskShape::LuminanceRange { lo: level(*low), hi, lo_feather: width(*low_feather), hi_feather: width(*high_feather) }
        }
        Shape::Color { samples, refine } => MaskShape::ColorRange { samples: samples.clone(), refine: *refine },
        Shape::Subject => MaskShape::Subject,
        Shape::Background => MaskShape::Background,
        Shape::Depth { from, to, feather } => {
            #[allow(clippy::float_cmp)] // The same range, exactly: `Adjustments::depth_ranges` collected it from this shape.
            let slot = ranges.iter().position(|r| *r == [*from, *to, *feather]).and_then(|i| DEPTH_SLOTS.get(i));
            match slot {
                Some(MatteKind::Sky) => MaskShape::Sky,
                Some(kind) => {
                    let part = match kind {
                        MatteKind::Hair => "hair",
                        MatteKind::Teeth => "teeth",
                        MatteKind::Glasses => "glasses",
                        _ => "skin",
                    };
                    MaskShape::People { person: 0, parts: vec![part.to_owned()] }
                }
                // Validation allows no more ranges than slots; select nothing rather than another range.
                None => MaskShape::Linear { start: Point::new(0.0, -2.0), end: Point::new(0.0, -1.0) },
            }
        }
    };
    MaskComponent { name: None, op, invert: c.invert, shape }
}

fn stroke(s: &Stroke) -> BrushStroke {
    BrushStroke {
        points: s.points.iter().copied().map(point).collect(),
        size: s.size,
        feather: s.feather,
        flow: s.flow,
        erase: s.erase,
        ..BrushStroke::default()
    }
}
