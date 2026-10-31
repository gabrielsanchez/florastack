//! The window's panels: the top bar (menus, the project's name, the workflow tabs, the jobs with their progress and
//! Cancel), the frame list on the left, the settings of the open tab on the right, the status line, the viewer in the
//! middle, and the dialogs over them. They draw what [`FloraStackApp`] holds and turn clicks into its actions and the
//! engine's commands; nothing here computes anything about the project.

use egui::{Align, Layout, RichText, Sense};
use florastack_engine::{Format, OutputSpec, Value, json};

use crate::actions::Action;
use crate::app::{Banner, Dialog, FloraStackApp, Selection};
use crate::i18n::{tr, tr_args};
use crate::prefs::{Method, Tab};

/// The height of a frame list row's thumbnail, in points.
const ROW_THUMBNAIL: f32 = 40.0;

/// What a form of the Batch tab holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchForm {
    pub input: String,
    pub output: String,
    pub preset: String,
}

/// The formats the Export tab writes, with their files' extensions.
pub const EXPORT_FORMATS: [(Format, &str, &str); 5] = [
    (Format::Tiff, "TIFF", "tif"),
    (Format::Jpeg, "JPEG", "jpg"),
    (Format::Png, "PNG", "png"),
    (Format::OpenExr, "OpenEXR", "exr"),
    (Format::Dng, "DNG", "dng"),
];

/// Draws the whole window.
pub fn show(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    egui::Panel::top("top-bar").show(ui, |ui| top_bar(app, ui));
    egui::Panel::bottom("status").show(ui, |ui| status(app, ui));
    egui::Panel::left("frames").resizable(true).default_size(260.0).size_range(180.0..=520.0).show(ui, |ui| frame_list(app, ui));
    egui::Panel::right("settings").resizable(true).default_size(300.0).size_range(220.0..=560.0).show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| match app.prefs.tab {
            Tab::Stack => stack_tab(app, ui),
            Tab::Retouch => {
                ui.heading(tr("tab-retouch"));
                ui.label(tr("retouch-later"));
            }
            Tab::Export => export_tab(app, ui),
            Tab::Batch => batch_tab(app, ui),
        });
    });
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
        banners(app, ui);
        if app.viewer.is_empty() {
            empty_view(app, ui);
        } else {
            app.viewer.ui(ui);
        }
    });
    dialogs(app, ui.ctx());
    drop_overlay(ui.ctx());
}

/// A menu item that runs `action`, with its shortcut.
fn item(app: &mut FloraStackApp, ui: &mut egui::Ui, action: Action, enabled: bool) {
    let shortcut = action.shortcut().map(|s| ui.ctx().format_shortcut(&s)).unwrap_or_default();
    let label = match action {
        Action::Undo => app.shell.engine().undo_label().map(|w| tr_args("action-undo-named", &[("what", w.to_owned().into())])),
        Action::Redo => app.shell.engine().redo_label().map(|w| tr_args("action-redo-named", &[("what", w.to_owned().into())])),
        _ => None,
    };
    let button = egui::Button::new(label.unwrap_or_else(|| action.label())).shortcut_text(shortcut);
    if ui.add_enabled(enabled, button).clicked() {
        let ctx = ui.ctx().clone();
        app.perform(&ctx, action);
    }
}

