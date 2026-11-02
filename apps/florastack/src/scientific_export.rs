//! Scientific Export-tab controls. Computation and persistent mutations go through registry commands.

use florastack_engine::{Value, json};

use crate::app::FloraStackApp;
use crate::i18n::tr;

fn set(app: &mut FloraStackApp, patch: Value) {
    if let Some(result) = app.shell.run("export.set", patch) {
        app.shell.export_settings = result["settings"].clone();
    }
}

fn toggle(app: &mut FloraStackApp, ui: &mut egui::Ui, key: &str, label: &str) {
    let mut value = app.shell.export_settings[key].as_bool().unwrap_or(false);
    if ui.checkbox(&mut value, tr(label)).changed() {
        set(app, json!({key: value}));
    }
}

pub(crate) fn show(app: &mut FloraStackApp, ui: &mut egui::Ui, output: u64) {
    ui.collapsing(tr("export-scientific"), |ui| {
        toggle(app, ui, "calibrated_depth", "export-calibrated-depth");
        let settings = app.shell.export_settings.clone();
        let mut known = settings["z_step_micrometers"].as_f64().is_some();
        if ui.checkbox(&mut known, tr("export-known-step")).changed() {
            set(app, json!({"z_step_micrometers": if known { json!(1.0) } else { Value::Null }}));
        }
        if known {
            let mut step = settings["z_step_micrometers"].as_f64().unwrap_or(1.0);
            let mut origin = settings["z_origin_micrometers"].as_f64().unwrap_or(0.0);
            ui.horizontal(|ui| {
                ui.label(tr("export-z-step"));
                if ui.add(egui::DragValue::new(&mut step).speed(0.1).range(-1e12..=1e12).suffix(" µm")).changed() {
                    set(app, json!({"z_step_micrometers":step}));
                }
            });
            ui.horizontal(|ui| {
                ui.label(tr("export-z-origin"));
                if ui.add(egui::DragValue::new(&mut origin).speed(0.1).range(-1e12..=1e12).suffix(" µm")).changed() {
                    set(app, json!({"z_origin_micrometers":origin}));
                }
            });
            ui.weak(tr("export-z-convention"));
        }
        toggle(app, ui, "contribution_maps", "export-contribution-maps");
        toggle(app, ui, "alignment_csv", "export-alignment-csv");
        ui.weak(tr("export-sidecar-always"));
        calibration(app, ui);
        annotations(app, ui, output);
    });
}

fn calibration(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.separator();
    ui.label(tr("export-calibration"));
    if app.export.profiles.is_none() {
        app.export.profiles = app.shell.run("calibration.get", json!({}));
    }
    let mut profiles = app.export.profiles.as_ref().and_then(|v| v["profiles"].as_array()).cloned().unwrap_or_default();
    let current = app.shell.export_settings["calibration_name"].as_str().unwrap_or("").to_owned();
    let shown = if current.is_empty() { tr("calibration-none") } else { current.clone() };
    egui::ComboBox::new("scientific-calibration", tr("calibration-profile")).selected_text(shown).show_ui(ui, |ui| {
        if ui.selectable_label(current.is_empty(), tr("calibration-none")).clicked() {
            set(app, json!({"pixels_per_micrometer":null,"calibration_name":""}));
        }
        for profile in &profiles {
            let name = profile["name"].as_str().unwrap_or("");
            if ui.selectable_label(current == name, name).clicked() {
                set(app, json!({"pixels_per_micrometer":profile["pixelsPerMicrometer"],"calibration_name":name}));
            }
        }
    });
    if let Some(ppm) = app.shell.export_settings["pixels_per_micrometer"].as_f64() {
        ui.label(format!("{ppm} px/µm"));
    }
    ui.weak(tr("calibration-master"));
    ui.horizontal(|ui| {
        let label = ui.label(tr("calibration-name"));
        ui.text_edit_singleline(&mut app.export.profile_name).labelled_by(label.id);
    });
    ui.horizontal(|ui| {
        ui.label(tr("calibration-ppm"));
        ui.add(egui::DragValue::new(&mut app.export.profile_ppm).speed(0.01).range(1e-9..=1e9));
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!app.export.profile_name.trim().is_empty() && app.export.profile_ppm > 0.0, egui::Button::new(tr("calibration-save")))
            .clicked()
        {
            let profile = json!({"name":app.export.profile_name,"pixelsPerMicrometer":app.export.profile_ppm});
            profiles.retain(|p| p["name"] != profile["name"]);
            profiles.push(profile.clone());
            app.export.profiles = app.shell.run("calibration.set", json!({"profiles":profiles}));
            if app.export.profiles.is_some() {
                set(app, json!({"calibration_name":profile["name"],"pixels_per_micrometer":profile["pixelsPerMicrometer"]}));
            }
        }
        if ui.add_enabled(!current.is_empty(), egui::Button::new(tr("calibration-delete"))).clicked() {
            profiles.retain(|p| p["name"] != current);
            app.export.profiles = app.shell.run("calibration.set", json!({"profiles":profiles}));
        }
    });
}

