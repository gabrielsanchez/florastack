//! The Stack tab, in the right panel: the workflow (add frames, align with each frame's quality, stack), the settings
//! always on screen (the method, radius and smoothing, the alignment's terms, the framing) with an Advanced section for
//! the rest, the depth map's contrast-threshold step, and the outputs with their parameters, two of which can be
//! compared. Every setting has a tooltip that says what it does and when to change it, and a "?" that opens its section
//! of `docs/stack-tab.md`. Settings change through `stack.set` and `align.set` (a slider's drag is one undo step), and
//! each change of a stacking setting asks for a quarter-scale preview once the frames are aligned, beside any full
//! stack still running.

use egui::{Response, RichText};
use florastack_engine::{Value, json};

use crate::actions::Action;
use crate::app::{FloraStackApp, Selection};
use crate::i18n::{tr, tr_args};
use crate::prefs::{Compare, Method};

/// The docs page that every "?" opens, at the setting's section.
pub fn docs_url(anchor: &str) -> String {
    format!("{}/blob/main/docs/stack-tab.md#{anchor}", env!("CARGO_PKG_REPOSITORY"))
}

/// A "?" after a setting: its section of the docs, opened in the browser.
fn learn_more(ui: &mut egui::Ui, anchor: &str) {
    let response = ui.add(egui::Button::new(RichText::new("?").small()).small()).on_hover_text(tr("learn-more"));
    if response.clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(docs_url(anchor)));
    }
}

/// A setting's row: the control `add` draws, with the tooltip `tip`, then its "?". The control's response.
fn row(ui: &mut egui::Ui, tip: &str, anchor: &str, add: impl FnOnce(&mut egui::Ui) -> Response) -> Response {
    ui.horizontal(|ui| {
        let response = add(ui).on_hover_text(tr(tip));
        learn_more(ui, anchor);
        response
    })
    .inner
}

/// A setting's texts: its label's key, its tooltip's key and its section of the docs.
type Texts<'a> = (&'a str, &'a str, &'a str);

/// Changes one stacking setting (`stack.set`; with `drag`, one undo step for a slider's whole drag), keeping the
/// settings the command returns.
fn set_stack(app: &mut FloraStackApp, group: &str, key: &str, value: &Value, drag: bool) {
    let coalesce = drag.then(|| format!("stack.set {group}.{key}"));
    if let Some(result) = app.shell.run_with("stack.set", json!({ group: { key: value } }), coalesce) {
        app.shell.stack_settings = result["settings"].clone();
    }
    app.preview_method = match group {
        "depth_map" => "depth-map",
        "average" => "average",
        _ => "pyramid",
    };
}

/// Changes alignment settings (`align.set`; with `drag`, one undo step for a slider's whole drag), keeping the
/// settings the command returns.
fn set_align(app: &mut FloraStackApp, patch: Value, drag: Option<&str>) {
    let coalesce = drag.map(|key| format!("align.set {key}"));
    if let Some(result) = app.shell.run_with("align.set", patch, coalesce) {
        app.shell.align_settings = result["settings"].clone();
    }
}

/// Whether a slider's change is done: its drag ended, or a click or a key changed it.
fn settled(response: &Response) -> bool {
    response.drag_stopped() || (response.changed() && !response.dragged())
}

/// A slider for a numeric stacking setting; a preview follows when the change is done.
fn stack_slider(app: &mut FloraStackApp, ui: &mut egui::Ui, (group, key): (&str, &str), range: std::ops::RangeInclusive<f64>, texts: Texts<'_>) {
    let (label, tip, anchor) = texts;
    let logarithmic = *range.end() / range.start().max(1e-9) > 1000.0;
    let mut value = app.shell.stack_settings[group][key].as_f64().unwrap_or(*range.start());
    let response = row(ui, tip, anchor, |ui| ui.add(egui::Slider::new(&mut value, range).logarithmic(logarithmic).text(tr(label))));
    if response.changed() {
        set_stack(app, group, key, &json!(value), true);
    }
    if settled(&response) {
        app.preview_if_aligned();
    }
}

