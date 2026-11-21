//! The central viewer ([`Viewer`]): the selected frame, output or preview, drawn on the GPU through an egui-wgpu
//! paint callback ([`GpuViewer`], `viewer.wgsl`), with smooth zoom about the pointer, panning, and fit, 100% and 200%.
//!
//! - **Pixels**: the image comes from florastack-engine's display loader, linear light in sRGB's primaries as half
//!   floats, and goes up once as an `Rgba16Float` texture with a full mip chain made on the GPU from 2 x 2 averages
//!   in linear light (`levels.wgsl`): zoomed out, the image is filtered trilinearly instead of aliasing (unfiltered
//!   downscaling makes previews look noisy and mislead); from 100% up, each image pixel is a crisp square.
//! - **Display transform**: sRGB's curve, in the shader unless the window's surface applies it; values clip to
//!   `[0, 1]`, and transparent pixels show over a checkerboard. Monitor profiles are a roadmap follow-up.
//! - **Zoom**: in device pixels per image pixel, so that 100% shows every image pixel on one screen pixel whatever
//!   the display's scale. The view fits the window until the user zooms or pans.
//!
//! - **Large images**: an image larger than the device's largest texture (8192 pixels a side on most) loads at up to
//!   twice that and is tiled: a half-size overview, made on the GPU from its tiles' first mip levels, shows it at 50%
//!   and less; above that, the tiles in view, of 2048 pixels each holding two of their neighbors' on each side so that
//!   filtering shows no seams, are uploaded as they come into view, and those least recently drawn let go beyond 512
//!   MB.
//!
//! Without a GPU renderer (headless tests), the viewer shows a reduced 8-bit copy through egui's own textures.

use std::sync::Arc;

use egui::{Pos2, Rect, Sense, Vec2};
use egui_wgpu::wgpu;
use florastack_engine::view::DisplayImage;

use crate::prefs::Compare;

const VIEWER_WGSL: &str = include_str!("viewer.wgsl");
const LEVELS_WGSL: &str = include_str!("levels.wgsl");
/// The side of the reduced copy shown without a GPU renderer.
const FALLBACK_SIDE: usize = 2048;
/// The longest side images load at on the GPU, which tiles those larger than its largest texture: 16384 pixels, as
/// large as medium-format cameras' frames (an image of that side takes up to 2 GB as half floats).
const MAX_LOAD_SIDE: usize = 16384;
/// The zoom's limits, in device pixels per image pixel.
const MIN_ZOOM: f32 = 1.0 / 64.0;
const MAX_ZOOM: f32 = 64.0;

/// The zoom that fits an image of `image` pixels into `view` device pixels.
pub(crate) fn fit_zoom(view: Vec2, image: Vec2) -> f32 {
    if image.x <= 0.0 || image.y <= 0.0 || view.x <= 0.0 || view.y <= 0.0 {
        return 1.0;
    }
    (view.x / image.x).min(view.y / image.y).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// The image point at the view's center after zooming from `zoom` to `next` about a point `anchor` device pixels
/// from the view's center, which stays where it is on screen.
pub(crate) fn zoomed_center(center: Vec2, anchor: Vec2, zoom: f32, next: f32) -> Vec2 {
    let point = center + anchor / zoom;
    point - anchor / next
}

/// An image a view draws: its id, by which its texture on the GPU is kept, its pixels, the reduced 8-bit copy drawn
/// without a GPU renderer, and the regions changed since it was last drawn, for the GPU's texture.
pub(crate) struct Picture {
    id: u64,
    image: Arc<DisplayImage>,
    texture: Option<egui::TextureHandle>,
    updates: Vec<([usize; 2], Arc<DisplayImage>)>,
}

/// The next picture's id: unique in the process, so that every view's textures on the GPU stay apart.
static NEXT_PICTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Picture {
    pub(crate) fn new(image: Arc<DisplayImage>) -> Picture {
        Picture { id: NEXT_PICTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed), image, texture: None, updates: Vec::new() }
    }

    /// Its size, in its pixels.
    pub(crate) fn size(&self) -> Vec2 {
        Vec2::new(self.image.width as f32, self.image.height as f32)
    }

    /// Puts `region` at `at` (its top left corner, in the picture's pixels): into the pixels kept, the 8-bit copy and,
    /// when it is next drawn, the GPU's texture.
    pub(crate) fn update(&mut self, at: [usize; 2], region: Arc<DisplayImage>) {
        let image = Arc::make_mut(&mut self.image);
        let (w, h) = (image.width, image.height);
        let width = region.width.min(w.saturating_sub(at[0]));
        for j in 0..region.height.min(h.saturating_sub(at[1])) {
            let (src, dst) = (j * region.width * 4, ((at[1] + j) * w + at[0]) * 4);
            if let (Some(from), Some(to)) = (region.rgba.get(src..src + width * 4), image.rgba.get_mut(dst..dst + width * 4)) {
                to.copy_from_slice(from);
            }
        }
        if let Some(texture) = &mut self.texture {
            let pixels = egui::ColorImage::from_rgba_unmultiplied([region.width, region.height], &region.to_srgb8());
            texture.set_partial(at, pixels, egui::TextureOptions::LINEAR);
        }
        self.updates.push((at, region));
    }
}

/// How far from the split line, in points, a drag moves the line rather than the image.
const SPLIT_GRAB: f32 = 8.0;

/// See the module documentation.
pub struct Viewer {
    shown: Option<Picture>,
    /// The second image when two are compared.
    other: Option<Picture>,
    /// How two images are compared, the split line's place (a share of the view's width) and whether it is being
    /// dragged.
    compare: Compare,
    split: f32,
    dragging_split: bool,
    /// The two images' names, shown in the view's corners when comparing.
    labels: [String; 2],
    /// Device pixels per image pixel, now and where it is going.
    zoom: f32,
    target: f32,
    /// The image point at the view's center, in image pixels.
    center: Vec2,
    /// The point the zoom is about, in device pixels from the view's center.
    anchor: Vec2,
    /// Whether the view follows the window, fitting the image.
    fit: bool,
    /// Whether the GPU renderer is installed; the largest image side it takes.
    gpu: Option<u32>,
}

impl Viewer {
    pub fn new(gpu: Option<u32>) -> Viewer {
        Viewer {
            shown: None,
            other: None,
            compare: Compare::Split,
            split: 0.5,
            dragging_split: false,
            labels: [String::new(), String::new()],
            zoom: 1.0,
            target: 1.0,
            center: Vec2::ZERO,
            anchor: Vec2::ZERO,
            fit: true,
            gpu,
        }
    }

    /// Whether images are drawn on the GPU (the window's renderer is wgpu and the viewer's shaders compiled there).
    pub fn gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// The longest side to load images at: twice the device's largest texture (larger images are tiled), at most
    /// [`MAX_LOAD_SIDE`] unless the texture itself is larger; or the fallback's size.
    pub fn max_side(&self) -> usize {
        self.gpu.map_or(FALLBACK_SIDE, |limit| (2 * limit as usize).min(MAX_LOAD_SIDE).max(limit as usize))
    }

