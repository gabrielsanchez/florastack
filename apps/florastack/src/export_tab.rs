//! The Export tab, in the right panel: the outputs, and how they are exported: the format with its bit depth and
//! compression, the color profile, the metadata copied from a frame, the extended range, the file name's template,
//! and the depth map, confidence map and aligned frames written alongside, with an estimate of the size it all takes.
//! The settings are the project's (`export.get`, changed by `export.set`; a preset's `output` group), so they are
//! saved with it and undone like any change; Export… writes the output picked into a folder (`export.run`).

use egui::{Response, RichText};
use florastack_engine::{Value, json};

use crate::app::{FloraStackApp, Selection};
use crate::i18n::{tr, tr_args};

/// The formats, as the settings name them, with their labels and the bit depths each holds (the first its default).
const FORMATS: [(&str, &str, &[&str]); 5] = [
    ("tiff", "TIFF", &["u16", "u8", "f32"]),
    ("png", "PNG", &["u16", "u8"]),
    ("jpeg", "JPEG", &["u8"]),
    ("dng", "DNG", &["u16"]),
    ("open-exr", "OpenEXR", &["f32"]),
];

/// The bit depths, as the settings name them, with their labels' keys.
const DEPTHS: [(&str, &str); 3] = [("u8", "export-depth-8"), ("u16", "export-depth-16"), ("f32", "export-depth-32")];

/// The color profiles offered, as the settings name them, with their labels' and tooltips' keys.
fn profiles() -> [(Value, &'static str, &'static str); 4] {
    [
        (json!("input"), "export-profile-input", "tip-profile-input"),
        (json!("srgb"), "export-profile-srgb", "tip-profile-srgb"),
        (json!({ "wide": "pro-photo" }), "export-profile-pro-photo", "tip-profile-wide"),
        (json!({ "wide": "rec2020" }), "export-profile-rec2020", "tip-profile-wide"),
    ]
}

/// What the Export tab keeps between frames. See the module documentation.
#[derive(Debug, Default)]
pub struct ExportUi {
    /// The size estimate, and what it was made for: the output and the lists' revision.
    estimate: Option<((u64, u64), Value)>,
    /// The template as it is being typed, until it is committed.
    template: Option<String>,
    /// Whether an export replaces files that exist.
    pub overwrite: bool,
}

/// Changes export settings (`export.set`; with `drag`, one undo step for a slider's whole drag), keeping the settings
/// the command returns.
fn set(app: &mut FloraStackApp, patch: Value, drag: Option<&str>) {
    let coalesce = drag.map(|key| format!("export.set {key}"));
    if let Some(result) = app.shell.run_with("export.set", patch, coalesce) {
        app.shell.export_settings = result["settings"].clone();
    }
}

/// A setting's row: the control `add` draws, with the tooltip `tip`.
fn row(ui: &mut egui::Ui, tip: &str, add: impl FnOnce(&mut egui::Ui) -> Response) -> Response {
    ui.horizontal(|ui| add(ui).on_hover_text(tr(tip))).inner
}

/// A choice among named values as a drop-down list: the value picked, if one was.
fn choice(ui: &mut egui::Ui, id: &str, label: &str, tip: &str, current: &str, options: &[(&str, String)]) -> Option<String> {
    let mut picked = None;
    row(ui, tip, |ui| {
        let shown = options.iter().find(|(value, _)| *value == current).map_or_else(|| current.to_owned(), |(_, text)| text.clone());
        egui::ComboBox::new(id, tr(label))
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for (value, text) in options {
                    if ui.selectable_label(*value == current, text).clicked() && *value != current {
                        picked = Some((*value).to_owned());
                    }
                }
            })
            .response
    });
    picked
}

/// A checkbox for a setting.
fn toggle(app: &mut FloraStackApp, ui: &mut egui::Ui, key: &str, label: &str, tip: &str, enabled: bool) {
    let mut on = app.shell.export_settings[key].as_bool().unwrap_or(false);
    if row(ui, tip, |ui| ui.add_enabled(enabled, egui::Checkbox::new(&mut on, tr(label)))).changed() {
        set(app, json!({ key: on }), None);
    }
}

/// A size in bytes, for people: `840 KB`, `23.4 MB`, `1.2 GB`.
pub fn size_text(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "a size, shown rounded")]
    let b = bytes as f64;
    if b < 1e6 {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    } else if b < 1e9 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.1} GB", b / 1e9)
    }
}