fn top_bar(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let project = app.has_project();
    let busy = app.shell.busy();
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button(tr("menu-file"), |ui| {
            item(app, ui, Action::New, true);
            item(app, ui, Action::Open, true);
            ui.menu_button(tr("menu-recent"), |ui| {
                if app.prefs.recent.is_empty() {
                    ui.weak(tr("menu-recent-none"));
                }
                for path in app.prefs.recent.clone() {
                    if ui.button(path.display().to_string()).clicked() {
                        app.dropped(vec![path]);
                    }
                }
            });
            ui.separator();
            item(app, ui, Action::AddFrames, true);
            item(app, ui, Action::ImportStacks, true);
            ui.separator();
            item(app, ui, Action::Save, project);
            item(app, ui, Action::SaveAs, project);
            item(app, ui, Action::Close, project);
            ui.separator();
            item(app, ui, Action::Quit, true);
        });
        ui.menu_button(tr("menu-edit"), |ui| {
            let (undo, redo) = (app.shell.engine().undo_label().is_some(), app.shell.engine().redo_label().is_some());
            item(app, ui, Action::Undo, undo);
            item(app, ui, Action::Redo, redo);
            ui.separator();
            item(app, ui, Action::RemoveFrame, matches!(app.selection, Selection::Frame(_)));
            ui.separator();
            item(app, ui, Action::Palette, true);
        });
        ui.menu_button(tr("menu-stack"), |ui| {
            item(app, ui, Action::Align, project);
            item(app, ui, Action::Preview, project);
            item(app, ui, Action::Stack, project);
            ui.separator();
            item(app, ui, Action::CancelJobs, busy);
        });
        ui.menu_button(tr("menu-view"), |ui| {
            for action in [Action::ZoomFit, Action::Zoom100, Action::Zoom200, Action::ZoomIn, Action::ZoomOut] {
                item(app, ui, action, true);
            }
            ui.separator();
            item(app, ui, Action::NextFrame, true);
            item(app, ui, Action::PreviousFrame, true);
            ui.separator();
            item(app, ui, Action::ToggleTheme, true);
        });
        ui.menu_button(tr("menu-help"), |ui| {
            item(app, ui, Action::Shortcuts, true);
            if ui.button(tr_args("menu-about", &[("name", florastack_engine::branding::DISPLAY_NAME.into())])).clicked() {
                app.dialog = Some(Dialog::About);
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(title) = app.project_title() {
                let dirty = app.shell.engine().is_dirty();
                let text = if dirty { tr_args("title-unsaved", &[("name", title.into())]) } else { title };
                ui.strong(text).on_hover_text(app.shell.engine().project_path().map(|p| p.display().to_string()).unwrap_or_default());
            }
        });
    });
    ui.horizontal(|ui| {
        for tab in Tab::ALL {
            if ui.selectable_label(app.prefs.tab == tab, tr(tab.label_key())).clicked() {
                app.set_tab(tab);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| jobs(app, ui));
    });
}

/// The running jobs: each one's progress, and a Cancel for them all.
fn jobs(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let running: Vec<_> = app.shell.jobs.values().filter(|j| !j.state.is_finished()).cloned().collect();
    if running.is_empty() {
        return;
    }
    if ui.button(tr("job-cancel")).clicked() {
        let ctx = ui.ctx().clone();
        app.perform(&ctx, Action::CancelJobs);
    }
    for job in running.iter().rev() {
        let progress = &job.progress;
        let mut text = progress.stage.clone().unwrap_or_else(|| job.title.clone());
        if let Some(eta) = progress.eta_ms {
            text = tr_args("job-eta", &[("stage", text.into()), ("time", duration_text(eta).into())]);
        }
        if job.cancel_requested {
            text = tr("job-cancelling");
        }
        ui.add(egui::ProgressBar::new(progress.fraction.clamp(0.0, 1.0)).desired_width(220.0).text(text)).on_hover_text(&job.title);
    }
}

/// A duration in milliseconds as minutes and seconds, `1:05`.
pub fn duration_text(ms: u64) -> String {
    let seconds = ms.div_ceil(1000);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn status(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        if let Some(notice) = app.shell.notices.last().cloned() {
            let color = if notice.error { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
            if ui.small_button(tr("notice-dismiss")).clicked() {
                app.shell.notices.pop();
            }
            ui.colored_label(color, notice.text);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let compute = app.shell.compute().clone();
            let device = match compute["device"].as_str() {
                Some("gpu") => tr_args("device-gpu", &[("name", compute["adapter"]["name"].as_str().unwrap_or("").to_owned().into())]),
                Some(_) => tr("device-cpu"),
                None => String::new(),
            };
            ui.weak(device).on_hover_text(compute["reason"].as_str().unwrap_or(""));
            if !app.viewer.is_empty() {
                ui.weak(format!("{:.0}%", app.viewer.zoom() * 100.0));
            }
        });
    });
}

