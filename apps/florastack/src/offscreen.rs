//! The same application and GPU viewer rendered into a texture, with no window or display server. Screenshots use
//! the same callback as eframe; this integration completes it after reading the offscreen texture.

use std::sync::Arc;
use std::time::Duration;

use egui_wgpu::wgpu;

use crate::{FloraStackApp, Options, prefs::Prefs, viewer::GpuViewer};

pub struct Renderer {
    pub state: egui_wgpu::RenderState,
}

impl Renderer {
    pub fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let state = pollster::block_on(egui_wgpu::RenderState::create(
            &egui_wgpu::WgpuConfiguration::default(),
            &instance,
            None,
            egui_wgpu::RendererOptions::default(),
        ))
        .map_err(|e| e.to_string())?;
        Ok(Self { state })
    }

    /// Process one application's frame. Texture deltas must be applied even when no screenshot is requested.
    pub fn frame(&mut self, app: &mut FloraStackApp, ctx: &egui::Context, size: [u32; 2]) -> Result<(), String> {
        if size.iter().any(|s| !(64..=4096).contains(s)) {
            return Err("offscreen dimensions must be 64 to 4096 pixels".into());
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32))),
            ..Default::default()
        };
        let logic = ctx.run_logic(&input, |ctx| app.tick(ctx));
        let output = ctx.run_ui(input, |ui| app.draw_ui(ui));
        let mut callbacks = Vec::new();
        for commands in logic.viewport_commands.values() {
            for command in commands {
                if let egui::ViewportCommand::Screenshot(callback) = command {
                    callbacks.push(callback.clone());
                }
            }
        }
        for viewport in output.viewport_output.values() {
            for command in &viewport.commands {
                if let egui::ViewportCommand::Screenshot(callback) = command {
                    callbacks.push(callback.clone());
                }
            }
        }
        self.paint(ctx, output, callbacks, size)
    }

    fn paint(
        &self,
        ctx: &egui::Context,
        mut output: egui::FullOutput,
        callbacks: Vec<egui::ScreenshotCallback>,
        size: [u32; 2],
    ) -> Result<(), String> {
        let device = &self.state.device;
        let queue = &self.state.queue;
        let mut renderer = self.state.renderer.write();
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(device, queue, *id, delta);
            }
        }
        let free = std::mem::take(&mut output.textures_delta.free);
        // egui tracks whether integrations acknowledge deltas, including on a fallible readback path.
        output.textures_delta.clear();
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let jobs = ctx.tessellate(output.shapes, output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: size, pixels_per_point: output.pixels_per_point };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        // Prepare callbacks every frame, even without a capture: viewer/retouch texture updates are consumed by
        // drawing the UI, so skipping preparation would leave the next screenshot with stale GPU pixels.
        let prepared = renderer.update_buffers(device, queue, &mut encoder, &jobs, &screen);
        if callbacks.is_empty() {
            queue.submit(prepared.into_iter().chain([encoder.finish()]));
            if let Some(error) = pollster::block_on(scope.pop()) {
                return Err(error.to_string());
            }
        } else {
            let extent = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("offscreen UI"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.state.target_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("offscreen UI"),
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
                renderer.render(&mut pass, &jobs, &screen);
            }
            let stride = (size[0] * 4).div_ceil(256) * 256;
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("screenshot readback"),
                size: u64::from(stride) * u64::from(size[1]),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(stride), rows_per_image: None },
                },
                extent,
            );
            queue.submit(prepared.into_iter().chain([encoder.finish()]));
            let image = Arc::new(readback(device, &buffer, size, stride)?);
            if let Some(error) = pollster::block_on(scope.pop()) {
                return Err(error.to_string());
            }
            for callback in callbacks {
                callback.complete(image.clone());
            }
        }
        for id in free {
            renderer.free_texture(&id);
        }
        Ok(())
    }
}

fn readback(device: &wgpu::Device, buffer: &wgpu::Buffer, size: [u32; 2], stride: u32) -> Result<egui::ColorImage, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let mapped = buffer.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
    let pixels: Vec<u8> = mapped.chunks_exact(stride as usize).flat_map(|row| row.get(..size[0] as usize * 4).unwrap_or_default()).copied().collect();
    Ok(egui::ColorImage::from_rgba_unmultiplied([size[0] as usize, size[1] as usize], &pixels))
}

pub fn run(options: Options, port: u16, token: String, size: [u32; 2]) -> Result<(), String> {
    let mut renderer = Renderer::new()?;
    let ctx = egui::Context::default();
    let gpu = GpuViewer::install(&renderer.state).ok_or("could not install the GPU viewer")?;
    let mut app = FloraStackApp::with_context(&ctx, options, Prefs::default(), Some(gpu));
    app.parallax.gpu = crate::relief_viewer::Renderer::install(&renderer.state);
    let address = app.enable_control(&ctx, port, token).map_err(|e| e.to_string())?;
    eprintln!("control: {address}");
    while !app.is_quitting() {
        renderer.frame(&mut app, &ctx, size)?;
        std::thread::sleep(Duration::from_millis(16));
    }
    // Allow the I/O task to deliver ui.close's response before shutting down its runtime.
    std::thread::sleep(Duration::from_millis(25));
    Ok(())
}