pub fn show(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.heading(tr("tab-export"));
    if app.shell.outputs.is_empty() {
        ui.label(tr("export-nothing"));
        return;
    }
    crate::stack_tab::outputs(app, ui);
    ui.add_space(8.0);
    ui.label(RichText::new(tr("export-settings")).strong());
    format(app, ui);
    profile(app, ui);
    metadata(app, ui);
    names(app, ui);
    let output = match app.selection {
        Selection::Output(id) => app.shell.outputs.iter().find(|o| o.id == id).cloned(),
        _ => None,
    };
    alongside(app, ui, output.as_ref().is_some_and(|o| o.method == "depth-map"));
    ui.add_space(8.0);
    let Some(output) = output else {
        ui.weak(tr("export-pick"));
        return;
    };
    estimate(app, ui, output.id);
    ui.checkbox(&mut app.export.overwrite, tr("export-overwrite")).on_hover_text(tr("tip-export-overwrite"));
    ui.horizontal(|ui| {
        if ui.button(tr("export-run")).on_hover_text(tr("tip-export-run")).clicked() {
            let mut dialog = rfd::FileDialog::new();
            if let Some(folder) = &app.prefs.last_folder {
                dialog = dialog.set_directory(folder);
            }
            if let Some(folder) = dialog.pick_folder() {
                app.export_to(output.id, &folder);
            }
        }
        if ui.button(tr("export-remove")).clicked() {
            app.shell.run("output.remove", json!({ "output": output.id }));
            app.select(Selection::Nothing);
        }
    });
}

/// The format, its bit depth and its compression.
fn format(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let settings = app.shell.export_settings.clone();
    let format = settings["format"].as_str().unwrap_or("tiff").to_owned();
    let depth = settings["depth"].as_str().unwrap_or("u16").to_owned();
    let options: Vec<(&str, String)> = FORMATS.iter().map(|(value, label, _)| (*value, (*label).to_owned())).collect();
    if let Some(picked) = choice(ui, "export.format", "export-format", "tip-export-format", &format, &options) {
        // A depth the new format holds: the one set when it does, its default otherwise.
        let depths = FORMATS.iter().find(|(f, _, _)| *f == picked).map_or(&["u16"][..], |(_, _, d)| d);
        let depth = if depths.contains(&depth.as_str()) { depth.clone() } else { depths.first().copied().unwrap_or("u16").to_owned() };
        set(app, json!({ "format": picked, "depth": depth }), None);
    }
    let held = FORMATS.iter().find(|(f, _, _)| *f == format).map_or(&["u16"][..], |(_, _, d)| d);
    let depths: Vec<(&str, String)> = DEPTHS.iter().filter(|(d, _)| held.contains(d)).map(|(d, key)| (*d, tr(key))).collect();
    if let Some(picked) = choice(ui, "export.depth", "export-depth", "tip-export-depth", &depth, &depths) {
        set(app, json!({ "depth": picked }), None);
    }
    match format.as_str() {
        "tiff" => {
            let options = [("deflate", tr("compression-deflate")), ("lzw", tr("compression-lzw")), ("none", tr("compression-none"))];
            let current = settings["tiff_compression"].as_str().unwrap_or("deflate");
            if let Some(picked) = choice(ui, "export.tiff", "export-compression", "tip-tiff-compression", current, &options) {
                set(app, json!({ "tiff_compression": picked }), None);
            }
        }
        "open-exr" => {
            let options = [
                ("zip", tr("compression-zip")),
                ("zips", tr("compression-zips")),
                ("piz", tr("compression-piz")),
                ("rle", tr("compression-rle")),
                ("none", tr("compression-none")),
            ];
            let current = settings["exr_compression"].as_str().unwrap_or("zip");
            if let Some(picked) = choice(ui, "export.exr", "export-compression", "tip-exr-compression", current, &options) {
                set(app, json!({ "exr_compression": picked }), None);
            }
            toggle(app, ui, "exr_half", "export-exr-half", "tip-exr-half", true);
        }
        "jpeg" => {
            let mut quality = settings["jpeg_quality"].as_u64().unwrap_or(95);
            let response = row(ui, "tip-jpeg-quality", |ui| ui.add(egui::Slider::new(&mut quality, 1..=100).text(tr("export-jpeg-quality"))));
            if response.changed() {
                set(app, json!({ "jpeg_quality": quality }), Some("jpeg_quality"));
            }
            let options = [("s444", tr("subsampling-444")), ("s422", tr("subsampling-422")), ("s420", tr("subsampling-420"))];
            let current = settings["jpeg_subsampling"].as_str().unwrap_or("s444");
            if let Some(picked) = choice(ui, "export.subsampling", "export-subsampling", "tip-jpeg-subsampling", current, &options) {
                set(app, json!({ "jpeg_subsampling": picked }), None);
            }
        }
        "dng" => {
            let options = [("lossless-jpeg", tr("compression-lossless-jpeg")), ("uncompressed", tr("compression-none"))];
            let current = settings["dng_compression"].as_str().unwrap_or("lossless-jpeg");
            if let Some(picked) = choice(ui, "export.dng", "export-compression", "tip-dng-compression", current, &options) {
                set(app, json!({ "dng_compression": picked }), None);
            }
            let options = [("rendered", tr("dng-look-rendered")), ("raw", tr("dng-look-raw"))];
            let current = settings["dng_look"].as_str().unwrap_or("rendered");
            if let Some(picked) = choice(ui, "export.look", "export-dng-look", "tip-dng-look", current, &options) {
                set(app, json!({ "dng_look": picked }), None);
            }
        }
        _ => {}
    }
    toggle(app, ui, "extended_range", "export-extended-range", "tip-extended-range", depth == "f32");
}