/// The frame list: thumbnails, names, flags, the enabled checkboxes, and reordering by dragging.
fn frame_list(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading(tr("frames-heading"));
        ui.weak(app.shell.frames.len().to_string());
    });
    if !app.has_project() || app.shell.frames.is_empty() {
        ui.label(tr("frames-empty"));
        if ui.button(Action::AddFrames.label()).clicked() {
            let ctx = ui.ctx().clone();
            app.perform(&ctx, Action::AddFrames);
        }
        return;
    }
    let frames = app.shell.frames.clone();
    let mut moved: Option<(u64, usize)> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (i, frame) in frames.iter().enumerate() {
            let selected = app.selection == Selection::Frame(frame.id);
            let drag = ui.dnd_drag_source(egui::Id::unique(("frame-row", frame.id)), i, |ui| {
                ui.horizontal(|ui| {
                    let mut enabled = frame.enabled;
                    if ui.checkbox(&mut enabled, "").on_hover_text(tr("frames-enabled")).changed() {
                        app.shell.run("project.setFramesEnabled", json!({ "frames": [frame.id], "enabled": enabled }));
                    }
                    let size = egui::vec2(ROW_THUMBNAIL * 1.5, ROW_THUMBNAIL);
                    match frame.preview.as_ref() {
                        Some(preview) => {
                            app.want_thumbnail(preview);
                            match app.thumbnails.get(preview) {
                                Some(texture) => {
                                    ui.add(egui::Image::new(texture).fit_to_exact_size(size).maintain_aspect_ratio(true));
                                }
                                None => {
                                    ui.allocate_exact_size(size, Sense::hover());
                                }
                            }
                        }
                        None => {
                            ui.allocate_exact_size(size, Sense::hover());
                        }
                    }
                    ui.vertical(|ui| {
                        let name = if frame.enabled { RichText::new(&frame.name) } else { RichText::new(&frame.name).weak().strikethrough() };
                        if ui.selectable_label(selected, name).on_hover_text(frame.file.display().to_string()).clicked() {
                            app.select(Selection::Frame(frame.id));
                        }
                        if !frame.flags.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                for flag in &frame.flags {
                                    let (text, help) = flag_text(flag);
                                    ui.colored_label(ui.visuals().warn_fg_color, RichText::new(text).small()).on_hover_text(help);
                                }
                            });
                        }
                    });
                });
            });
            let row = drag.response;
            if let (Some(pointer), Some(_)) = (ui.input(|i| i.pointer.interact_pos()), row.dnd_hover_payload::<usize>()) {
                let below = pointer.y > row.rect.center().y;
                let y = if below { row.rect.bottom() } else { row.rect.top() };
                ui.painter().hline(row.rect.x_range(), y, ui.visuals().selection.stroke);
                if let Some(from) = row.dnd_release_payload::<usize>() {
                    let target = if below { i + 1 } else { i };
                    // The position among the frames without the one moved.
                    let to = if *from < target { target - 1 } else { target };
                    if let Some(f) = frames.get(*from) {
                        moved = Some((f.id, to));
                    }
                }
            }
        }
    });
    if let Some((id, to)) = moved {
        app.shell.run("project.moveFrames", json!({ "frames": [id], "to": to }));
    }
}

/// A flag's label and its explanation.
fn flag_text(flag: &str) -> (String, String) {
    let (label, help) = match flag {
        "limit" => ("flag-limit", "flag-limit-help"),
        "low-score" => ("flag-low-score", "flag-low-score-help"),
        "repeating-pattern" => ("flag-repeating-pattern", "flag-repeating-pattern-help"),
        "no-match" => ("flag-no-match", "flag-no-match-help"),
        "not-refined" => ("flag-not-refined", "flag-not-refined-help"),
        "out-of-focus" => ("flag-out-of-focus", "flag-out-of-focus-help"),
        "moved" => ("flag-moved", "flag-moved-help"),
        other => return (other.to_owned(), String::new()),
    };
    (tr(label), tr(help))
}