    /// Shows `image`, fitted.
    pub fn show(&mut self, image: Arc<DisplayImage>) {
        self.shown = Some(Picture::new(image));
        self.other = None;
        self.fit = true;
    }

    /// Shows `image` in place of the one shown, keeping the view where it is when it has the same size (the next
    /// preview of the same stack), fitted otherwise.
    pub fn replace(&mut self, image: Arc<DisplayImage>) {
        let same = self.shown.as_ref().is_some_and(|s| (s.image.width, s.image.height) == (image.width, image.height));
        self.shown = Some(Picture::new(image));
        self.other = None;
        self.fit |= !same;
    }

    /// Shows `first` and `second` compared, named `labels`; the view stays where it was when the first has the size of
    /// the image shown before (the same stack's other output), and fits otherwise.
    pub fn show_pair(&mut self, first: Arc<DisplayImage>, second: Arc<DisplayImage>, labels: [String; 2]) {
        let same = self.shown.as_ref().is_some_and(|s| (s.image.width, s.image.height) == (first.width, first.height));
        self.shown = Some(Picture::new(first));
        self.other = Some(Picture::new(second));
        self.labels = labels;
        self.fit |= !same;
    }

    pub fn set_compare(&mut self, compare: Compare) {
        if compare != self.compare {
            self.compare = compare;
            self.fit = true;
        }
    }

    /// Whether two images are compared.
    pub fn is_comparing(&self) -> bool {
        self.other.is_some()
    }

    pub fn clear(&mut self) {
        self.shown = None;
        self.other = None;
    }

    pub fn is_empty(&self) -> bool {
        self.shown.is_none()
    }

    /// Fits the image to the window.
    pub fn zoom_fit(&mut self) {
        self.fit = true;
    }

    /// Zooms to `zoom` device pixels per image pixel (1 is 100%), about the view's center.
    pub fn zoom_to(&mut self, zoom: f32) {
        self.fit = false;
        self.anchor = Vec2::ZERO;
        self.target = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// Zooms in or out by `factor`, about the view's center.
    pub fn zoom_by(&mut self, factor: f32) {
        self.zoom_to(self.target * factor);
    }

    /// The zoom now, for the status line.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Draws the viewer in `ui`'s remaining space and handles its input.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let ppp = ui.ctx().pixels_per_point();
        let Some(size) = self.shown.as_ref().map(|s| Vec2::new(s.image.width as f32, s.image.height as f32)) else { return };
        let side_by_side = self.other.is_some() && self.compare == Compare::SideBySide;
        // Side by side, each image has half the view, with a gap between them.
        let views: Vec<Rect> = if side_by_side {
            let half = (rect.width() - 2.0) / 2.0;
            vec![
                Rect::from_min_size(rect.min, Vec2::new(half, rect.height())),
                Rect::from_min_size(rect.min + Vec2::new(half + 2.0, 0.0), Vec2::new(half, rect.height())),
            ]
        } else {
            vec![rect]
        };
        let view = views.first().map_or(rect, |v| *v);
        if self.fit {
            let z = fit_zoom(view.size() * ppp, size);
            (self.zoom, self.target, self.center, self.anchor) = (z, z, size / 2.0, Vec2::ZERO);
        }
        let split_x = rect.left() + rect.width() * self.split;
        let splitting = self.other.is_some() && self.compare == Compare::Split;
        if response.drag_started() {
            self.dragging_split = splitting && response.interact_pointer_pos().is_some_and(|p| (p.x - split_x).abs() <= SPLIT_GRAB);
        }
        if response.drag_stopped() {
            self.dragging_split = false;
        }
        // Dragging moves the split line when it was grabbed, and pans otherwise; the wheel or a pinch zooms about the
        // pointer.
        if response.dragged() {
            if self.dragging_split {
                if let Some(p) = response.interact_pointer_pos() {
                    self.split = ((p.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
                }
            } else {
                self.fit = false;
                self.center -= response.drag_delta() * ppp / self.zoom;
            }
        }
        if splitting && response.hover_pos().is_some_and(|p| (p.x - split_x).abs() <= SPLIT_GRAB) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if response.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.0015).exp();
            if (factor - 1.0).abs() > 1e-4 {
                self.fit = false;
                if let Some(pointer) = response.hover_pos() {
                    let under = views.iter().find(|v| v.contains(pointer)).copied().unwrap_or(view);
                    self.anchor = (pointer - under.center()) * ppp;
                }
                self.target = (self.target * factor).clamp(MIN_ZOOM, MAX_ZOOM);
            }
        }
        if response.double_clicked() && !self.dragging_split {
            self.fit = true;
        }
        // The zoom eases toward its target, in proportion, about its anchor.
        if (self.target / self.zoom).ln().abs() > 1e-3 {
            let dt = ui.input(|i| i.stable_dt).min(0.1);
            let next = self.zoom * (self.target / self.zoom).powf((dt * 14.0).min(1.0));
            self.center = zoomed_center(self.center, self.anchor, self.zoom, next);
            self.zoom = next;
            ui.ctx().request_repaint();
        } else if (self.zoom - self.target).abs() > 0.0 {
            self.center = zoomed_center(self.center, self.anchor, self.zoom, self.target);
            self.zoom = self.target;
        }
        // What each image is drawn in, and clipped to: side by side, each its half; split, the first whole and the
        // second right of the line.
        let second_view = if side_by_side { views.get(1).copied().unwrap_or(rect) } else { rect };
        let second_clip = if splitting { Rect::from_min_max(Pos2::new(split_x, rect.top()), rect.max) } else { second_view };
        let (zoom, center) = (self.zoom, self.center);
        let ctx = ui.ctx().clone();
        let gpu = self.gpu.is_some();
        for (slot, (shown, area, clip)) in
            [(self.shown.as_mut(), view, view), (self.other.as_mut(), second_view, second_clip)].into_iter().enumerate()
        {
            let Some(shown) = shown else { continue };
            draw(ui, &ctx, gpu, slot, shown, (area, clip), zoom, center, 1.0);
        }
        if self.other.is_some() {
            let painter = ui.painter().with_clip_rect(rect);
            let font = egui::FontId::proportional(13.0);
            let (text, back) = (egui::Color32::WHITE, egui::Color32::from_black_alpha(150));
            let label = |at: Pos2, align: egui::Align2, name: &str| {
                let galley = painter.layout_no_wrap(name.to_owned(), font.clone(), text);
                let r = align.anchor_size(at, galley.size()).expand(4.0);
                painter.rect_filled(r, 3.0, back);
                painter.galley(r.min + Vec2::splat(4.0), galley, text);
            };
            label(view.left_top() + Vec2::splat(8.0), egui::Align2::LEFT_TOP, &self.labels[0]);
            label(second_view.right_top() + Vec2::new(-8.0, 8.0), egui::Align2::RIGHT_TOP, &self.labels[1]);
            if splitting {
                painter.vline(split_x, rect.y_range(), egui::Stroke::new(2.0, egui::Color32::WHITE));
                painter.circle_filled(Pos2::new(split_x, rect.center().y), 6.0, egui::Color32::WHITE);
            }
        }
    }
}

