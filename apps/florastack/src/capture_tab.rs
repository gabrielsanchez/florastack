//! Capture forms dispatch only engine commands. Connected state is queried without blocking on hardware.

use florastack_engine::{Value, capture::serial::Protocol, json};

use crate::app::FloraStackApp;
use crate::i18n::{tr, tr_args};

pub struct CaptureUi {
    pub input: String,
    pub output: String,
    pub preset: String,
    pub frames: u32,
    pub step: f64,
    pub settle_ms: u64,
    pub timeout_ms: u64,
    pub port: String,
    pub protocol: Protocol,
    pub calibration: f64,
    pub travel: f64,
    pub baud: u32,
    pub lens: bool,
    /// 0: external; 1: controller shutter; 2: built-in camera.
    pub trigger: u8,
    pub magnification: f64,
    pub aperture: f64,
    pub pixel_pitch: f64,
    pub recommendation: Option<Value>,
}
impl Default for CaptureUi {
    fn default() -> Self {
        Self {
            input: String::new(),
            output: String::new(),
            preset: "builtin:insect-macro".into(),
            frames: 20,
            step: 50.0,
            settle_ms: 300,
            timeout_ms: 120_000,
            port: String::new(),
            protocol: Protocol::Arduino,
            calibration: 1.0,
            travel: 10000.0,
            baud: 115_200,
            lens: false,
            trigger: 0,
            magnification: 1.0,
            aperture: 4.0,
            pixel_pitch: 4.0,
            recommendation: None,
        }
    }
}

fn text(ui: &mut egui::Ui, key: &str, value: &mut String) {
    ui.label(tr(key));
    ui.add(egui::TextEdit::singleline(value).hint_text(tr(key)));
}
fn number<T: egui::emath::Numeric>(ui: &mut egui::Ui, key: &str, value: &mut T, range: std::ops::RangeInclusive<T>) {
    ui.horizontal(|ui| {
        ui.label(tr(key));
        ui.add(egui::DragValue::new(value).range(range));
    });
}

pub fn show(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.heading(tr("tab-capture"));
    ui.label(tr("capture-workflow"));
    #[cfg(target_os = "windows")]
    ui.label(tr("capture-windows"));
    ui.hyperlink_to(tr("capture-help"), "https://github.com/gabrielsanchez/florastack/blob/main/docs/capture.md");
    text(ui, "capture-input", &mut app.capture.input);
    text(ui, "capture-output", &mut app.capture.output);
    text(ui, "capture-preset", &mut app.capture.preset);
    egui::ComboBox::from_id_salt("capture-built-in").selected_text(tr("capture-choose-preset")).show_ui(ui, |ui| {
        for (id, label) in [
            ("insect-macro", "capture-preset-insect"),
            ("product-shot", "capture-preset-product"),
            ("landscape", "capture-preset-landscape"),
            ("microscope", "capture-preset-microscope"),
        ] {
            if ui.selectable_label(app.capture.preset == format!("builtin:{id}"), tr(label)).clicked() {
                app.capture.preset = format!("builtin:{id}");
            }
        }
    });
    if ui.button(tr("capture-watch")).clicked() {
        app.follow_preview = true;
        app.shell.run("watch.run", json!({"input":app.capture.input,"output":app.capture.output,"preset":app.capture.preset}));
    }
    let info = app.shell.run("capture.info", json!({})).unwrap_or_default();
    let busy = info["busy"].as_bool().unwrap_or(false);
    for job in app.shell.jobs.values().filter(|j| matches!(j.command.as_str(), "watch.run" | "capture.run")) {
        if let Some(preview) = &job.preview {
            ui.label(tr_args("capture-progress", &[("frames", preview.frames.to_string().into())]));
        }
    }
    ui.separator();
    calculator(app, ui);
    rail_controls(app, ui, &info, busy);
    camera_controls(app, ui, &info, busy);
    ui.separator();
    number(ui, "capture-frames", &mut app.capture.frames, 2..=10001);
    number(ui, "capture-step", &mut app.capture.step, -1_000_000.0..=1_000_000.0);
    number(ui, "capture-settle", &mut app.capture.settle_ms, 0..=60000);
    number(ui, "capture-timeout", &mut app.capture.timeout_ms, 100..=3_600_000);
    if info["cameraControl"] == true {
        ui.checkbox(&mut app.capture.lens, tr("capture-lens"));
    }
    egui::ComboBox::from_id_salt("capture_trigger")
        .selected_text(tr(match app.capture.trigger {
            1 => "capture-controller",
            2 => "capture-built-in",
            _ => "capture-external",
        }))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut app.capture.trigger, 0, tr("capture-external"));
            ui.selectable_value(&mut app.capture.trigger, 1, tr("capture-controller"));
            if info["cameraControl"] == true {
                ui.selectable_value(&mut app.capture.trigger, 2, tr("capture-built-in"));
            }
        });
    if ui.add_enabled(!busy, egui::Button::new(tr("capture-start"))).clicked() {
        app.follow_preview = true;
        app.shell.run("capture.run", json!({"watch":{"input":app.capture.input,"output":app.capture.output,"preset":app.capture.preset},"frames":app.capture.frames,
            "mode":if app.capture.lens {"lens"} else {"rail"},"trigger":match app.capture.trigger {1=>"controller",2=>"camera",_=>"external"},
            "stepMicrometers":if app.capture.lens {0.0} else {app.capture.step},"settleMs":app.capture.settle_ms,"frameTimeoutMs":app.capture.timeout_ms}));
    }
    // Cancellation is an engine command and stays available even while the device worker owns the driver mutex.
    if ui.button(tr("capture-stop")).clicked() {
        let jobs: Vec<_> = app
            .shell
            .jobs
            .values()
            .filter(|job| matches!(job.command.as_str(), "capture.run" | "capture.rail" | "capture.camera" | "watch.run") && !job.state.is_finished())
            .map(|job| job.id)
            .collect();
        for id in jobs {
            app.shell.run("job.cancel", json!({"job":id}));
        }
    }
}