/// The settings of the stacking methods, as `stack.get` gives them: read again when the project changed.
fn stack_settings(app: &mut FloraStackApp) -> Value {
    let revision = app.shell.revision;
    if app.settings.as_ref().is_none_or(|(r, _)| *r != revision) {
        let settings = app.shell.run("stack.get", json!({})).map_or(Value::Null, |v| v["settings"].clone());
        app.settings = Some((revision, settings));
    }
    app.settings.as_ref().map_or(Value::Null, |(_, s)| s.clone())
}

/// A slider for a numeric stacking setting, which `stack.set` changes (one undo step per drag) and a preview follows.
fn slider(
    app: &mut FloraStackApp,
    ui: &mut egui::Ui,
    settings: &Value,
    (group, key): (&str, &str),
    range: std::ops::RangeInclusive<f64>,
    label: &str,
) {
    let mut value = settings[group][key].as_f64().unwrap_or(*range.start());
    let response = ui.add(egui::Slider::new(&mut value, range).text(tr(label)));
    if response.changed() {
        app.shell.run_with("stack.set", json!({ group: { key: value } }), Some(format!("stack.set {group}.{key}")));
        if let Some((_, s)) = &mut app.settings {
            s[group][key] = json!(value);
        }
    }
    if response.drag_stopped() || (response.changed() && !response.dragged()) {
        app.preview_if_aligned();
    }
}

/// A choice among a stacking setting's named values.
fn choice(app: &mut FloraStackApp, ui: &mut egui::Ui, settings: &Value, (group, key): (&str, &str), options: &[(&str, &str)], label: &str) {
    let current = settings[group][key].as_str().unwrap_or("").to_owned();
    let shown = options.iter().find(|(v, _)| *v == current).map_or_else(|| current.clone(), |(_, l)| tr(l));
    egui::ComboBox::from_label(tr(label)).selected_text(shown).show_ui(ui, |ui| {
        for (value, option) in options {
            if ui.selectable_label(current == *value, tr(option)).clicked() && current != *value {
                app.shell.run("stack.set", json!({ group: { key: value } }));
                app.preview_if_aligned();
            }
        }
    });
}

