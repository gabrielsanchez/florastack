//! The Export tab's 3D and stereo controls dispatch engine jobs; only the camera lives here.
use crate::{app::FloraStackApp, i18n::tr, relief_viewer::ReliefViewer};
use florastack_engine::{json, parallax::Settings};

pub struct ParallaxUi {
    pub settings: Settings,
    pub viewer: ReliefViewer,
    pub active: bool,
    pub gpu: bool,
    pub output: Option<u64>,
    pub frames: u16,
    pub fps: u16,
}

impl Default for ParallaxUi {
    fn default() -> Self {
        Self { settings: Settings::default(), viewer: ReliefViewer::default(), active: false, gpu: false, output: None, frames: 32, fps: 16 }
    }
}

pub fn show(app: &mut FloraStackApp, ui: &mut egui::Ui, output: u64) {
    if let Some(result) = app.shell.relief.take()
        && result["output"].as_u64() == Some(output)
        && let Some(path) = result["path"].as_str()
    {
        match florastack_engine::parallax::load_scene(std::path::Path::new(path)) {
            Ok(scene) => {
                app.parallax.viewer.set(scene);
                app.parallax.active = true;
                app.parallax.output = Some(output);
            }
            Err(e) => app.shell.notify(e.info().message, true),
        }
    }
    if app.parallax.output != Some(output) {
        app.parallax.active = false;
    }
    egui::CollapsingHeader::new(tr("parallax-title")).id_salt("parallax").show(ui, |ui| {
        ui.label(tr("parallax-relative"));
        ui.add(egui::Slider::new(&mut app.parallax.settings.confidence, 0.0..=1.0).text(tr("parallax-confidence")));
        ui.add(egui::Slider::new(&mut app.parallax.settings.smoothing, 0..=16).text(tr("parallax-smoothing")));
        ui.checkbox(&mut app.parallax.settings.reverse_depth, tr("parallax-reverse"));
        ui.add(egui::Slider::new(&mut app.parallax.settings.relief, 0.001..=1.0).text(tr("parallax-relief")));
        ui.add(egui::Slider::new(&mut app.parallax.settings.baseline, 0.0..=0.25).text(tr("parallax-baseline")));
        ui.add(egui::Slider::new(&mut app.parallax.settings.max_depth_jump, 0.0..=1.0).text(tr("parallax-jump")));
        ui.add(egui::Slider::new(&mut app.parallax.settings.max_side, 16..=2048).text(tr("parallax-resolution")));
        ui.add(egui::Slider::new(&mut app.parallax.settings.mesh_side, 2..=512).text(tr("parallax-grid")));
        ui.add(egui::Slider::new(&mut app.parallax.frames, 4..=120).text(tr("parallax-frames")));
        ui.add(egui::Slider::new(&mut app.parallax.fps, 1..=50).text(tr("parallax-fps")));
        if ui.button(tr("parallax-view")).clicked() {
            app.shell.run("parallax.preview", json!({"output":output,"settings":app.parallax.settings}));
        }
        if app.parallax.viewer.loaded() {
            ui.checkbox(&mut app.parallax.active, tr("parallax-show"));
            ui.weak(tr("parallax-orbit"));
        }
        for (key, command, extra) in [
            ("parallax-glb", "parallax.mesh", json!({"format":"glb"})),
            ("parallax-obj", "parallax.mesh", json!({"format":"obj"})),
            ("parallax-parallel", "parallax.stereo", json!({"mode":"parallel"})),
            ("parallax-cross", "parallax.stereo", json!({"mode":"cross-eyed"})),
            ("parallax-anaglyph", "parallax.stereo", json!({"mode":"anaglyph"})),
            ("parallax-animation", "parallax.animate", json!({"frames":app.parallax.frames,"fps":app.parallax.fps})),
        ] {
            if ui.button(tr(key)).clicked()
                && let Some(folder) = rfd::FileDialog::new().pick_folder()
            {
                let mut params = json!({"output":output,"folder":folder,"settings":app.parallax.settings,"overwrite":app.export.overwrite});
                if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
                    params.extend(extra.clone());
                }
                app.shell.run(command, params);
            }
        }
    });
}