/// A choice among a stacking setting's named values, as radio buttons.
fn stack_choice(app: &mut FloraStackApp, ui: &mut egui::Ui, (group, key): (&str, &str), options: &[(&str, &str)], texts: Texts<'_>) {
    let (label, tip, anchor) = texts;
    let current = app.shell.stack_settings[group][key].as_str().unwrap_or("").to_owned();
    let mut picked = None;
    row(ui, tip, anchor, |ui| {
        ui.label(tr(label));
        let mut last = ui.label("");
        for (value, option) in options {
            last = ui.radio(current == *value, tr(option));
            if last.clicked() && current != *value {
                picked = Some(*value);
            }
        }
        last
    });
    if let Some(value) = picked {
        set_stack(app, group, key, &json!(value), false);
        app.preview_if_aligned();
    }
}

/// A checkbox for alignment settings: on when every one of `keys` is.
fn align_toggle(app: &mut FloraStackApp, ui: &mut egui::Ui, keys: &[&str], texts: Texts<'_>, enabled: bool) {
    let (label, tip, anchor) = texts;
    let mut on = keys.iter().all(|k| app.shell.align_settings[*k].as_bool().unwrap_or(false));
    let response = row(ui, tip, anchor, |ui| ui.add_enabled(enabled, egui::Checkbox::new(&mut on, tr(label))));
    if response.changed() {
        let patch: serde_json::Map<String, Value> = keys.iter().map(|k| ((*k).to_owned(), json!(on))).collect();
        set_align(app, Value::Object(patch), None);
    }
}

/// A slider for a numeric alignment setting.
fn align_slider(app: &mut FloraStackApp, ui: &mut egui::Ui, key: &str, range: std::ops::RangeInclusive<f64>, texts: Texts<'_>, enabled: bool) {
    let (label, tip, anchor) = texts;
    let mut value = app.shell.align_settings[key].as_f64().unwrap_or(*range.start());
    let response = row(ui, tip, anchor, |ui| ui.add_enabled(enabled, egui::Slider::new(&mut value, range).text(tr(label))));
    if response.changed() {
        set_align(app, json!({ key: value }), Some(key));
    }
}

/// Draws the Stack tab.
pub fn show(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.heading(tr("tab-stack"));
    if !app.has_project() {
        ui.label(tr("stack-no-project"));
        return;
    }
    workflow(app, ui);
    if app.threshold_step {
        threshold_step(app, ui);
    }
    ui.separator();
    settings(app, ui);
    ui.separator();
    outputs(app, ui);
}

/// The three steps: frames, alignment with its quality, and stacking.
fn workflow(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let frames = app.shell.frames.len();
    let aligning = app.shell.running(&["align.run"]);
    let stacking = app.shell.running(&["stack.average", "stack.pyramid", "stack.depthMap", "stack.slabbed"]);
    egui::Grid::new("workflow").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label(RichText::new(tr_args("step-frames", &[("count", frames.into())])).strong());
        if ui.button(Action::AddFrames.label()).clicked() {
            app.perform(&ctx, Action::AddFrames);
        }
        ui.end_row();
        ui.label(RichText::new(tr("step-align")).strong());
        if ui.add_enabled(frames > 0 && !aligning && !stacking, egui::Button::new(tr("stack-align"))).clicked() {
            app.perform(&ctx, Action::Align);
        }
        ui.end_row();
        ui.label(RichText::new(tr("step-stack")).strong());
        ui.horizontal(|ui| {
            let aligned = app.aligned();
            if ui.add_enabled(aligned, egui::Button::new(tr("stack-preview"))).on_hover_text(tr("tip-preview")).clicked() {
                app.perform(&ctx, Action::Preview);
            }
            // A stack can be queued while another runs; the threshold step stands in for the button while it is open.
            let button = egui::Button::new(tr("stack-run"));
            if ui.add_enabled(aligned && !aligning && !app.threshold_step, button).on_hover_text(tr("tip-stack")).clicked() {
                app.perform(&ctx, Action::Stack);
            }
        });
        ui.end_row();
    });
    alignment_status(app, ui);
    if app.shell.preview.is_some() && ui.selectable_label(app.selection == Selection::Preview, tr("stack-show-preview")).clicked() {
        app.select(Selection::Preview);
    }
}