fn stack_tab(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.heading(tr("tab-stack"));
    if !app.has_project() {
        ui.label(tr("stack-no-project"));
        return;
    }
    ui.add_space(4.0);
    ui.label(RichText::new(tr("stack-method")).strong());
    for method in Method::ALL {
        if ui.radio(app.prefs.method == method, tr(method.label_key())).clicked() && app.prefs.method != method {
            app.set_method(method);
            app.preview_if_aligned();
        }
    }
    ui.add_space(8.0);
    let settings = stack_settings(app);
    if !settings.is_null() {
        ui.label(RichText::new(tr("stack-settings")).strong());
        match app.prefs.method {
            Method::Average => {
                slider(app, ui, &settings, ("average", "radius"), 0.0..=64.0, "setting-radius");
                slider(app, ui, &settings, ("average", "smoothing"), 0.0..=64.0, "setting-smoothing");
                slider(app, ui, &settings, ("average", "power"), 0.5..=4.0, "setting-power");
            }
            Method::Pyramid => {
                slider(app, ui, &settings, ("pyramid", "grit"), 0.0..=1.0, "setting-grit");
                slider(app, ui, &settings, ("pyramid", "range_limit"), 0.0..=1.0, "setting-range-limit");
                let tones = [("compress", "tone-compress"), ("clip", "tone-clip"), ("keep", "tone-keep")];
                choice(app, ui, &settings, ("pyramid", "tone"), &tones, "setting-tone");
                let decisions = [("luminance", "decisions-luminance"), ("channels", "decisions-channels")];
                choice(app, ui, &settings, ("pyramid", "decisions"), &decisions, "setting-decisions");
            }
            Method::DepthMap => {
                slider(app, ui, &settings, ("depth_map", "radius"), 0.0..=64.0, "setting-radius");
                slider(app, ui, &settings, ("depth_map", "smoothing"), 0.0..=256.0, "setting-smoothing");
                let modes = [("auto", "threshold-auto"), ("level", "threshold-level"), ("percentile", "threshold-percentile")];
                choice(app, ui, &settings, ("depth_map", "threshold"), &modes, "setting-threshold");
                match settings["depth_map"]["threshold"].as_str() {
                    Some("level") => slider(app, ui, &settings, ("depth_map", "level"), 0.0..=0.05, "setting-level"),
                    Some("percentile") => slider(app, ui, &settings, ("depth_map", "percentile"), 0.0..=100.0, "setting-percentile"),
                    _ => {}
                }
            }
        }
        if ui.small_button(tr("stack-reset")).clicked() {
            app.shell.run("stack.reset", json!({}));
        }
    }
    ui.add_space(8.0);
    ui.separator();
    let busy = app.shell.busy();
    let ctx = ui.ctx().clone();
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(!busy, egui::Button::new(tr("stack-align"))).clicked() {
            app.perform(&ctx, Action::Align);
        }
        if ui.add_enabled(app.aligned(), egui::Button::new(tr("stack-preview"))).clicked() {
            app.perform(&ctx, Action::Preview);
        }
        if ui.add_enabled(app.aligned() && !busy, egui::Button::new(tr("stack-run"))).clicked() {
            app.perform(&ctx, Action::Stack);
        }
    });
    if !app.aligned() {
        ui.weak(tr("stack-align-first"));
    }
    if app.shell.preview.is_some() && ui.selectable_label(app.selection == Selection::Preview, tr("stack-show-preview")).clicked() {
        app.select(Selection::Preview);
    }
    outputs(app, ui);
}

/// The outputs, to show one.
fn outputs(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    if app.shell.outputs.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.label(RichText::new(tr("outputs-heading")).strong());
    for output in app.shell.outputs.clone() {
        let selected = app.selection == Selection::Output(output.id);
        if ui.selectable_label(selected, &output.name).on_hover_text(&output.method).clicked() {
            app.select(Selection::Output(output.id));
        }
    }
}

fn export_tab(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.heading(tr("tab-export"));
    if app.shell.outputs.is_empty() {
        ui.label(tr("export-nothing"));
        return;
    }
    outputs(app, ui);
    ui.add_space(8.0);
    let (_, name, _) = EXPORT_FORMATS.iter().find(|(f, _, _)| *f == app.export_format).copied().unwrap_or(EXPORT_FORMATS[0]);
    egui::ComboBox::from_label(tr("export-format")).selected_text(name).show_ui(ui, |ui| {
        for (format, name, _) in EXPORT_FORMATS {
            ui.selectable_value(&mut app.export_format, format, name);
        }
    });
    let Selection::Output(id) = app.selection else {
        ui.weak(tr("export-pick"));
        return;
    };
    if ui.button(tr("export-save")).clicked() {
        app.export(id);
    }
    ui.horizontal(|ui| {
        if ui.button(tr("export-remove")).clicked() {
            app.shell.run("output.remove", json!({ "output": id }));
            app.select(Selection::Nothing);
        }
    });
}

impl FloraStackApp {
    /// Whether the project's frames are aligned: what stacking and previews need.
    pub fn aligned(&self) -> bool {
        self.shell.engine().project().is_some_and(|p| p.alignment.is_some())
    }

    /// Asks for a preview of the current method, when the frames are aligned.
    pub fn preview_if_aligned(&mut self) {
        if self.aligned() {
            let method = self.prefs.method.name();
            self.shell.run("stack.preview", json!({ "method": method }));
            self.follow_preview = true;
        }
    }

