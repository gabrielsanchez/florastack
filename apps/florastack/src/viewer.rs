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
//! Images larger than the device's largest texture (8192 pixels a side on most) are loaded reduced to it. Without
//! a GPU renderer (headless tests), the viewer shows a reduced 8-bit copy through egui's own textures.

use std::sync::Arc;

use egui::{Pos2, Rect, Sense, Vec2};
use egui_wgpu::wgpu;
use florastack_engine::view::DisplayImage;

use crate::prefs::Compare;

const VIEWER_WGSL: &str = include_str!("viewer.wgsl");
const LEVELS_WGSL: &str = include_str!("levels.wgsl");
/// The side of the reduced copy shown without a GPU renderer.
const FALLBACK_SIDE: usize = 2048;
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

    /// The longest side to load images at: the device's largest texture, or the fallback's size.
    pub fn max_side(&self) -> usize {
        self.gpu.map_or(FALLBACK_SIDE, |s| s as usize)
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
    /// The images uploaded, by their ids, the most recently used last.
    textures: Vec<(u64, wgpu::Texture, wgpu::TextureView)>,
    /// One for each image drawn at once (two when comparing): its view's uniform, and its bind group with the image
    /// bound now.
    slots: Vec<Slot>,
}

/// An image drawn: its view's uniform, and the bind group of the uniform with the image it draws, by the image's id.
struct Slot {
    uniform: wgpu::Buffer,
    bound: Option<(u64, wgpu::BindGroup)>,
}

/// The uniform's size: the 12 floats of `View` in viewer.wgsl.
const UNIFORM_BYTES: u64 = 48;
/// How many images can be drawn at once, and how many stay uploaded.
const SLOTS: usize = 2;
const KEPT_TEXTURES: usize = 4;

impl GpuViewer {
    /// Makes the viewer's resources for `state`'s device and surface and keeps them in its renderer; returns the
    /// largest image side it takes, or `None` when the viewer's shaders don't compile there (the viewer then draws
    /// through egui). The device then logs its errors instead of panicking, as wgpu does by default: a crash loses
    /// work.
    pub fn install(state: &egui_wgpu::RenderState) -> Option<u32> {
        state.device.on_uncaptured_error(Arc::new(|e: wgpu::Error| tracing::error!("the display's GPU reported an error: {e}")));
        let viewer = GpuViewer::new(&state.device, state.target_format)?;
        state.renderer.write().callback_resources.insert(viewer);
        Some(state.device.limits().max_texture_dimension_2d)
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
            textures: Vec::new(),
            slots: (0..SLOTS)
                .map(|_| Slot {
                    uniform: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("viewer view"),
                        size: UNIFORM_BYTES,
                        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    bound: None,
                })
                .collect(),
        };
        if let Some(e) = pollster::block_on(scope.pop()) {
            tracing::warn!("the viewer's shaders don't compile on this GPU ({e}): images are drawn without them");
            return None;
        }
        Some(resources)
    }

    /// Binds image `id` to `slot`, uploading `image` first when it isn't uploaded yet; nothing for a slot out of range.
    fn bind(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, slot: usize, id: u64, image: &DisplayImage) {
        if self.slots.get(slot).is_none_or(|s| s.bound.as_ref().is_some_and(|(bound, _)| *bound == id)) {
            return;
        }
        let (texture, view) = match self.textures.iter().position(|(t, _, _)| *t == id) {
            Some(i) => {
                let (_, texture, view) = self.textures.remove(i);
                (texture, view)
            }
            None => self.upload(device, queue, encoder, image),
        };
        let Some(s) = self.slots.get(slot) else { return };
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewer image"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: s.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.trilinear) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.pixels) },
            ],
        });
        // The most recently used last; the oldest beyond those kept are let go (a bind group holds its own).
        self.textures.push((id, texture, view));
        if self.textures.len() > KEPT_TEXTURES {
            self.textures.remove(0);
        }
        if let Some(s) = self.slots.get_mut(slot) {
            s.bound = Some((id, group));
        }
    }

    /// Writes `updates` (regions at their top left corners) into image `id`'s texture, if it is uploaded, and makes
    /// its levels again.
    fn update(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        id: u64,
        updates: &[([usize; 2], Arc<DisplayImage>)],
    ) {
        let Some((_, texture, _)) = self.textures.iter().find(|(t, _, _)| *t == id) else { return };
        let size = texture.size();
        let mut written = false;
        for (at, region) in updates {
            let (Ok(x), Ok(y), Ok(w), Ok(h)) =
                (u32::try_from(at[0]), u32::try_from(at[1]), u32::try_from(region.width), u32::try_from(region.height))
            else {
                continue;
            };
            if w == 0 || h == 0 || x + w > size.width || y + h > size.height {
                continue;
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
                bytemuck::cast_slice(&region.rgba),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            written = true;
        }
        if written {
            self.levels(device, encoder, texture);
        }
    }

    /// Uploads `image` as a mipmapped texture, its levels made by `encoder`.
    fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        image: &DisplayImage,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let (w, h) = (u32::try_from(image.width).unwrap_or(1).max(1), u32::try_from(image.height).unwrap_or(1).max(1));
        let levels = 32 - w.max(h).leading_zeros();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewer image"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(&image.rgba),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.levels(device, encoder, &texture);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
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
        viewer.update(device, queue, encoder, self.id, &self.updates);
        viewer.bind(device, queue, encoder, self.slot, self.id, &self.image);
        let Some(slot) = viewer.slots.get(self.slot) else { return Vec::new() };
        // Clip space from image pixels: the view's center shows `center`, `zoom` device pixels to an image pixel.
        let ppp = screen.pixels_per_point;
        let (w, h) = (self.rect.width() * ppp, self.rect.height() * ppp);
        let (sx, sy) = (2.0 * self.zoom / w.max(1.0), -2.0 * self.zoom / h.max(1.0));
        let (ox, oy) = (-self.center.x * sx, -self.center.y * sy);
        let origin = self.rect.min * ppp;
        let nearest = if self.zoom >= 1.0 { 1.0 } else { 0.0 };
        let encode = if viewer.encode { 1.0 } else { 0.0 };
        let view: [f32; 12] =
            [sx, sy, ox, oy, self.image.width as f32, self.image.height as f32, origin.x, origin.y, (8.0 * ppp).round(), encode, nearest, self.boost];
        queue.write_buffer(&slot.uniform, 0, bytemuck::cast_slice(&view));
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        let Some(viewer) = resources.get::<GpuViewer>() else { return };
        let Some((_, group)) = viewer.slots.get(self.slot).and_then(|s| s.bound.as_ref()) else { return };
        pass.set_pipeline(&viewer.pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..6, 0..1);
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
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut resources = egui_wgpu::CallbackResources::default();
        resources.insert(GpuViewer::new(device, format).expect("the viewer's shaders compile"));
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
            let _ = egui_wgpu::CallbackTrait::prepare(paint, device, queue, &screen, &mut encoder, &mut resources);
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
                egui_wgpu::CallbackTrait::paint(paint, info, &mut pass, &resources);
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
}