/// Draws `shown` in `area` (whose center shows image point `center` at `zoom`), clipped to `clip`, brightened `boost`
/// times: on the GPU through uniform `slot`, or through egui with a reduced copy, which isn't brightened.
#[allow(clippy::too_many_arguments)] // Where and how one image is drawn.
pub(crate) fn draw(
    ui: &egui::Ui,
    ctx: &egui::Context,
    gpu: bool,
    slot: usize,
    shown: &mut Picture,
    (area, clip): (Rect, Rect),
    zoom: f32,
    center: Vec2,
    boost: f32,
) {
    let painter = ui.painter().with_clip_rect(clip.intersect(ui.clip_rect()));
    if gpu {
        let updates = std::mem::take(&mut shown.updates);
        let paint = Paint { slot, id: shown.id, image: Arc::clone(&shown.image), updates, rect: area, zoom, center, boost };
        painter.add(egui_wgpu::Callback::new_paint_callback(area, paint));
        return;
    }
    shown.updates.clear();
    let image = Arc::clone(&shown.image);
    let texture = shown.texture.get_or_insert_with(|| {
        let pixels = egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.to_srgb8());
        ctx.load_texture(format!("viewer {slot}"), pixels, egui::TextureOptions::LINEAR)
    });
    let ppp = ctx.pixels_per_point();
    let size = Vec2::new(image.width as f32, image.height as f32);
    let middle = area.center() - (center - size / 2.0) * zoom / ppp;
    let at = Rect::from_center_size(middle, size * zoom / ppp);
    painter.image(texture.id(), at, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), egui::Color32::WHITE);
}

/// The viewer's GPU resources, kept in egui-wgpu's callback resources. See the module documentation.
pub struct GpuViewer {
    pipeline: wgpu::RenderPipeline,
    levels: wgpu::RenderPipeline,
    trilinear: wgpu::Sampler,
    pixels: wgpu::Sampler,
    bilinear: wgpu::Sampler,
    /// Whether the shader applies sRGB's curve (the surface doesn't).
    encode: bool,
    /// The largest texture side: a larger image is tiled.
    limit: u32,
    /// The images uploaded, by their ids, the most recently used last.
    uploaded: Vec<Uploaded>,
    /// One for each image drawn at once (two when comparing): what it draws this frame.
    slots: Vec<Slot>,
    /// Counts the frames prepared, for the tiles least recently drawn.
    frame: u64,
}

/// A texture with its mip levels, and its view.
struct Layer {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// An image uploaded: whole when it fits in a texture; otherwise a half-size overview, made from its tiles, for the
/// zoomed-out view, and its tiles at full size, uploaded as they come into view at more than 50%.
struct Uploaded {
    id: u64,
    /// The whole image, or its overview.
    whole: Layer,
    /// The overview's size in image pixels (twice its own); the image's size when it isn't tiled.
    covers: [u32; 2],
    /// The tiles, none when the image isn't tiled.
    tiles: Vec<Tile>,
}

/// A tile: the image pixels it draws (its core, `[x, y, w, h]`), the ones its texture holds (the core and up to
/// [`BORDER`] pixels of its neighbors on each side, so that filtering across its edges needs nothing it lacks), and
/// its texture once uploaded, with the frame it was last drawn in.
struct Tile {
    core: [u32; 4],
    held: [u32; 4],
    layer: Option<Layer>,
    drawn: u64,
}

/// What a slot draws this frame: a quad for each texture, each with its uniform and its bind group.
struct Slot {
    uniforms: Vec<wgpu::Buffer>,
    drawn: Vec<wgpu::BindGroup>,
}

/// The uniform's size: the 20 floats of `View` in viewer.wgsl.
const UNIFORM_BYTES: u64 = 80;
/// How many images can be drawn at once, and how many stay uploaded.
const SLOTS: usize = 2;
const KEPT_TEXTURES: usize = 4;
/// A tile's side at most, and how many pixels of its neighbors it holds on each side: two, so that its first mip
/// level, which tiles use down to 50%, still has one.
const TILE: u32 = 2048;
const BORDER: u32 = 2;
/// The bytes of tiles (with their mip levels) an image keeps uploaded beyond those drawn now.
const TILE_BUDGET: u64 = 512 << 20;

/// The tiles of an image of `width` x `height` pixels for textures of `limit` pixels a side at most: cores of
/// [`TILE`] pixels or less, even and at even places, so that each tile's first mip level averages the same 2 x 2
/// blocks the overview does.
fn tiles(width: u32, height: u32, limit: u32) -> Vec<Tile> {
    let side = TILE.min(limit.saturating_sub(2 * BORDER + 1)) & !1;
    let side = side.max(2);
    let mut tiles = Vec::new();
    for y in (0..height).step_by(side as usize) {
        for x in (0..width).step_by(side as usize) {
            let (w, h) = (side.min(width - x), side.min(height - y));
            let (hx, hy) = (x.saturating_sub(BORDER), y.saturating_sub(BORDER));
            let held = [hx, hy, (x + w + BORDER).min(width) - hx, (y + h + BORDER).min(height) - hy];
            tiles.push(Tile { core: [x, y, w, h], held, layer: None, drawn: 0 });
        }
    }
    tiles
}

/// A texture's bytes with its mip levels: a third more than its first.
fn layer_bytes(layer: &Layer) -> u64 {
    let size = layer.texture.size();
    u64::from(size.width) * u64::from(size.height) * 8 * 4 / 3
}

impl GpuViewer {
    /// Makes the viewer's resources for `state`'s device and surface and keeps them in its renderer; returns the
    /// largest texture side it takes, or `None` when the viewer's shaders don't compile there (the viewer then draws
    /// through egui). The device then logs its errors instead of panicking, as wgpu does by default: a crash loses
    /// work.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn install(state: &egui_wgpu::RenderState) -> Option<u32> {
        state.device.on_uncaptured_error(Arc::new(|e: wgpu::Error| tracing::error!("the display's GPU reported an error: {e}")));
        let viewer = GpuViewer::new(&state.device, state.target_format)?;
        let limit = viewer.limit;
        state.renderer.write().callback_resources.insert(viewer);
        Some(limit)
    }

    /// In a browser built with threads, wgpu's objects can't be shared between threads, which egui's renderer asks of
    /// what it keeps: the viewer draws through egui there.
    #[cfg(target_arch = "wasm32")]
    pub fn install(_state: &egui_wgpu::RenderState) -> Option<u32> {
        None
    }