    /// Saves output `id` to a file the user picks, in the Export tab's format.
    pub fn export(&mut self, id: u64) {
        let Some((format, name, extension)) = EXPORT_FORMATS.iter().find(|(f, _, _)| *f == self.export_format).copied() else { return };
        let Ok(spec) = OutputSpec::new(format) else { return };
        let stem = self.shell.outputs.iter().find(|o| o.id == id).map_or_else(|| "stack".to_owned(), |o| o.name.clone());
        let mut dialog = rfd::FileDialog::new().add_filter(name, &[extension]).set_file_name(format!("{stem}.{extension}"));
        if let Some(folder) = &self.prefs.last_folder {
            dialog = dialog.set_directory(folder);
        }
        let Some(path) = dialog.save_file() else { return };
        self.save_output(id, &path, spec);
    }

    /// Writes output `id` to `path` with `spec`, replacing the file (the file dialog asked).
    pub fn save_output(&mut self, id: u64, path: &std::path::Path, spec: OutputSpec) {
        let spec = serde_json::to_value(spec).unwrap_or(Value::Null);
        self.shell.run("output.save", json!({ "output": id, "path": path, "spec": spec, "overwrite": true }));
    }
}

fn batch_tab(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    ui.heading(tr("tab-batch"));
    ui.label(tr("batch-intro"));
    ui.add_space(4.0);
    let folder = |ui: &mut egui::Ui, value: &mut String, label: &str, file: bool| {
        ui.label(tr(label));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(value).desired_width(180.0));
            if ui.button(tr("browse")).clicked() {
                let dialog = rfd::FileDialog::new();
                let picked = if file { dialog.add_filter("TOML", &["toml"]).pick_file() } else { dialog.pick_folder() };
                if let Some(path) = picked {
                    *value = path.display().to_string();
                }
            }
        });
    };
    folder(ui, &mut app.batch.input, "batch-input", false);
    folder(ui, &mut app.batch.output, "batch-output", false);
    folder(ui, &mut app.batch.preset, "batch-preset", true);
    ui.label(tr_args("batch-method", &[("method", tr(app.prefs.method.label_key()).into())]));
    let ready = !app.batch.input.is_empty() && !app.batch.output.is_empty() && !app.batch.preset.is_empty();
    if ui.add_enabled(ready, egui::Button::new(tr("batch-run"))).clicked() {
        let form = app.batch.clone();
        let method = app.prefs.method.name();
        app.shell.run("batch.run", json!({ "input": form.input, "output": form.output, "preset": form.preset, "method": method }));
    }
}

/// The banners over the viewer: old temporary projects to trash, frames missing or changed.
fn banners(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let mut gone = Vec::new();
    for (i, banner) in app.banners.clone().into_iter().enumerate() {
        egui::Frame::group(ui.style()).fill(ui.visuals().faint_bg_color).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| match &banner {
                Banner::Orphans { paths, bytes } => {
                    #[allow(clippy::cast_precision_loss, reason = "a size in megabytes, shown rounded")]
                    let megabytes = *bytes as f64 / 1e6;
                    let count = paths.len();
                    ui.label(tr_args("orphans-found", &[("count", count.into()), ("size", format!("{megabytes:.0}").into())]));
                    if ui.button(tr("orphans-trash")).clicked() {
                        app.shell.run("project.trashOrphans", json!({ "paths": paths }));
                        gone.push(i);
                    }
                    if ui.button(tr("orphans-keep")).clicked() {
                        gone.push(i);
                    }
                }
                Banner::Frames { missing, changed } => {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        tr_args("frames-check", &[("missing", (*missing).into()), ("changed", (*changed).into())]),
                    );
                    if ui.button(tr("notice-dismiss")).clicked() {
                        gone.push(i);
                    }
                }
            });
        });
    }
    for i in gone.into_iter().rev() {
        app.banners.remove(i);
    }
}

