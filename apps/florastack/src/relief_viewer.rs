//! Textured, depth-tested wgpu relief renderer. Orbit and zoom are pure view state.

use egui_wgpu::wgpu;
use florastack_engine::parallax::Scene;
use std::sync::Arc;
use wgpu::util::DeviceExt as _;

static NEXT_RELIEF: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Default)]
pub struct ReliefViewer {
    scene: Option<Arc<Scene>>,
    id: u64,
    yaw: f32,
    pitch: f32,
    distance: f32,
}

impl ReliefViewer {
    pub fn set(&mut self, scene: Scene) {
        self.scene = Some(Arc::new(scene));
        self.id = NEXT_RELIEF.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.reset();
    }
    pub fn reset(&mut self) {
        self.yaw = 0.0;
        self.pitch = 0.0;
        self.distance = 1.6;
    }
    pub fn loaded(&self) -> bool {
        self.scene.is_some()
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, gpu: bool) {
        let Some(scene) = self.scene.clone() else { return };
        let (rect, response) = ui.allocate_exact_size(ui.available_size().max(egui::Vec2::splat(1.0)), egui::Sense::click_and_drag());
        if response.dragged() {
            let delta = ui.input(|i| i.pointer.delta());
            self.yaw += delta.x * 0.01;
            self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.4, 1.4);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            self.distance = (self.distance * (-scroll * 0.002).exp()).clamp(0.8, 8.0);
        }
        if response.double_clicked() {
            self.reset();
        }
        if gpu {
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                rect,
                Paint { scene: scene.clone(), id: self.id, rect, orbit: [self.yaw, self.pitch, self.distance, rect.aspect_ratio()] },
            ));
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                crate::i18n::tr("relief-gpu-needed"),
                egui::FontId::proportional(16.0),
                ui.visuals().text_color(),
            );
        }
    }
}

struct Mesh {
    id: u64,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
    group: wgpu::BindGroup,
}
struct Target {
    size: [u32; 2],
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    group: wgpu::BindGroup,
}
pub(crate) struct Renderer {
    mesh_pipeline: wgpu::RenderPipeline,
    blit_pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    sampler: wgpu::Sampler,
    mesh: Option<Mesh>,
    target: Option<Target>,
    encode: bool,
}