/// The color profile.
fn profile(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let current = app.shell.export_settings["profile"].clone();
    let mut picked = None;
    ui.label(tr("export-profile"));
    for (value, label, tip) in profiles() {
        if ui.radio(current == value, tr(label)).on_hover_text(tr(tip)).clicked() && current != value {
            picked = Some(value);
        }
    }
    if let Some(value) = picked {
        set(app, json!({ "profile": value }), None);
    }
}

/// The metadata: copied or not, from which frame.
fn metadata(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let settings = app.shell.export_settings.clone();
    let mut on = settings["metadata"].as_bool().unwrap_or(true);
    let mut frame = settings["metadata_frame"].as_u64().unwrap_or(1);
    let frames = u64::try_from(app.shell.frames.len()).unwrap_or(u64::MAX).max(1);
    ui.horizontal(|ui| {
        if ui.checkbox(&mut on, tr("export-metadata")).on_hover_text(tr("tip-export-metadata")).changed() {
            set(app, json!({ "metadata": on }), None);
        }
        let response = ui.add_enabled(on, egui::DragValue::new(&mut frame).range(1..=frames)).on_hover_text(tr("tip-metadata-frame"));
        if response.changed() {
            set(app, json!({ "metadata_frame": frame }), Some("metadata_frame"));
        }
    });
}

/// The file name's template, committed when typing ends, with the tokens it may hold.
fn names(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let saved = app.shell.export_settings["template"].as_str().unwrap_or("").to_owned();
    let mut text = app.export.template.clone().unwrap_or_else(|| saved.clone());
    ui.label(tr("export-template"));
    let response = ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY)).on_hover_text(tr("tip-export-template"));
    if response.changed() {
        app.export.template = Some(text.clone());
    }
    if response.lost_focus() {
        app.export.template = None;
        if text != saved {
            set(app, json!({ "template": text }), None);
        }
    }
    ui.weak(tr_args("export-tokens", &[("tokens", florastack_engine::commands::export::TOKENS.map(|t| format!("{{{t}}}")).join(" ").into())]));
}

/// What is written alongside: the maps, which only a depth-map output has, and the aligned frames.
fn alongside(app: &mut FloraStackApp, ui: &mut egui::Ui, depth_map: bool) {
    ui.label(tr("export-alongside"));
    toggle(app, ui, "depth_map", "export-depth-map", "tip-export-depth-map", true);
    toggle(app, ui, "confidence_map", "export-confidence-map", "tip-export-confidence-map", true);
    if !depth_map && (app.shell.export_settings["depth_map"] == json!(true) || app.shell.export_settings["confidence_map"] == json!(true)) {
        ui.weak(tr("export-maps-none"));
    }
    toggle(app, ui, "aligned_frames", "export-aligned-frames", "tip-export-aligned-frames", true);
}

/// The size the export takes and the main file's name, asked of the engine once for each output and change.
fn estimate(app: &mut FloraStackApp, ui: &mut egui::Ui, id: u64) {
    let key = (id, app.shell.revision);
    if app.export.estimate.as_ref().is_none_or(|(k, _)| *k != key) {
        let estimate = app.shell.run("export.estimate", json!({ "output": id })).unwrap_or(Value::Null);
        app.export.estimate = Some((key, estimate));
    }
    let Some((_, estimate)) = &app.export.estimate else { return };
    let (Some(bytes), Some(files)) = (estimate["bytes"].as_u64(), estimate["files"].as_u64()) else { return };
    let size = size_text(bytes);
    let key = if estimate["approximate"] == json!(true) { "export-estimate-about" } else { "export-estimate" };
    ui.label(tr_args(key, &[("size", size.into()), ("files", files.into())])).on_hover_text(tr("tip-export-estimate"));
    ui.weak(estimate["name"].as_str().unwrap_or(""));
}

impl FloraStackApp {
    /// Exports output `id` into `folder` with the project's export settings.
    pub fn export_to(&mut self, id: u64, folder: &std::path::Path) {
        self.remember_folder(folder);
        self.shell.run("export.run", json!({ "output": id, "folder": folder, "overwrite": self.export.overwrite }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size_text(12), "1 KB");
        assert_eq!(size_text(840_000), "840 KB");
        assert_eq!(size_text(23_400_000), "23.4 MB");
        assert_eq!(size_text(1_250_000_000), "1.2 GB");
    }

    #[test]
    fn every_format_holds_its_default_depth() {
        for (format, _, depths) in FORMATS {
            assert!(!depths.is_empty(), "{format}");
            assert!(depths.iter().all(|d| DEPTHS.iter().any(|(known, _)| known == d)), "{format}");
        }
    }
}