/// How alignment went: how many frames are aligned, flagged and rejected, and whether the settings changed since.
fn alignment_status(app: &FloraStackApp, ui: &mut egui::Ui) {
    let frames = &app.shell.frames;
    let enabled = frames.iter().filter(|f| f.enabled).count();
    let aligned = frames.iter().filter(|f| f.enabled && f.alignment.is_some()).count();
    if aligned == 0 {
        ui.weak(tr("align-none"));
        return;
    }
    let flagged = frames.iter().filter(|f| f.enabled && !f.flags.is_empty()).count();
    let rejected = frames.iter().filter(|f| f.alignment.as_ref().is_some_and(|a| a["status"] == "rejected")).count();
    ui.weak(tr_args(
        "align-summary",
        &[("aligned", aligned.into()), ("count", enabled.into()), ("flagged", flagged.into()), ("rejected", rejected.into())],
    ));
    // The settings it ran with, but the framing, which applies when stacking.
    let without_framing = |v: &Value| {
        let mut v = v.clone();
        if let Some(map) = v.as_object_mut() {
            map.remove("framing");
        }
        v
    };
    let ran = &app.shell.alignment["run"]["settings"];
    let warn = ui.visuals().warn_fg_color;
    if !ran.is_null() && without_framing(ran) != without_framing(&app.shell.align_settings) {
        ui.colored_label(warn, tr("align-changed"));
    } else if aligned < enabled {
        ui.colored_label(warn, tr_args("align-missing", &[("count", (enabled - aligned).into())]));
    }
}

/// The settings: the method, radius and smoothing, the alignment's terms and the framing, then the Advanced section.
fn settings(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    if app.shell.stack_settings.is_null() || app.shell.align_settings.is_null() {
        return;
    }
    ui.label(RichText::new(tr("stack-method")).strong());
    for method in Method::ALL {
        let tip = match method {
            Method::Pyramid => "tip-method-pyramid",
            Method::DepthMap => "tip-method-depth-map",
            Method::Average => "tip-method-average",
            Method::Both => "tip-method-both",
        };
        let response = row(ui, tip, "method", |ui| ui.radio(app.prefs.method == method, tr(method.label_key())));
        if response.clicked() && app.prefs.method != method {
            app.set_method(method);
            app.preview_method = if method == Method::DepthMap { "depth-map" } else { method.name() };
            app.preview_if_aligned();
        }
    }
    ui.add_space(6.0);
    // Radius and smoothing are the weighted average's and the depth map's; the pyramid has none.
    let group = match app.prefs.method {
        Method::Average => Some("average"),
        Method::DepthMap | Method::Both => Some("depth_map"),
        Method::Pyramid => None,
    };
    if let Some(group) = group {
        let smoothing = if group == "depth_map" { 256.0 } else { 64.0 };
        stack_slider(app, ui, (group, "radius"), 0.0..=64.0, ("setting-radius", "tip-radius", "radius"));
        stack_slider(app, ui, (group, "smoothing"), 0.0..=smoothing, ("setting-smoothing", "tip-smoothing", "smoothing"));
    } else {
        ui.add_enabled_ui(false, |ui| {
            let (mut radius, mut smoothing) = (0.0, 0.0);
            row(ui, "tip-radius-pyramid", "radius", |ui| ui.add(egui::Slider::new(&mut radius, 0.0..=64.0).text(tr("setting-radius"))));
            row(ui, "tip-radius-pyramid", "smoothing", |ui| ui.add(egui::Slider::new(&mut smoothing, 0.0..=64.0).text(tr("setting-smoothing"))));
        });
    }
    ui.add_space(6.0);
    // Alignment settings can't change while alignment or a stack reads them.
    let free = !app.shell.running(&["align.run", "stack.average", "stack.pyramid", "stack.depthMap", "stack.slabbed"]);
    ui.label(RichText::new(tr("align-terms")).strong());
    align_toggle(app, ui, &["shift_x", "shift_y"], ("align-shift", "tip-align-shift", "alignment"), free);
    align_toggle(app, ui, &["scale"], ("align-scale", "tip-align-scale", "alignment"), free);
    align_toggle(app, ui, &["rotation"], ("align-rotation", "tip-align-rotation", "alignment"), free);
    align_toggle(app, ui, &["brightness"], ("align-brightness", "tip-align-brightness", "alignment"), free);
    ui.add_space(6.0);
    let framing = app.shell.align_settings["framing"].as_str().unwrap_or("narrow").to_owned();
    let mut picked = None;
    ui.label(RichText::new(tr("setting-framing")).strong());
    for (value, label, tip) in [
        ("narrow", "framing-narrow", "tip-framing-narrow"),
        ("wide", "framing-wide", "tip-framing-wide"),
        ("common", "framing-common", "tip-framing-common"),
    ] {
        let response = row(ui, tip, "framing", |ui| ui.add_enabled(free, egui::RadioButton::new(framing == value, tr(label))));
        if response.clicked() && framing != value {
            picked = Some(value);
        }
    }
    if let Some(value) = picked {
        set_align(app, json!({ "framing": value }), None);
        app.preview_if_aligned();
    }
    ui.add_space(6.0);
    egui::CollapsingHeader::new(tr("advanced")).id_salt("advanced").show(ui, |ui| advanced(app, ui, free));
}