fn annotations(app: &mut FloraStackApp, ui: &mut egui::Ui, output: u64) {
    ui.separator();
    toggle(app, ui, "annotations", "export-annotations");
    let Some(records) = app.shell.run("annotation.get", json!({"output":output})) else { return };
    let mut layers = records["layers"].as_array().cloned().unwrap_or_default();
    let mut changed = false;
    let mut remove = None;
    for (i, layer) in layers.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                let mut visible = layer["visible"].as_bool().unwrap_or(true);
                if ui.checkbox(&mut visible, layer["name"].as_str().unwrap_or("")).changed() {
                    layer["visible"] = json!(visible);
                    changed = true;
                }
                if ui.small_button(tr("annotation-remove")).clicked() {
                    remove = Some(i);
                }
            });
            let mut white = layer["white"].as_bool().unwrap_or(true);
            if ui.checkbox(&mut white, tr("annotation-white")).changed() {
                layer["white"] = json!(white);
                changed = true;
            }
            let mut size = layer["textSize"].as_u64().unwrap_or(8);
            ui.horizontal(|ui| {
                ui.label(tr("annotation-text-size"));
                if ui.add(egui::DragValue::new(&mut size).range(8..=256)).changed() {
                    layer["textSize"] = json!(size);
                    changed = true;
                }
            });
            let annotation = &mut layer["annotation"];
            if annotation["kind"] == "scale-bar" {
                let mut automatic = annotation["length"].as_f64().is_none();
                if ui.checkbox(&mut automatic, tr("annotation-auto-length")).changed() {
                    annotation["length"] = if automatic { Value::Null } else { json!(10.0) };
                    changed = true;
                }
                if !automatic {
                    let mut length = annotation["length"].as_f64().unwrap_or(10.0);
                    if ui.add(egui::DragValue::new(&mut length).range(1e-9..=1e12).suffix(" µm")).changed() {
                        annotation["length"] = json!(length);
                        changed = true;
                    }
                }
                for (key, label, options) in [
                    (
                        "position",
                        "annotation-position",
                        &[
                            ("top-left", "annotation-top-left"),
                            ("top-right", "annotation-top-right"),
                            ("bottom-left", "annotation-bottom-left"),
                            ("bottom-right", "annotation-bottom-right"),
                        ][..],
                    ),
                    ("style", "annotation-style", &[("solid", "annotation-solid"), ("bracket", "annotation-bracket")][..]),
                ] {
                    let current = annotation[key].as_str().unwrap_or(options.first().map_or("", |o| o.0)).to_owned();
                    egui::ComboBox::new(key, tr(label))
                        .selected_text(options.iter().find(|o| o.0 == current).map_or_else(|| current.clone(), |o| tr(o.1)))
                        .show_ui(ui, |ui| {
                            for &(value, label) in options {
                                if ui.selectable_label(value == current, tr(label)).clicked() {
                                    annotation[key] = json!(value);
                                    changed = true;
                                }
                            }
                        });
                }
            } else {
                let mut text = annotation["text"].as_str().unwrap_or("").to_owned();
                let response = ui.text_edit_singleline(&mut text);
                if response.changed() && !text.is_empty() && annotation["text"] != text {
                    annotation["text"] = json!(text);
                    changed = true;
                }
                for (index, label) in [(0, "annotation-x"), (1, "annotation-y")] {
                    let mut position = annotation["position"][index].as_f64().unwrap_or(0.05);
                    if ui.add(egui::Slider::new(&mut position, 0.0..=1.0).text(tr(label))).changed() {
                        annotation["position"][index] = json!(position);
                        changed = true;
                    }
                }
            }
        });
    }
    if let Some(i) = remove {
        layers.remove(i);
        changed = true;
    }
    changed |= add_layer_controls(app, ui, &mut layers);
    if changed {
        app.shell.run("annotation.set", json!({"output":output,"layers":layers}));
    }
}

fn add_layer_controls(app: &mut FloraStackApp, ui: &mut egui::Ui, layers: &mut Vec<Value>) -> bool {
    let mut changed = false;
    if ui.button(tr("annotation-add-scale")).clicked() {
        layers.push(json!({"name":tr("annotation-scale-name"),"visible":true,"white":true,"textSize":8,"annotation":{"kind":"scale-bar","position":"bottom-right","style":"solid"}}));
        changed = true;
    }
    ui.horizontal(|ui| {
        let label = ui.label(tr("annotation-label"));
        ui.text_edit_singleline(&mut app.export.label).labelled_by(label.id);
    });
    if ui.add_enabled(!app.export.label.trim().is_empty(), egui::Button::new(tr("annotation-add-label"))).clicked() {
        layers.push(json!({"name":app.export.label,"visible":true,"white":true,"textSize":8,"annotation":{"kind":"text","text":app.export.label,"position":[0.05,0.05]}}));
        changed = true;
    }
    changed
}