/// What the viewer shows with nothing selected.
fn empty_view(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Center)), |ui| {
        ui.add_space(rect.height() * 0.35);
        let loading = app.loading.is_some();
        ui.label(RichText::new(if loading { tr("view-loading") } else { tr("view-hint") }).weak());
        if !loading && !app.has_project() && !app.prefs.recent.is_empty() {
            ui.add_space(12.0);
            ui.label(RichText::new(tr("menu-recent")).strong());
            for path in app.prefs.recent.clone().into_iter().take(5) {
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                if ui.link(name).on_hover_text(path.display().to_string()).clicked() {
                    app.dropped(vec![path]);
                }
            }
        }
    });
}

/// The highlight while files are dragged over the window.
fn drop_overlay(ctx: &egui::Context) {
    if ctx.input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop-overlay")));
    painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
    painter.text(screen.center(), egui::Align2::CENTER_CENTER, tr("drop-hint"), egui::FontId::proportional(24.0), egui::Color32::WHITE);
}

fn dialogs(app: &mut FloraStackApp, ctx: &egui::Context) {
    let Some(dialog) = app.dialog.clone() else { return };
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| {
        ui.set_max_width(520.0);
        match dialog {
            Dialog::Unsaved { then } => {
                ui.heading(tr("unsaved-title"));
                ui.label(tr("unsaved-text"));
                ui.horizontal(|ui| {
                    if ui.button(tr("unsaved-save")).clicked() {
                        app.save();
                        close = true;
                        if !app.shell.engine().is_dirty() {
                            app.then(then.clone(), false);
                        }
                    }
                    if ui.button(tr("unsaved-discard")).clicked() {
                        close = true;
                        app.then(then.clone(), true);
                    }
                    if ui.button(tr("cancel")).clicked() {
                        close = true;
                    }
                });
            }
            Dialog::SaveAs { mut name, mut folder } => {
                ui.heading(Action::SaveAs.label());
                ui.label(tr("save-as-name"));
                ui.text_edit_singleline(&mut name);
                ui.label(tr("save-as-folder"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut folder).desired_width(320.0));
                    if ui.button(tr("browse")).clicked()
                        && let Some(picked) = rfd::FileDialog::new().pick_folder()
                    {
                        folder = picked.display().to_string();
                    }
                });
                let ready = !name.trim().is_empty() && !folder.trim().is_empty();
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready, egui::Button::new(tr("save-as-save"))).clicked() {
                        close = true;
                        app.save_as(name.trim(), folder.trim());
                    }
                    if ui.button(tr("cancel")).clicked() {
                        close = true;
                    }
                });
                if !close {
                    app.dialog = Some(Dialog::SaveAs { name, folder });
                }
            }
            Dialog::Shortcuts => {
                ui.heading(Action::Shortcuts.label());
                egui::Grid::new("shortcuts").striped(true).show(ui, |ui| {
                    for action in Action::ALL {
                        if let Some(shortcut) = action.shortcut() {
                            ui.label(action.label());
                            ui.monospace(ctx.format_shortcut(&shortcut));
                            ui.end_row();
                        }
                    }
                });
                if ui.button(tr("close")).clicked() {
                    close = true;
                }
            }
            Dialog::About => {
                ui.heading(florastack_engine::branding::DISPLAY_NAME);
                ui.label(tr_args("about-version", &[("version", env!("CARGO_PKG_VERSION").into())]));
                ui.label(tr("about-text"));
                if ui.button(tr("close")).clicked() {
                    close = true;
                }
            }
            Dialog::Stacks { stacks } => {
                ui.heading(tr("stacks-title"));
                ui.label(tr_args("stacks-found", &[("count", stacks.len().into())]));
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for files in &stacks {
                        let first = files.first().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let label = tr_args("stacks-stack", &[("first", first.into()), ("count", files.len().into())]);
                        if ui.button(label).clicked() {
                            close = true;
                            app.add_frames(files);
                        }
                    }
                });
                if ui.button(tr("cancel")).clicked() {
                    close = true;
                }
            }
        }
    });
    if close || modal.should_close() {
        app.dialog = None;
    }
}
