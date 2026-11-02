//! Host-only view operations. Engine commands use the registry; UI requests change only view state.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use florastack_engine::{
    ErrorInfo, Value,
    automation::decode,
    control::{self, Pending},
    json,
};
use serde::Deserialize;

use crate::{FloraStackApp, app::Selection, prefs::Tab};

impl FloraStackApp {
    pub fn enable_control(&mut self, ctx: &egui::Context, port: u16, token: String) -> Result<std::net::SocketAddr, control::ControlError> {
        let ctx = ctx.clone();
        let server = control::Server::start(port, token, Arc::new(move || ctx.request_repaint()))?;
        let address = server.address;
        self.control = Some(server);
        Ok(address)
    }

    pub(crate) fn poll_control(&mut self, ctx: &egui::Context) {
        // Keep the listener while borrowing the app mutably to change pure view state.
        let Some(server) = self.control.take() else { return };
        for _ in 0..16 {
            let Some(pending) = server.try_recv() else { break };
            if pending.is_closed() {
                continue;
            }
            if pending.method == "ui.screenshot" {
                screenshot(ctx, pending);
                continue;
            }
            let result = self.control_request(&pending.method, pending.params.clone());
            pending.respond(result);
            // A subsequent request may arrive in this same frame. Fold command/job events before it selects an
            // output or asks whether the viewer is ready, just as a frame after a local UI action would do.
            self.update_state(ctx);
        }
        self.control = Some(server);
    }

    fn control_request(&mut self, method: &str, params: Value) -> Result<Value, ErrorInfo> {
        match method {
            "ui.state" => Ok(json!({"tab": self.prefs.tab, "zoom": self.viewer.zoom(), "ready": self.loading.is_none() && !self.shell.busy(),
                "frames": self.shell.frames.len(), "outputs": self.shell.outputs.len()})),
            "ui.setView" => {
                let p: View = decode(method, params).map_err(|e| e.info())?;
                if p.frame.is_some() && p.output.is_some() {
                    return Err(control::error("params", "choose a frame or an output"));
                }
                if p.zoom.is_some_and(|z| !z.is_finite() || !(1.0 / 64.0..=64.0).contains(&z)) {
                    return Err(control::error("params", "zoom must be between 1/64 and 64"));
                }
                if let Some(id) = p.frame {
                    if !self.shell.frames.iter().any(|f| f.id == id) {
                        return Err(control::error("notFound", "frame is not in the project"));
                    }
                    self.select(Selection::Frame(id));
                }
                if let Some(id) = p.output {
                    if !self.shell.outputs.iter().any(|o| o.id == id) {
                        return Err(control::error("notFound", "output is not in the project"));
                    }
                    self.select(Selection::Output(id));
                }
                if let Some(tab) = p.tab {
                    self.prefs.tab = tab;
                }
                if let Some(zoom) = p.zoom {
                    self.viewer.zoom_to(zoom);
                }
                Ok(json!({"tab": self.prefs.tab}))
            }
            "ui.close" => {
                let p: Close = decode(method, params).map_err(|e| e.info())?;
                if self.shell.engine().is_dirty() && !p.discard_changes {
                    return Err(control::error("unavailable", "save the project or pass discardChanges before closing"));
                }
                self.close_from_control();
                Ok(json!({"closing": true}))
            }
            _ => self.shell.automation(method, params).map_err(|e| e.info()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct View {
    tab: Option<Tab>,
    frame: Option<u64>,
    output: Option<u64>,
    zoom: Option<f32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Close {
    #[serde(default)]
    discard_changes: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Screenshot {
    path: PathBuf,
    #[serde(default)]
    overwrite: bool,
}

fn screenshot(ctx: &egui::Context, pending: Pending) {
    let params = decode::<Screenshot>("ui.screenshot", pending.params.clone());
    match params {
        Err(error) => pending.respond(Err(error.info())),
        Ok(p) => {
            if p.path.as_os_str().is_empty() {
                pending.respond(Err(control::error("params", "screenshot path is empty")));
                return;
            }
            ctx.request_screenshot(move |image| {
                let result =
                    write_png(&p.path, p.overwrite, &image).map(|()| json!({"path": p.path, "width": image.width(), "height": image.height()}));
                pending.respond(result.map_err(|e| control::error("io", &e.to_string())));
            });
        }
    }
}

/// Stage the PNG beside the destination and publish only the complete image; a full disk leaves no partial file.
pub fn write_png(path: &Path, overwrite: bool, image: &egui::ColorImage) -> Result<(), std::io::Error> {
    let invalid = || std::io::Error::other("invalid screenshot dimensions");
    let (width, height) = (u32::try_from(image.width()).map_err(|_| invalid())?, u32::try_from(image.height()).map_err(|_| invalid())?);
    if width == 0 || height == 0 {
        return Err(invalid());
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(egui::Color32::to_srgba_unmultiplied).collect();
    {
        let mut encoder = png::Encoder::new(staged.as_file_mut(), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        encoder.write_header()?.write_image_data(&bytes)?;
    }
    staged.flush()?;
    if overwrite { staged.persist(path) } else { staged.persist_noclobber(path) }.map_err(|e| e.error)?;
    Ok(())
}