impl Renderer {
    pub fn install(state: &egui_wgpu::RenderState) -> bool {
        if let Some(renderer) = Self::new(&state.device, state.target_format) {
            state.renderer.write().callback_resources.insert(renderer);
            return true;
        }
        false
    }
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Option<Self> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("3D relief"),
            source: wgpu::ShaderSource::Wgsl(include_str!("relief.wgsl").into()),
        });
        let attributes = wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x2];
        let buffers = [Some(wgpu::VertexBufferLayout { array_stride: 20, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attributes })];
        let pipeline = |mesh: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("3D relief"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(if mesh { "mesh_vertex" } else { "blit_vertex" }),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: if mesh { &buffers } else { &[] },
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: mesh.then_some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(if mesh { "mesh_fragment" } else { "blit_fragment" }),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: if mesh { wgpu::TextureFormat::Rgba8UnormSrgb } else { format },
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let mesh_pipeline = pipeline(true);
        let blit_pipeline = pipeline(false);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("3D orbit"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            tracing::warn!("3D viewer unavailable: {e}");
            return None;
        }
        Some(Self { mesh_pipeline, blit_pipeline, uniform, sampler, mesh: None, target: None, encode: !format.is_srgb() })
    }
    fn group(&self, device: &wgpu::Device, pipeline: &wgpu::RenderPipeline, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("3D texture"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, id: u64, scene: &Scene) {
        if self.mesh.as_ref().is_some_and(|m| m.id == id) || scene.validate().is_err() {
            return;
        }
        let (Ok(w), Ok(h), Ok(count)) = (u32::try_from(scene.width), u32::try_from(scene.height), u32::try_from(scene.indices.len())) else { return };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("3D composite"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &scene.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            texture.size(),
        );
        let group = self.group(device, &self.mesh_pipeline, &texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let buffer = |label, contents, usage| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage });
        self.mesh = Some(Mesh {
            id,
            vertices: buffer("relief vertices", bytemuck::cast_slice(&scene.vertices), wgpu::BufferUsages::VERTEX),
            indices: buffer("relief indices", bytemuck::cast_slice(&scene.indices), wgpu::BufferUsages::INDEX),
            count,
            group,
        });
    }
    fn resize(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let texture = |format, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("3D view"),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let color = texture(wgpu::TextureFormat::Rgba8UnormSrgb, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
        let depth = texture(wgpu::TextureFormat::Depth32Float, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let group = self.group(device, &self.blit_pipeline, &color);
        self.target = Some(Target { size, color, depth, group });
    }
}

struct Paint {
    scene: Arc<Scene>,
    id: u64,
    rect: egui::Rect,
    orbit: [f32; 4],
}
impl egui_wgpu::CallbackTrait for Paint {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Viewport sizes are clamped to positive device limits.
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(renderer) = resources.get_mut::<Renderer>() else { return Vec::new() };
        renderer.upload(device, queue, self.id, &self.scene);
        let side = |v: f32| (v * screen.pixels_per_point).round().clamp(1.0, 2048.0) as u32;
        renderer.resize(device, [side(self.rect.width()), side(self.rect.height())]);
        let [a, b, c, d] = self.orbit;
        queue.write_buffer(&renderer.uniform, 0, bytemuck::cast_slice(&[a, b, c, d, if renderer.encode { 1.0_f32 } else { 0.0 }, 0.0, 0.0, 0.0]));
        if let (Some(mesh), Some(target)) = (&renderer.mesh, &renderer.target) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("3D relief"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.025, g: 0.025, b: 0.025, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&renderer.mesh_pipeline);
            pass.set_bind_group(0, &mesh.group, &[]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.count, 0, 0..1);
        }
        Vec::new()
    }
    fn paint(&self, _: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        if let Some(renderer) = resources.get::<Renderer>()
            && let Some(target) = &renderer.target
        {
            pass.set_pipeline(&renderer.blit_pipeline);
            pass.set_bind_group(0, &target.group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_wgpu::CallbackTrait as _;

    fn render(device: &wgpu::Device, queue: &wgpu::Queue, scene: Scene, yaw: f32) -> Vec<u8> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut resources = egui_wgpu::CallbackResources::default();
        resources.insert(Renderer::new(device, wgpu::TextureFormat::Rgba8Unorm).expect("3D shaders compile"));
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(64.0));
        let paint = Paint { scene: Arc::new(scene), id: 1, rect, orbit: [yaw, 0.0, 1.6, 1.0] };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [64, 64], pixels_per_point: 1.0 };
        paint.prepare(device, queue, &screen, &mut encoder, &mut resources);
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
            paint.paint(
                egui::PaintCallbackInfo { viewport: rect, clip_rect: rect, pixels_per_point: 1.0, screen_size_px: [64, 64] },
                &mut pass,
                &resources,
            );
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 64 * 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(64) },
            },
            target.size(),
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let error = pollster::block_on(scope.pop());
        assert!(error.is_none(), "{error:?}");
        buffer.slice(..).get_mapped_range().unwrap().to_vec()
    }

    #[test]
    fn textured_relief_depth_tests_and_orbits_offscreen() {
        let (device, queue) = match crate::viewer::tests::device() {
            Ok(pair) => pair,
            Err(reason) => {
                assert!(std::env::var_os("FLORASTACK_REQUIRE_GPU").is_none_or(|v| v.is_empty()), "GPU required: {reason}");
                florastack_testkit::skip!(Gpu, "no GPU adapter is usable: {reason}");
                return;
            }
        };
        let mut vertices = Vec::new();
        // Red foreground is drawn first; blue background must still fail the depth test afterward.
        for (z, u) in [(0.1, 0.0), (-0.1, 1.0)] {
            for (x, y) in [(-0.4, 0.4), (-0.4, -0.4), (0.4, 0.4), (0.4, -0.4)] {
                vertices.push([x, y, z, u, 0.0]);
            }
        }
        let scene =
            Scene { vertices, indices: vec![0, 1, 2, 2, 1, 3, 4, 5, 6, 6, 5, 7], width: 2, height: 1, rgba: vec![255, 0, 0, 255, 0, 0, 255, 255] };
        let front = render(&device, &queue, scene.clone(), 0.0);
        assert_eq!(&front[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 4], &[255, 0, 0, 255]);
        let orbited = render(&device, &queue, scene, 0.7);
        assert_ne!(front, orbited, "orbit changes projection");
    }
}