fn calculator(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.collapsing(tr("capture-calculator"), |ui| {
        number(ui, "capture-magnification", &mut app.capture.magnification, 0.01..=100.0);
        number(ui, "capture-aperture", &mut app.capture.aperture, 0.5..=64.0);
        number(ui, "capture-pitch", &mut app.capture.pixel_pitch, 0.1..=100.0);
        if ui.button(tr("capture-calculate")).clicked() {
            app.capture.recommendation = app.shell.run(
                "capture.calculate",
                json!({"magnification":app.capture.magnification,"aperture":app.capture.aperture,"pixelPitch":app.capture.pixel_pitch}),
            );
        }
        if let Some(r) = &app.capture.recommendation {
            ui.label(r["explanation"].as_str().unwrap_or_default());
            if ui.button(tr("capture-use-step")).clicked() {
                app.capture.step = r["stepMicrometers"].as_f64().unwrap_or(app.capture.step);
            }
        }
    });
}

fn rail_controls(app: &mut FloraStackApp, ui: &mut egui::Ui, info: &Value, busy: bool) {
    ui.collapsing(tr("capture-rail"), |ui| {
        text(ui, "capture-port", &mut app.capture.port);
        number(ui, "capture-baud", &mut app.capture.baud, 1200..=1_000_000);
        egui::ComboBox::from_id_salt("rail-protocol").selected_text(format!("{:?}", app.capture.protocol)).show_ui(ui, |ui| {
            for (protocol, key) in [(Protocol::Arduino,"capture-arduino"),(Protocol::Marlin,"capture-marlin"),(Protocol::ZaberAscii,"capture-zaber")] {
                ui.selectable_value(&mut app.capture.protocol, protocol, tr(key));
            }
        });
        number(ui, "capture-calibration", &mut app.capture.calibration, 0.001..=10000.0);
        number(ui, "capture-travel", &mut app.capture.travel, 0.001..=1_000_000.0);
        ui.label(tr("capture-zero"));
        if ui.add_enabled(!busy, egui::Button::new(tr("capture-connect-rail"))).clicked() {
            app.shell.run("capture.rail", json!({"action":"connect","settings":{"port":app.capture.port,"baud":app.capture.baud,"protocol":app.capture.protocol,"micrometersPerStep":app.capture.calibration,"travelMicrometers":app.capture.travel,"feed":10,"timeoutMs":app.capture.timeout_ms}}));
        }
        if ui.add_enabled(!busy, egui::Button::new(tr("capture-disconnect-rail"))).clicked() { app.shell.run("capture.rail", json!({"action":"disconnect"})); }
        if ui.add_enabled(!busy && info["rail"] == true, egui::Button::new(tr("capture-jog"))).clicked() { app.shell.run("capture.rail", json!({"action":"move","micrometers":app.capture.step})); }
    });
}

fn camera_controls(app: &mut FloraStackApp, ui: &mut egui::Ui, info: &Value, busy: bool) {
    if info["cameraControl"] == true {
        ui.collapsing(tr("capture-camera"), |ui| {
            ui.label(tr("capture-focus-note"));
            for (key, action, pulses) in [
                ("capture-connect-camera", "connect", 0),
                ("capture-live-view", "liveView", 0),
                ("capture-near", "focus", -1),
                ("capture-far", "focus", 1),
                ("capture-mark-near", "markNear", 0),
                ("capture-mark-far", "markFar", 0),
                ("capture-invalidate", "invalidateMarks", 0),
                ("capture-disconnect-camera", "disconnect", 0),
            ] {
                let supported = match action {
                    "connect" => info["camera"].is_null(),
                    "focus" | "markNear" | "markFar" => info["camera"]["focus"] == true && info["camera"]["liveView"] == true,
                    "liveView" => info["camera"]["liveView"] == true,
                    _ => info["camera"].is_object(),
                };
                if ui.add_enabled(!busy && supported, egui::Button::new(tr(key))).clicked() {
                    if action == "liveView" {
                        app.follow_preview = true;
                    }
                    app.shell.run("capture.camera", json!({"action":action,"pulses":pulses}));
                }
            }
            if let Some(near) = info["focus"]["near"].as_i64() {
                ui.label(format!("{}: {near}", tr("capture-mark-near")));
            }
            if let Some(far) = info["focus"]["far"].as_i64() {
                ui.label(format!("{}: {far}", tr("capture-mark-far")));
            }
        });
    }
}