/// Everything else: the methods' other settings, and alignment's.
fn advanced(app: &mut FloraStackApp, ui: &mut egui::Ui, free: bool) {
    let method = app.prefs.method;
    if matches!(method, Method::Pyramid | Method::Both) {
        ui.label(RichText::new(tr("method-pyramid")).strong());
        stack_slider(app, ui, ("pyramid", "grit"), 0.0..=1.0, ("setting-grit", "tip-grit", "grit"));
        stack_slider(app, ui, ("pyramid", "range_limit"), 0.0..=1.0, ("setting-range-limit", "tip-range-limit", "range-limit"));
        stack_slider(app, ui, ("pyramid", "levels"), 0.0..=10.0, ("setting-levels", "tip-levels", "levels"));
        let tones = [("compress", "tone-compress"), ("clip", "tone-clip"), ("keep", "tone-keep")];
        stack_choice(app, ui, ("pyramid", "tone"), &tones, ("setting-tone", "tip-tone", "tone"));
        let decisions = [("luminance", "decisions-luminance"), ("channels", "decisions-channels")];
        stack_choice(app, ui, ("pyramid", "decisions"), &decisions, ("setting-decisions", "tip-decisions", "decisions"));
    }
    if method == Method::Average {
        ui.label(RichText::new(tr("method-average")).strong());
        stack_slider(app, ui, ("average", "power"), 0.5..=4.0, ("setting-power", "tip-power", "power"));
    }
    if method.uses_depth() {
        ui.label(RichText::new(tr("method-depth-map")).strong());
        ui.weak(tr("threshold-when-stacking"));
    }
    ui.add_space(4.0);
    ui.label(RichText::new(tr("align-heading")).strong());
    align_toggle(app, ui, &["refine"], ("align-refine", "tip-align-refine", "refine"), free);
    align_slider(app, ui, "max_shift", 0.0..=50.0, ("align-max-shift", "tip-align-limits", "limits"), free);
    align_slider(app, ui, "max_scale", 0.0..=50.0, ("align-max-scale", "tip-align-limits", "limits"), free);
    align_slider(app, ui, "max_rotation", 0.0..=45.0, ("align-max-rotation", "tip-align-limits", "limits"), free);
    align_slider(app, ui, "min_score", 0.0..=1.0, ("align-min-score", "tip-align-min-score", "match-score"), free);
    align_toggle(app, ui, &["reject"], ("align-reject", "tip-align-reject", "reject"), free);
    align_slider(app, ui, "resolution", 64.0..=16384.0, ("align-resolution", "tip-align-resolution", "resolution"), free);
    align_toggle(app, ui, &["automatic_order"], ("align-automatic-order", "tip-align-automatic-order", "order"), free);
    align_toggle(app, ui, &["reverse_order"], ("align-reverse-order", "tip-align-reverse-order", "order"), free);
    align_slider(app, ui, "min_sharp_fraction", 0.0..=1.0, ("align-min-sharp", "tip-align-min-sharp", "out-of-focus"), free);
    align_slider(app, ui, "min_motion", 0.1..=100.0, ("align-min-motion", "tip-align-min-motion", "motion"), free);
    align_toggle(app, ui, &["detect_defects"], ("align-detect-defects", "tip-align-detect-defects", "defects"), free);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.small_button(tr("stack-reset")).clicked()
            && let Some(result) = app.shell.run("stack.reset", json!({}))
        {
            app.shell.stack_settings = result["settings"].clone();
            app.preview_if_aligned();
        }
        if ui.add_enabled(free, egui::Button::new(tr("align-reset")).small()).clicked()
            && let Some(result) = app.shell.run("align.reset", json!({}))
        {
            app.shell.align_settings = result["settings"].clone();
        }
    });
}