    /// The viewer's resources on `device`, drawing to textures of the format `target`; `None` when its shaders don't
    /// compile there.
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat) -> Option<GpuViewer> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = |label, source: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(source.into()) })
        };
        let (viewer, levels) = (module("viewer", VIEWER_WGSL), module("viewer levels", LEVELS_WGSL));
        let pipeline_for = |label, shader: &wgpu::ShaderModule, vs, fs, format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: None,
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some(vs),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some(fs),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = pipeline_for("viewer", &viewer, "vs_image", "fs_image", target);
        let levels = pipeline_for("viewer levels", &levels, "vs_level", "fs_level", wgpu::TextureFormat::Rgba16Float);
        let sampler = |label, filter, mipmap| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter: mipmap,
                ..Default::default()
            })
        };
        let resources = GpuViewer {
            pipeline,
            levels,
            trilinear: sampler("viewer trilinear", wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Linear),
            pixels: sampler("viewer pixels", wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Nearest),
            bilinear: sampler("viewer bilinear", wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Nearest),
            encode: !target.is_srgb(),
            limit: device.limits().max_texture_dimension_2d,
            uploaded: Vec::new(),
            slots: (0..SLOTS).map(|_| Slot { uniforms: Vec::new(), drawn: Vec::new() }).collect(),
            frame: 0,
        };
        if let Some(e) = pollster::block_on(scope.pop()) {
            tracing::warn!("the viewer's shaders don't compile on this GPU ({e}): images are drawn without them");
            return None;
        }
        Some(resources)
    }

    /// The same viewer, tiling images larger than `limit` pixels a side (at most the device's own limit), as tests
    /// do to tile small images.
    #[must_use]
    pub fn with_limit(mut self, limit: u32) -> GpuViewer {
        self.limit = limit.clamp(2 * BORDER + 3, self.limit);
        self
    }

    /// Image `id`, uploaded first when it isn't yet (as the most recently used, the oldest beyond those kept let go).
    fn uploaded(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        id: u64,
        image: &DisplayImage,
    ) -> Option<usize> {
        let uploaded = match self.uploaded.iter().position(|u| u.id == id) {
            Some(i) => self.uploaded.remove(i),
            None => self.upload(device, queue, encoder, id, image)?,
        };
        self.uploaded.push(uploaded);
        if self.uploaded.len() > KEPT_TEXTURES {
            self.uploaded.remove(0);
        }
        Some(self.uploaded.len() - 1)
    }

    /// Uploads `image`: whole when it fits in a texture; otherwise its overview, made from each tile uploaded in turn
    /// (the tiles themselves are uploaded again when drawn).
    fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        id: u64,
        image: &DisplayImage,
    ) -> Option<Uploaded> {
        let (w, h) = (u32::try_from(image.width).ok()?.max(1), u32::try_from(image.height).ok()?.max(1));
        if image.rgba.len() < image.width * image.height * 4 {
            return None;
        }
        if w <= self.limit && h <= self.limit {
            let whole = self.layer(device, queue, encoder, image, [0, 0, w, h], false);
            return Some(Uploaded { id, whole, covers: [w, h], tiles: Vec::new() });
        }
        // Twice the limit at most (the loader's), so that the overview fits.
        let (ow, oh) = ((w / 2).clamp(1, self.limit), (h / 2).clamp(1, self.limit));
        let overview = GpuViewer::texture(device, "viewer overview", ow, oh);
        let tiles = tiles(w, h, self.limit);
        for tile in &tiles {
            let layer = self.layer(device, queue, encoder, image, tile.held, true);
            let [x, y, cw, ch] = tile.core;
            let (from, to) = ([(x - tile.held[0]) / 2, (y - tile.held[1]) / 2], [x / 2, y / 2]);
            let (cw, ch) = ((cw / 2).min(ow.saturating_sub(to[0])), (ch / 2).min(oh.saturating_sub(to[1])));
            if cw == 0 || ch == 0 {
                continue;
            }
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &layer.texture,
                    mip_level: 1,
                    origin: wgpu::Origin3d { x: from[0], y: from[1], z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &overview,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: to[0], y: to[1], z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
            );
        }
        self.levels(device, encoder, &overview);
        let view = overview.create_view(&wgpu::TextureViewDescriptor::default());
        Some(Uploaded { id, whole: Layer { texture: overview, view }, covers: [2 * ow, 2 * oh], tiles })
    }

    /// An `Rgba16Float` texture of `w` x `h` pixels with every mip level.
    fn texture(device: &wgpu::Device, label: &str, w: u32, h: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 32 - w.max(h).leading_zeros(),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
    }

    /// Uploads region `[x, y, w, h]` of `image` as a mipmapped texture, its levels made by `encoder`. With `even`,
    /// the texture's sides are made even by repeating the region's last column and row, so that its first mip level
    /// averages whole 2 x 2 blocks.
    fn layer(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        image: &DisplayImage,
        [x, y, w, h]: [u32; 4],
        even: bool,
    ) -> Layer {
        let (tw, th) = if even { (w.next_multiple_of(2), h.next_multiple_of(2)) } else { (w, h) };
        let texture = GpuViewer::texture(device, "viewer image", tw, th);
        let write = |at: [u32; 2], from: [u32; 2], size: [u32; 2]| {
            write_region(queue, &texture, image, at, from, size);
        };
        write([0, 0], [x, y], [w, h]);
        if tw > w {
            write([w, 0], [x + w - 1, y], [1, h]);
        }
        if th > h {
            write([0, h], [x, y + h - 1], [w, 1]);
            if tw > w {
                write([w, h], [x + w - 1, y + h - 1], [1, 1]);
            }
        }
        self.levels(device, encoder, &texture);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Layer { texture, view }
    }

    /// Writes `updates` (regions at their top left corners) into image `id`'s textures, if it is uploaded, and makes
    /// their levels again: the whole image's, or the overview's (from `image`, which holds the updates already) and
    /// the uploaded tiles' they reach.
    fn update(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        id: u64,
        image: &DisplayImage,
        updates: &[([usize; 2], Arc<DisplayImage>)],
    ) {
        let Some(uploaded) = self.uploaded.iter().find(|u| u.id == id) else { return };
        let mut written = false;
        let mut tiles_written = vec![false; uploaded.tiles.len()];
        for (at, region) in updates {
            let (Ok(x), Ok(y), Ok(w), Ok(h)) =
                (u32::try_from(at[0]), u32::try_from(at[1]), u32::try_from(region.width), u32::try_from(region.height))
            else {
                continue;
            };
            if w == 0 || h == 0 || x.saturating_add(w) as usize > image.width || y.saturating_add(h) as usize > image.height {
                continue;
            }
            if uploaded.tiles.is_empty() {
                write_region(queue, &uploaded.whole.texture, region, [x, y], [0, 0], [w, h]);
                written = true;
                continue;
            }
            // The overview's 2 x 2 averages over the region, widened to whole blocks.
            let size = uploaded.whole.texture.size();
            let (bx, by) = (x / 2, y / 2);
            let (bw, bh) = ((x + w).div_ceil(2).min(size.width) - bx, (y + h).div_ceil(2).min(size.height) - by);
            if bw > 0 && bh > 0 {
                let reduced = halve(image, [bx, by, bw, bh]);
                write_region(queue, &uploaded.whole.texture, &reduced, [bx, by], [0, 0], [bw, bh]);
                written = true;
            }
            for (tile, touched) in uploaded.tiles.iter().zip(&mut tiles_written) {
                let Some(layer) = &tile.layer else { continue };
                let [hx, hy, hw, hh] = tile.held;
                let (x0, y0, x1, y1) = (x.max(hx), y.max(hy), (x + w).min(hx + hw), (y + h).min(hy + hh));
                if x0 < x1 && y0 < y1 {
                    write_region(queue, &layer.texture, image, [x0 - hx, y0 - hy], [x0, y0], [x1 - x0, y1 - y0]);
                    *touched = true;
                }
            }
        }
        if written {
            self.levels(device, encoder, &uploaded.whole.texture);
        }
        for (tile, touched) in uploaded.tiles.iter().zip(tiles_written) {
            if let (Some(layer), true) = (&tile.layer, touched) {
                self.levels(device, encoder, &layer.texture);
            }
        }
    }

    /// Makes `texture`'s mip levels from its first, each level the 2 x 2 averages of the one above.
    fn levels(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture) {
        let levels = texture.mip_level_count();
        let level_view =
            |level| texture.create_view(&wgpu::TextureViewDescriptor { base_mip_level: level, mip_level_count: Some(1), ..Default::default() });
        for level in 1..levels {
            let (source, target) = (level_view(level - 1), level_view(level));
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("viewer level"),
                layout: &self.levels.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&source) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.bilinear) },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewer level"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.levels);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Which of image `index`'s textures to draw at `zoom` over the image pixels `visible` (`[x0, y0, x1, y1]`), each
    /// with the image pixels its quad covers and the part of the texture it shows (`[x, y, w, h]` and `[u0, v0, u1,
    /// v1]`): the whole image or its overview, or the tiles in view at more than 50%, uploaded first when they aren't.
    fn quads(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        (index, image): (usize, &DisplayImage),
        zoom: f32,
        visible: [f32; 4],
    ) -> Vec<(usize, [f32; 4], [f32; 4])> {
        const WHOLE: usize = usize::MAX;
        let frame = self.frame;
        let Some(uploaded) = self.uploaded.get(index) else { return Vec::new() };
        let covers = [0.0, 0.0, uploaded.covers[0] as f32, uploaded.covers[1] as f32];
        if uploaded.tiles.is_empty() || zoom <= 0.5 {
            return vec![(WHOLE, covers, [0.0, 0.0, 1.0, 1.0])];
        }
        let wanted: Vec<usize> = uploaded
            .tiles
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                let [x, y, w, h] = t.core.map(|v| v as f32);
                x < visible[2] && y < visible[3] && x + w > visible[0] && y + h > visible[1]
            })
            .map(|(i, _)| i)
            .collect();
        for &i in &wanted {
            let missing = self.uploaded.get(index).and_then(|u| u.tiles.get(i)).is_some_and(|t| t.layer.is_none());
            if missing && let Some(held) = self.uploaded.get(index).and_then(|u| u.tiles.get(i)).map(|t| t.held) {
                let layer = self.layer(device, queue, encoder, image, held, true);
                if let Some(tile) = self.uploaded.get_mut(index).and_then(|u| u.tiles.get_mut(i)) {
                    tile.layer = Some(layer);
                }
            }
        }
        let Some(uploaded) = self.uploaded.get_mut(index) else { return Vec::new() };
        let mut quads = Vec::new();
        for &i in &wanted {
            let Some(tile) = uploaded.tiles.get_mut(i) else { continue };
            tile.drawn = frame;
            let Some(layer) = &tile.layer else { continue };
            let size = layer.texture.size();
            let ([x, y, w, h], [hx, hy, _, _]) = (tile.core, tile.held);
            let (u0, v0) = ((x - hx) as f32 / size.width as f32, (y - hy) as f32 / size.height as f32);
            let (u1, v1) = (u0 + w as f32 / size.width as f32, v0 + h as f32 / size.height as f32);
            quads.push((i, [x as f32, y as f32, w as f32, h as f32], [u0, v0, u1, v1]));
        }
        // The tiles least recently drawn go beyond the budget, never those drawn now.
        let mut kept: u64 = uploaded.tiles.iter().filter(|t| t.drawn != frame).filter_map(|t| t.layer.as_ref()).map(layer_bytes).sum();
        while kept > TILE_BUDGET {
            let Some(oldest) = uploaded.tiles.iter_mut().filter(|t| t.layer.is_some() && t.drawn != frame).min_by_key(|t| t.drawn) else { break };
            kept = kept.saturating_sub(oldest.layer.take().as_ref().map_or(0, layer_bytes));
        }
        quads
    }
}