/// The depth map's contrast threshold, before the stack: its mode and value over a live picture of what falls below
/// it, then Continue.
fn threshold_step(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    egui::Frame::group(ui.style()).fill(ui.visuals().faint_bg_color).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("threshold-title")).strong());
            learn_more(ui, "contrast-threshold");
        });
        ui.label(tr("threshold-text"));
        let depth = app.shell.stack_settings["depth_map"].clone();
        let mode = depth["threshold"].as_str().unwrap_or("auto").to_owned();
        let shown = app.shell.threshold.clone();
        let measuring = app.shell.running(&["stack.threshold"]);
        let mut changed = false;
        ui.horizontal(|ui| {
            for (value, label) in [("auto", "threshold-auto"), ("percentile", "threshold-percentile"), ("level", "threshold-level")] {
                if ui.radio(mode == value, tr(label)).on_hover_text(tr("tip-threshold-mode")).clicked() && mode != value {
                    // The new mode starts where the threshold is now, so that the picture stays as it was.
                    match (value, &shown) {
                        ("percentile", Some(s)) => {
                            set_stack(app, "depth_map", "percentile", &json!((s["below"].as_f64().unwrap_or(0.3) * 100.0).round()), false);
                        }
                        ("level", Some(s)) => set_stack(app, "depth_map", "level", &s["level"], false),
                        _ => {}
                    }
                    set_stack(app, "depth_map", "threshold", &json!(value), false);
                    changed = true;
                }
            }
        });
        let slider = match mode.as_str() {
            "percentile" => Some(("percentile", 0.0..=100.0, false)),
            "level" => Some(("level", 1e-5..=0.1, true)),
            _ => None,
        };
        if let Some((key, range, logarithmic)) = slider {
            let mut value = depth[key].as_f64().unwrap_or(*range.start());
            let label = if key == "percentile" { "threshold-percentile" } else { "threshold-level" };
            let response = ui.add(egui::Slider::new(&mut value, range).logarithmic(logarithmic).text(tr(label))).on_hover_text(tr("tip-threshold"));
            if response.changed() {
                set_stack(app, "depth_map", key, &json!(value), true);
                changed = true;
            }
        }
        if changed {
            app.shell.run("stack.threshold", json!({}));
        }
        match (&shown, measuring) {
            (Some(s), false) => {
                let below = (s["below"].as_f64().unwrap_or(0.0) * 100.0).round();
                ui.weak(tr_args("threshold-below", &[("share", below.into())]));
            }
            _ => {
                ui.weak(tr("threshold-measuring"));
            }
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(shown.is_some(), egui::Button::new(tr("threshold-continue"))).clicked() {
                app.stack_now();
            }
            if ui.button(tr("cancel")).clicked() {
                app.close_threshold();
            }
        });
    });
}