/// Writes the `size` pixels of `from` at `from_at` into `texture`'s first level at `at`.
fn write_region(queue: &wgpu::Queue, texture: &wgpu::Texture, from: &DisplayImage, at: [u32; 2], from_at: [u32; 2], size: [u32; 2]) {
    let Ok(row) = u32::try_from(from.width) else { return };
    let offset = (u64::from(from_at[1]) * u64::from(row) + u64::from(from_at[0])) * 8;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x: at[0], y: at[1], z: 0 }, aspect: wgpu::TextureAspect::All },
        bytemuck::cast_slice(&from.rgba),
        wgpu::TexelCopyBufferLayout { offset, bytes_per_row: Some(row * 8), rows_per_image: Some(size[1]) },
        wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
    );
}

/// The 2 x 2 averages, in linear light, of `image`'s blocks `[x, y, w, h]` (in blocks), as the overview holds them;
/// a block cut by the image's edge averages the pixels it has.
fn halve(image: &DisplayImage, [x, y, w, h]: [u32; 4]) -> DisplayImage {
    let (w, h) = (w as usize, h as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for by in y as usize..y as usize + h {
        for bx in x as usize..x as usize + w {
            let mut sum = [0.0f32; 4];
            let mut count = 0.0;
            for (px, py) in [(2 * bx, 2 * by), (2 * bx + 1, 2 * by), (2 * bx, 2 * by + 1), (2 * bx + 1, 2 * by + 1)] {
                if px >= image.width || py >= image.height {
                    continue;
                }
                let at = (py * image.width + px) * 4;
                for (c, s) in sum.iter_mut().enumerate() {
                    *s += image.rgba.get(at + c).map_or(0.0, |&b| half::f16::from_bits(b).to_f32());
                }
                count += 1.0;
            }
            rgba.extend(sum.map(|s| half::f16::from_f32(s / f32::max(count, 1.0)).to_bits()));
        }
    }
    DisplayImage { width: w, height: h, rgba, alpha: image.alpha }
}

/// One frame's drawing of an image of the viewer: which, through which slot, and where it lies in the view.
struct Paint {
    slot: usize,
    id: u64,
    image: Arc<DisplayImage>,
    /// Regions of the image changed since it was last drawn, at their top left corners.
    updates: Vec<([usize; 2], Arc<DisplayImage>)>,
    /// How many times brighter it is shown.
    boost: f32,
    /// The view, in points.
    rect: Rect,
    zoom: f32,
    center: Vec2,
}

impl egui_wgpu::CallbackTrait for Paint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(viewer) = resources.get_mut::<GpuViewer>() else { return Vec::new() };
        viewer.frame += 1;
        viewer.update(device, queue, encoder, self.id, &self.image, &self.updates);
        let Some(index) = viewer.uploaded(device, queue, encoder, self.id, &self.image) else { return Vec::new() };
        // Clip space from image pixels: the view's center shows `center`, `zoom` device pixels to an image pixel.
        let ppp = screen.pixels_per_point;
        let (w, h) = (self.rect.width() * ppp, self.rect.height() * ppp);
        let (sx, sy) = (2.0 * self.zoom / w.max(1.0), -2.0 * self.zoom / h.max(1.0));
        let (ox, oy) = (-self.center.x * sx, -self.center.y * sy);
        let half_view = Vec2::new(w, h) / (2.0 * self.zoom.max(1e-6));
        let visible = [self.center.x - half_view.x, self.center.y - half_view.y, self.center.x + half_view.x, self.center.y + half_view.y];
        let quads = viewer.quads(device, queue, encoder, (index, &self.image), self.zoom, visible);
        let origin = self.rect.min * ppp;
        let nearest = if self.zoom >= 1.0 { 1.0 } else { 0.0 };
        let encode = if viewer.encode { 1.0 } else { 0.0 };
        let mut drawn = Vec::with_capacity(quads.len());
        for (n, (texture, quad, uvs)) in quads.into_iter().enumerate() {
            let Some(uploaded) = viewer.uploaded.get(index) else { break };
            let layer = match uploaded.tiles.get(texture) {
                Some(tile) => tile.layer.as_ref(),
                None => Some(&uploaded.whole),
            };
            let Some(layer) = layer else { continue };
            let Some(slot) = viewer.slots.get_mut(self.slot) else { break };
            while slot.uniforms.len() <= n {
                slot.uniforms.push(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("viewer view"),
                    size: UNIFORM_BYTES,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
            let Some(uniform) = slot.uniforms.get(n) else { break };
            let view: [f32; 20] = [
                sx,
                sy,
                ox,
                oy,
                origin.x,
                origin.y,
                (8.0 * ppp).round(),
                encode,
                nearest,
                self.boost,
                0.0,
                0.0,
                quad[0],
                quad[1],
                quad[2],
                quad[3],
                uvs[0],
                uvs[1],
                uvs[2],
                uvs[3],
            ];
            queue.write_buffer(uniform, 0, bytemuck::cast_slice(&view));
            drawn.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("viewer image"),
                layout: &viewer.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&layer.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&viewer.trilinear) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&viewer.pixels) },
                ],
            }));
        }
        if let Some(slot) = viewer.slots.get_mut(self.slot) {
            slot.drawn = drawn;
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        let Some(viewer) = resources.get::<GpuViewer>() else { return };
        let Some(slot) = viewer.slots.get(self.slot) else { return };
        pass.set_pipeline(&viewer.pipeline);
        for group in &slot.drawn {
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn fitting_shows_the_whole_image() {
        assert_eq!(fit_zoom(Vec2::new(1000.0, 500.0), Vec2::new(6000.0, 4000.0)), 0.125);
        assert_eq!(fit_zoom(Vec2::new(1000.0, 800.0), Vec2::new(100.0, 100.0)), 8.0);
        assert_eq!(fit_zoom(Vec2::ZERO, Vec2::new(100.0, 100.0)), 1.0, "nothing to fit into");
        assert_eq!(fit_zoom(Vec2::new(10.0, 10.0), Vec2::new(1e9, 1e9)), MIN_ZOOM);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let (center, anchor) = (Vec2::new(500.0, 400.0), Vec2::new(120.0, -40.0));
        let under = center + anchor / 0.5;
        let next = zoomed_center(center, anchor, 0.5, 2.0);
        assert!((next + anchor / 2.0 - under).length() < 1e-4, "the same image point stays at the pointer");
        assert_eq!(zoomed_center(center, Vec2::ZERO, 1.0, 4.0), center, "about the center, the center stays");
    }

    /// A device on any adapter (`WGPU_ADAPTER_NAME` picks one by name), or why there is none.
    pub(crate) fn device() -> Result<(wgpu::Device, wgpu::Queue), String> {
        let backends = wgpu::Backends::VULKAN | wgpu::Backends::METAL | wgpu::Backends::DX12;
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let adapters = pollster::block_on(instance.enumerate_adapters(backends));
        let wanted = std::env::var("WGPU_ADAPTER_NAME").ok().map(|n| n.to_lowercase());
        let adapter = adapters
            .into_iter()
            .find(|a| wanted.as_ref().is_none_or(|n| a.get_info().name.to_lowercase().contains(n)))
            .ok_or("no Metal, Vulkan or DX12 adapter")?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).map_err(|e| e.to_string())
    }

    /// Draws `image` into a `size` x `size` `Rgba8Unorm` target (the shader applies sRGB's curve) at `zoom`, centered
    /// on `center`, and reads the target back.
    fn draw(device: &wgpu::Device, queue: &wgpu::Queue, image: DisplayImage, size: u32, zoom: f32, center: Vec2) -> Vec<u8> {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(size as f32));
        render(device, queue, &[(Paint { slot: 0, id: 1, image: Arc::new(image), updates: Vec::new(), boost: 1.0, rect, zoom, center }, None)], size)
    }

    /// Draws `paints` in order into a `size` x `size` `Rgba8Unorm` target, each in the viewport of its rect and clipped
    /// to its scissor `[x, y, w, h]` when it has one, as egui-wgpu draws a frame's callbacks: every `prepare` first,
    /// then every `paint`; and reads the target back.
    fn render(device: &wgpu::Device, queue: &wgpu::Queue, paints: &[(Paint, Option<[u32; 4]>)], size: u32) -> Vec<u8> {
        render_with(device, queue, paints, size, None)
    }

    /// [`render`], tiling images larger than `limit` pixels a side when there is one.
    fn render_with(device: &wgpu::Device, queue: &wgpu::Queue, paints: &[(Paint, Option<[u32; 4]>)], size: u32, limit: Option<u32>) -> Vec<u8> {
        render_in(device, queue, &mut resources(device, limit), paints, size)
    }

    /// The viewer's resources for an `Rgba8Unorm` target, tiling images larger than `limit` when there is one.
    fn resources(device: &wgpu::Device, limit: Option<u32>) -> egui_wgpu::CallbackResources {
        let mut resources = egui_wgpu::CallbackResources::default();
        let viewer = GpuViewer::new(device, wgpu::TextureFormat::Rgba8Unorm).expect("the viewer's shaders compile");
        resources.insert(match limit {
            Some(limit) => viewer.with_limit(limit),
            None => viewer,
        });
        resources
    }

    /// [`render`] with `resources` kept from earlier draws.
    fn render_in(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &mut egui_wgpu::CallbackResources,
        paints: &[(Paint, Option<[u32; 4]>)],
        size: u32,
    ) -> Vec<u8> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [size, size], pixels_per_point: 1.0 };
        for (paint, _) in paints {
            let _ = egui_wgpu::CallbackTrait::prepare(paint, device, queue, &screen, &mut encoder, resources);
        }
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            for (paint, scissor) in paints {
                let [x, y, w, h] = scissor.unwrap_or([0, 0, size, size]);
                pass.set_scissor_rect(x, y, w, h);
                let r = paint.rect;
                pass.set_viewport(r.min.x, r.min.y, r.width(), r.height(), 0.0, 1.0);
                let info =
                    egui::PaintCallbackInfo { viewport: paint.rect, clip_rect: paint.rect, pixels_per_point: 1.0, screen_size_px: [size, size] };
                egui_wgpu::CallbackTrait::paint(paint, info, &mut pass, resources);
            }
        }
        // Rows of 256 bytes, as copies want them, hold 64 pixels: the targets are at most that wide.
        assert!(size <= 64);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * u64::from(size),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: None },
            },
            wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        assert!(pollster::block_on(scope.pop()).is_none(), "no validation errors");
        let bytes = buffer.slice(..).get_mapped_range().unwrap().to_vec();
        (0..size as usize).flat_map(|y| bytes[y * 256..y * 256 + size as usize * 4].to_vec()).collect()
    }

    fn half(v: f32) -> u16 {
        half::f16::from_f32(v).to_bits()
    }

    #[test]
    fn the_viewer_draws_pixels_as_squares_and_filters_zoomed_out_images_in_linear_light() {
        let (device, queue) = match device() {
            Ok(d) => d,
            Err(reason) => {
                let required = std::env::var_os("FLORASTACK_REQUIRE_GPU").is_some_and(|v| !v.is_empty());
                assert!(!required, "FLORASTACK_REQUIRE_GPU is set, but no GPU adapter is usable: {reason}");
                florastack_testkit::skip!(Gpu, "no GPU adapter is usable: {reason}");
                return;
            }
        };
        // 4 x 4 pixels at 1600%: linear 0.214 (sRGB's 0.5) on the left, white on the right, the top-left one transparent.
        let mut rgba = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let v = if x < 2 { 0.214 } else { 1.0 };
                let a = if x == 0 && y == 0 { 0.0 } else { 1.0 };
                rgba.extend([half(v), half(v), half(v), half(a)]);
            }
        }
        let shown = draw(&device, &queue, DisplayImage { width: 4, height: 4, rgba, alpha: true }, 64, 16.0, Vec2::splat(2.0));
        let at = |x: usize, y: usize| shown[(y * 64 + x) * 4];
        assert_eq!(at(40, 40), 255);
        assert!(at(24, 40).abs_diff(128) <= 2, "sRGB's curve: {}", at(24, 40));
        assert_eq!(at(17, 40), at(31, 40), "each image pixel is a flat square");
        // The checkerboard's light square, linear 0.1, through the curve.
        assert!(at(8, 8).abs_diff(89) <= 2, "{}", at(8, 8));
        // Black and white columns at 25%: their average in linear light (0.5, sRGB's 188), not either of them.
        let mut rgba = Vec::new();
        for _ in 0..64 {
            for x in 0..64 {
                let v = if x % 2 == 0 { 0.0 } else { 1.0 };
                rgba.extend([half(v), half(v), half(v), half(1.0)]);
            }
        }
        let shown = draw(&device, &queue, DisplayImage { width: 64, height: 64, rgba, alpha: false }, 16, 0.25, Vec2::splat(32.0));
        for (i, pixel) in shown.as_chunks::<4>().0.iter().enumerate() {
            assert!(pixel[0].abs_diff(188) <= 3, "pixel {i}: {}", pixel[0]);
        }
    }

    /// A flat opaque image of `w` x `h` pixels at linear `v`.
    fn flat(w: usize, h: usize, v: f32) -> Arc<DisplayImage> {
        let rgba = (0..w * h).flat_map(|_| [half(v), half(v), half(v), half(1.0)]).collect();
        Arc::new(DisplayImage { width: w, height: h, rgba, alpha: false })
    }

    #[test]
    fn a_picture_updated_in_part_and_brightened_shows_it() {
        let Ok((device, queue)) = device() else {
            let required = std::env::var_os("FLORASTACK_REQUIRE_GPU").is_some_and(|v| !v.is_empty());
            assert!(!required, "FLORASTACK_REQUIRE_GPU is set, but no GPU adapter is usable");
            florastack_testkit::skip!(Gpu, "no GPU adapter is usable");
            return;
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(64.0));
        let mut picture = Picture::new(flat(4, 4, 0.214));
        picture.update([2, 0], flat(2, 4, 1.0));
        assert_eq!(picture.image.rgba[(4 + 3) * 4], half(1.0), "the pixels kept are updated: row 1, column 3");
        // The texture is uploaded from the updated pixels, then updated again in part: white on the right half.
        let updates = std::mem::take(&mut picture.updates);
        let paint =
            Paint { slot: 0, id: picture.id, image: Arc::clone(&picture.image), updates, boost: 2.0, rect, zoom: 16.0, center: Vec2::splat(2.0) };
        let shown = render(&device, &queue, &[(paint, None)], 64);
        let at = |x: usize, y: usize| shown[(y * 64 + x) * 4];
        assert_eq!(at(48, 32), 255);
        // Linear 0.214, twice as bright, through sRGB's curve.
        assert!(at(16, 32).abs_diff(175) <= 2, "{}", at(16, 32));
    }

    #[test]
    fn two_images_compared_each_keep_their_own_view() {
        let Ok((device, queue)) = device() else {
            let required = std::env::var_os("FLORASTACK_REQUIRE_GPU").is_some_and(|v| !v.is_empty());
            assert!(!required, "FLORASTACK_REQUIRE_GPU is set, but no GPU adapter is usable");
            florastack_testkit::skip!(Gpu, "no GPU adapter is usable");
            return;
        };
        // Split: gray on the whole view, then white right of the line through slot 1, at another zoom and center;
        // both prepared before either is painted, as in a frame.
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(64.0));
        let first = Paint { slot: 0, id: 1, image: flat(4, 4, 0.214), updates: Vec::new(), boost: 1.0, rect, zoom: 16.0, center: Vec2::splat(2.0) };
        let second = Paint { slot: 1, id: 2, image: flat(2, 2, 1.0), updates: Vec::new(), boost: 1.0, rect, zoom: 32.0, center: Vec2::splat(1.0) };
        let shown = render(&device, &queue, &[(first, None), (second, Some([40, 0, 24, 64]))], 64);
        let at = |x: usize, y: usize| shown[(y * 64 + x) * 4];
        assert!(at(10, 32).abs_diff(128) <= 2 && at(38, 10).abs_diff(128) <= 2, "the first image, left of the line: {}", at(10, 32));
        assert_eq!((at(41, 32), at(63, 63)), (255, 255), "the second image, right of it");
        // Side by side: each image in its half of the target, with the same zoom.
        let left = Rect::from_min_size(Pos2::ZERO, Vec2::new(32.0, 64.0));
        let right = Rect::from_min_size(Pos2::new(32.0, 0.0), Vec2::new(32.0, 64.0));
        let first =
            Paint { slot: 0, id: 3, image: flat(2, 4, 0.214), updates: Vec::new(), boost: 1.0, rect: left, zoom: 16.0, center: Vec2::new(1.0, 2.0) };
        let second =
            Paint { slot: 1, id: 4, image: flat(2, 4, 1.0), updates: Vec::new(), boost: 1.0, rect: right, zoom: 16.0, center: Vec2::new(1.0, 2.0) };
        let shown = render(&device, &queue, &[(first, None), (second, None)], 64);
        let at = |x: usize, y: usize| shown[(y * 64 + x) * 4];
        assert!(at(1, 32).abs_diff(128) <= 2 && at(30, 32).abs_diff(128) <= 2, "the first image fills the left half: {}", at(30, 32));
        assert_eq!((at(33, 32), at(62, 32)), (255, 255), "the second fills the right half");
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixel positions in a 64-pixel target")]
    fn a_tiled_image_shows_as_the_whole_one_does_across_its_tiles_edges() {
        let Ok((device, queue)) = device() else {
            let required = std::env::var_os("FLORASTACK_REQUIRE_GPU").is_some_and(|v| !v.is_empty());
            assert!(!required, "FLORASTACK_REQUIRE_GPU is set, but no GPU adapter is usable");
            florastack_testkit::skip!(Gpu, "no GPU adapter is usable");
            return;
        };
        // 27 x 21 pixels, odd sides, each its own color; tiled at a limit of 16 pixels, in cores of 10 (three by three
        // tiles), with a half-size overview of 13 x 10.
        let value = |x: usize, y: usize, c: usize| ((x * 7 + y * 13 + c * 5) % 23) as f32 / 22.0;
        let make = |w: usize, h: usize| {
            let rgba = (0..w * h).flat_map(|i| [half(value(i % w, i / w, 0)), half(value(i % w, i / w, 1)), half(value(i % w, i / w, 2)), half(1.0)]);
            Arc::new(DisplayImage { width: w, height: h, rgba: rgba.collect(), alpha: false })
        };
        let (w, h) = (27, 21);
        let image = make(w, h);
        assert_eq!(tiles(27, 21, 16).len(), 9);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(64.0));
        let shown = |image: &Arc<DisplayImage>, zoom: f32, center: Vec2, limit: Option<u32>| {
            let paint = Paint { slot: 0, id: 1, image: Arc::clone(image), updates: Vec::new(), boost: 1.0, rect, zoom, center };
            render_with(&device, &queue, &[(paint, None)], 64, limit)
        };
        let encode = |v: f32| {
            let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            (e * 255.0).round()
        };
        // At 200% and 100%, every image pixel is itself, on both sides of every tile's edge.
        for (zoom, center) in [(2.0, Vec2::new(16.0, 16.0)), (2.0, Vec2::new(11.0, 5.0)), (1.0, Vec2::new(13.0, 10.0))] {
            let tiled = shown(&image, zoom, center, Some(16));
            for (py, px) in (0..64).flat_map(|y| (0..64).map(move |x| (y, x))) {
                let (ix, iy) = (center.x + (px as f32 + 0.5 - 32.0) / zoom, center.y + (py as f32 + 0.5 - 32.0) / zoom);
                if ix < 0.0 || iy < 0.0 || ix >= w as f32 || iy >= h as f32 {
                    continue;
                }
                let (x, y) = (ix as usize, iy as usize);
                for c in 0..3 {
                    let got = f32::from(tiled[(py * 64 + px) * 4 + c]);
                    let want = encode(half::f16::from_f32(value(x, y, c)).to_f32());
                    assert!((got - want).abs() <= 1.0, "zoom {zoom}, pixel ({x}, {y}) channel {c}: {got} against {want}");
                }
            }
        }
        // Filtered at 75% (the tiles, their first two levels) and at 40% (the overview), as the whole image is: of even
        // sides, whose mip levels average whole 2 x 2 blocks as the tiles' and the overview's do (an odd side's levels
        // sample between blocks, a different filter, which a pattern changing at every pixel shows).
        let even = make(28, 22);
        for (zoom, tolerance) in [(0.75, 2), (0.4, 2)] {
            let center = Vec2::new(14.0, 11.0);
            let (tiled, whole) = (shown(&even, zoom, center, Some(16)), shown(&even, zoom, center, None));
            let worst = tiled.iter().zip(&whole).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(worst <= tolerance, "zoom {zoom}: the tiled image differs by {worst}");
        }
    }

    #[test]
    fn an_overview_update_averages_whole_blocks() {
        let rgba = [0.0f32, 1.0, 0.5, 0.5, 0.25, 0.75].iter().flat_map(|&v| [half(v), half(v), half(v), half(1.0)]).collect();
        let image = DisplayImage { width: 3, height: 2, rgba, alpha: false };
        let reduced = halve(&image, [0, 0, 2, 1]);
        let red: Vec<f32> = reduced.rgba.as_chunks::<4>().0.iter().map(|p| half::f16::from_bits(p[0]).to_f32()).collect();
        // (0, 1, 0.5, 0.25) and the edge's block (0.5, 0.75) alone.
        assert_eq!(red, [0.4375, 0.625]);
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixel positions in a 64-pixel target")]
    fn a_tiled_image_updated_in_part_shows_it_in_its_tiles_and_overview() {
        let Ok((device, queue)) = device() else {
            let required = std::env::var_os("FLORASTACK_REQUIRE_GPU").is_some_and(|v| !v.is_empty());
            assert!(!required, "FLORASTACK_REQUIRE_GPU is set, but no GPU adapter is usable");
            florastack_testkit::skip!(Gpu, "no GPU adapter is usable");
            return;
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(64.0));
        let mut resources = resources(&device, Some(16));
        let mut picture = Picture::new(flat(28, 22, 0.214));
        let paint = |picture: &mut Picture, zoom: f32| Paint {
            slot: 0,
            id: picture.id,
            image: Arc::clone(&picture.image),
            updates: std::mem::take(&mut picture.updates),
            boost: 1.0,
            rect,
            zoom,
            center: Vec2::new(14.0, 11.0),
        };
        // Drawn at 200%, which uploads the tiles in view, then white over four tiles' corners, drawn again.
        let first = paint(&mut picture, 2.0);
        render_in(&device, &queue, &mut resources, &[(first, None)], 64);
        picture.update([7, 7], flat(6, 6, 1.0));
        let second = paint(&mut picture, 2.0);
        let shown = render_in(&device, &queue, &mut resources, &[(second, None)], 64);
        let at = |x: f32, y: f32| shown[((32.0 + (y - 11.0) * 2.0) as usize * 64 + (32.0 + (x - 14.0) * 2.0) as usize) * 4];
        for (x, y) in [(7.5, 7.5), (9.5, 9.5), (10.5, 10.5), (12.5, 12.5)] {
            assert_eq!(at(x, y), 255, "pixel ({x}, {y}) updated");
        }
        assert!(at(6.5, 6.5).abs_diff(128) <= 2 && at(13.5, 13.5).abs_diff(128) <= 2, "the rest as it was");
        // The overview shows it too, as a new upload of the updated image does.
        let zoomed_out = paint(&mut picture, 0.4);
        let kept = render_in(&device, &queue, &mut resources, &[(zoomed_out, None)], 64);
        let fresh = render_with(&device, &queue, &[(paint(&mut picture, 0.4), None)], 64, Some(16));
        let worst = kept.iter().zip(&fresh).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(worst <= 1, "the overview updated in place differs by {worst}");
    }
}