/// A number as short text: at most three decimals, no trailing zeros.
fn short(v: f64) -> String {
    let text = format!("{v:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// What an output was made with, in a line: its method and main parameters.
pub fn made_with(method: &str, parameters: &Value) -> String {
    let (label, keys): (&str, &[(&str, &str)]) = match method {
        "average" => ("method-average", &[("radius", "setting-radius"), ("smoothing", "setting-smoothing"), ("power", "setting-power")]),
        "pyramid" => ("method-pyramid", &[("grit", "setting-grit"), ("rangeLimit", "setting-range-limit")]),
        "depth-map" => ("method-depth-map", &[("radius", "setting-radius"), ("smoothing", "setting-smoothing")]),
        other => return other.to_owned(),
    };
    let mut parts = vec![tr(label)];
    for (key, name) in keys {
        if let Some(v) = parameters[*key].as_f64() {
            parts.push(format!("{} {}", tr(name).to_lowercase(), short(v)));
        }
    }
    if method == "depth-map" {
        match parameters["threshold"].as_str() {
            Some("percentile") => {
                parts.push(tr_args("made-percentile", &[("value", short(parameters["percentile"].as_f64().unwrap_or(0.0)).into())]));
            }
            Some("level") => parts.push(tr_args("made-level", &[("value", short(parameters["level"].as_f64().unwrap_or(0.0)).into())])),
            _ => parts.push(tr("made-auto")),
        }
    }
    parts.join(" · ")
}

/// The outputs, oldest first: each with what it was made with, to show it or to pick it for comparing.
pub fn outputs(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    if app.shell.outputs.is_empty() {
        return;
    }
    ui.label(RichText::new(tr("outputs-heading")).strong());
    for output in app.shell.outputs.clone() {
        ui.horizontal(|ui| {
            let mut picked = app.compare.contains(&output.id);
            if ui.checkbox(&mut picked, tr("output-compare")).on_hover_text(tr("tip-compare")).changed() {
                app.pick_compare(output.id);
            }
            let selected = app.selection == Selection::Output(output.id);
            let parameters = serde_json::to_string_pretty(&output.parameters).unwrap_or_default();
            if ui.selectable_label(selected, &output.name).on_hover_text(parameters).clicked() {
                app.select(Selection::Output(output.id));
            }
        });
        ui.weak(made_with(&output.method, &output.parameters));
    }
    if app.compare.len() == 2 {
        ui.horizontal(|ui| {
            for (mode, label) in [(Compare::Split, "compare-split"), (Compare::SideBySide, "compare-side")] {
                if ui.radio(app.prefs.compare == mode, tr(label)).clicked() {
                    app.prefs.compare = mode;
                    app.viewer.set_compare(mode);
                }
            }
        });
    } else if app.shell.outputs.len() > 1 {
        ui.weak(tr("compare-hint"));
    }
}

impl FloraStackApp {
    /// Whether the project's frames are aligned: what stacking and previews need.
    pub fn aligned(&self) -> bool {
        self.shell.engine().project().is_some_and(|p| p.alignment.is_some())
    }

    /// Asks for a preview of the current method, when the frames are aligned; while the threshold step is open, the
    /// threshold's picture follows instead.
    pub fn preview_if_aligned(&mut self) {
        if self.threshold_step {
            self.shell.run("stack.threshold", json!({}));
        } else if self.aligned() {
            self.preview();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_say_what_they_were_made_with() {
        let average = made_with("average", &json!({ "radius": 2.0, "smoothing": 8.0, "power": 3.0, "framing": "narrow" }));
        assert_eq!(average, "Weighted average · radius 2 · smoothing 8 · power 3");
        let depth = made_with("depth-map", &json!({ "radius": 2.5, "smoothing": 8.0, "threshold": "percentile", "percentile": 40.0 }));
        assert_eq!(depth, "Depth map · radius 2.5 · smoothing 8 · fills 40%");
        assert_eq!(made_with("pyramid", &json!({ "grit": 0.25, "rangeLimit": 1.0 })), "Pyramid · grit 0.25 · range limit 1");
        assert_eq!(made_with("slab", &json!({})), "slab", "an unknown method shows as its name");
        assert!(docs_url("radius").ends_with("/blob/main/docs/stack-tab.md#radius"));
    }

    /// GitHub's anchor for a heading: lower case, spaces as hyphens, other punctuation dropped.
    fn anchor(heading: &str) -> String {
        heading
            .trim()
            .to_lowercase()
            .chars()
            .filter_map(|c| match c {
                ' ' => Some('-'),
                c if c.is_alphanumeric() || c == '-' => Some(c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_learn_more_link_has_its_section_in_the_docs() {
        let docs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/stack-tab.md")).unwrap();
        let sections: Vec<String> = docs.lines().filter_map(|l| l.strip_prefix("## ")).map(anchor).collect();
        let source = include_str!("stack_tab.rs");
        // A setting's section is the literal after its tooltip's key, or the one `learn_more` and `row` are given.
        let mut anchors: Vec<&str> = source
            .split("\"tip-")
            .skip(1)
            .filter_map(|piece| piece.split_once('"').and_then(|(_, rest)| rest.strip_prefix(", \"")).and_then(|rest| rest.split('"').next()))
            .collect();
        for call in ["learn_more(ui, \"", "row(ui, tip, \""] {
            anchors.extend(source.split(call).skip(1).filter_map(|piece| piece.split('"').next()));
        }
        let anchors: Vec<&str> = anchors.into_iter().filter(|a| !a.is_empty() && a.chars().all(|c| c.is_ascii_lowercase() || c == '-')).collect();
        assert!(anchors.len() > 20, "{anchors:?}");
        for a in anchors {
            assert!(sections.iter().any(|s| s == a), "docs/stack-tab.md has no section for #{a}: {sections:?}");
        }
    }
}
