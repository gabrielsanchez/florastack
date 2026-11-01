//! The window's panels: the top bar (menus, the project's name, the workflow tabs, the jobs with their progress and
//! Cancel), the frame list on the left, the settings of the open tab on the right, the status line, the viewer in the
//! middle, and the dialogs over them. They draw what [`FloraStackApp`] holds and turn clicks into its actions and the
//! engine's commands; nothing here computes anything about the project.

use egui::{Align, Layout, RichText, Sense};
use florastack_engine::{Value, json};

use crate::actions::Action;
use crate::app::{Banner, Dialog, FILM_SIDE, FloraStackApp, Selection, THUMBNAIL_SIDE};
use crate::i18n::{tr, tr_args};
use crate::prefs::{Method, Tab};

/// The height of a frame list row's thumbnail, in points.
const ROW_THUMBNAIL: f32 = 40.0;

/// What a form of the Batch tab holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchForm {
    pub input: String,
    pub output: String,
    /// The preset file, when no built-in preset is picked.
    pub preset: String,
    /// The built-in preset picked, by its id.
    pub built_in: Option<String>,
}

impl BatchForm {
    /// The preset as `batch.run` takes it; `None` until one is picked.
    pub fn preset(&self) -> Option<String> {
        match &self.built_in {
            Some(id) => Some(crate::presets::batch_preset(&crate::presets::Source::BuiltIn(id.clone()))),
            None => Some(self.preset.trim()).filter(|p| !p.is_empty()).map(str::to_owned),
        }
    }
}

/// Draws the whole window.
pub fn show(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    egui::Panel::top("top-bar").show(ui, |ui| top_bar(app, ui));
    egui::Panel::bottom("status").show(ui, |ui| status(app, ui));
    egui::Panel::left("frames").resizable(true).default_size(260.0).size_range(180.0..=520.0).show(ui, |ui| frame_list(app, ui));
    egui::Panel::right("settings").resizable(true).default_size(300.0).size_range(220.0..=560.0).show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| match app.prefs.tab {
            Tab::Stack => crate::stack_tab::show(app, ui),
            Tab::Retouch => crate::retouch_tab::panel(app, ui),
            Tab::Export => crate::export_tab::show(app, ui),
            Tab::Batch => batch_tab(app, ui),
            Tab::Capture => crate::capture_tab::show(app, ui),
        });
    });
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
        banners(app, ui);
        if app.prefs.tab == Tab::Retouch {
            crate::retouch_tab::view(app, ui);
            if app.shell.retouch.is_some() {
                ui.ctx().request_repaint();
            }
        } else if app.film.is_some() {
            film_view(app, ui);
        } else if app.viewer.is_empty() {
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
            item(app, ui, Action::Preferences, true);
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
        crate::presets::menu(app, ui);
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

/// The frame list: thumbnails, names, each frame's alignment quality and flags, the enabled checkboxes, reordering by
/// dragging a frame's name, and the filmstrip: dragging up and down over the thumbnails plays the stack frame by
/// frame, Play plays it on its own.
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
    ui.horizontal(|ui| {
        let playing = app.film.as_ref().is_some_and(|f| f.playing);
        let label = if playing { tr("film-stop") } else { tr("film-play") };
        if ui.button(label).on_hover_text(tr("tip-film")).clicked() {
            let at = app.film.as_ref().map_or(0, |f| f.index);
            if playing { app.film_at(at, false) } else { app.film_at(at, true) }
        }
        ui.checkbox(&mut app.prefs.film_as_shot, tr("film-as-shot")).on_hover_text(tr("tip-film-as-shot"));
    });
    let frames = app.shell.frames.clone();
    let min_score = app.shell.align_settings["min_score"].as_f64().unwrap_or(0.0);
    let mut moved: Option<(u64, usize)> = None;
    // Each row's span, top to bottom, for the scrubbing pointer; and whether a thumbnail is being dragged.
    let mut spans = Vec::with_capacity(frames.len());
    let mut scrubbing = false;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (i, frame) in frames.iter().enumerate() {
            let selected = app.selection == Selection::Frame(frame.id);
            let filmed = app.film.as_ref().is_some_and(|f| f.index == i);
            let row = ui.horizontal(|ui| {
                let mut enabled = frame.enabled;
                if ui.checkbox(&mut enabled, "").on_hover_text(tr("frames-enabled")).changed() {
                    app.shell.run("project.setFramesEnabled", json!({ "frames": [frame.id], "enabled": enabled }));
                }
                let size = egui::vec2(ROW_THUMBNAIL * 1.5, ROW_THUMBNAIL);
                let texture = frame.preview.as_ref().and_then(|p| app.thumbnail(p, THUMBNAIL_SIDE));
                let thumb = match texture {
                    Some(texture) => {
                        ui.add(egui::Image::new(&texture).fit_to_exact_size(size).maintain_aspect_ratio(true).sense(Sense::click_and_drag()))
                    }
                    None => ui.allocate_exact_size(size, Sense::click_and_drag()).1,
                };
                if filmed {
                    ui.painter().rect_stroke(thumb.rect, 2.0, ui.visuals().selection.stroke, egui::StrokeKind::Outside);
                }
                let thumb = thumb.on_hover_text(tr("tip-film"));
                if thumb.is_pointer_button_down_on() || thumb.dragged() {
                    scrubbing = true;
                }
                let info = ui.dnd_drag_source(egui::Id::unique(("frame-row", frame.id)), i, |ui| {
                    ui.vertical(|ui| {
                        let name = if frame.enabled { RichText::new(&frame.name) } else { RichText::new(&frame.name).weak().strikethrough() };
                        if ui.selectable_label(selected, name).on_hover_text(frame.file.display().to_string()).clicked() {
                            app.select(Selection::Frame(frame.id));
                        }
                        if let Some(alignment) = &frame.alignment {
                            quality(ui, alignment, min_score);
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
                info.response
            });
            spans.push(row.response.rect.y_range());
            let row = row.inner;
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
    // Scrubbing: the frame under the pointer, as it moves over the rows.
    if scrubbing && let Some(y) = ui.input(|i| i.pointer.interact_pos()).map(|p| p.y) {
        let index = spans.iter().position(|s| s.contains(y)).unwrap_or(if spans.first().is_some_and(|s| y < s.min) {
            0
        } else {
            spans.len().saturating_sub(1)
        });
        if app.film.as_ref().is_none_or(|f| f.index != index || f.playing) {
            app.film_at(index, false);
        }
    }
    if let Some((id, to)) = moved {
        app.shell.run("project.moveFrames", json!({ "frames": [id], "to": to }));
    }
}

/// A frame's alignment quality, under its name: the master, rejected, or how well it matched its neighbor, with its
/// terms in the tooltip.
fn quality(ui: &mut egui::Ui, alignment: &Value, min_score: f64) {
    let score = alignment["score"].as_f64().unwrap_or(0.0);
    let (text, warn) = match alignment["status"].as_str() {
        Some("master") => (tr("quality-master"), false),
        Some("rejected") => (tr("quality-rejected"), true),
        _ => (tr_args("quality-score", &[("score", format!("{score:.2}").into())]), score < min_score),
    };
    let color = if warn { ui.visuals().warn_fg_color } else { ui.visuals().weak_text_color() };
    let shift = &alignment["shift"];
    let tip = tr_args(
        "quality-tip",
        &[
            ("score", format!("{score:.3}").into()),
            ("x", format!("{:.1}", shift[0].as_f64().unwrap_or(0.0)).into()),
            ("y", format!("{:.1}", shift[1].as_f64().unwrap_or(0.0)).into()),
            ("scale", format!("{:+.2}", (alignment["scale"].as_f64().unwrap_or(1.0) - 1.0) * 100.0).into()),
            ("rotation", format!("{:+.2}", alignment["rotation"].as_f64().unwrap_or(0.0)).into()),
            ("gain", format!("{:.3}", alignment["gain"].as_f64().unwrap_or(1.0)).into()),
        ],
    );
    #[allow(clippy::cast_possible_truncation)] // A score from 0 to 1, drawn as a bar.
    let bar = egui::ProgressBar::new(score.clamp(0.0, 1.0) as f32).desired_width(60.0).desired_height(4.0);
    ui.horizontal(|ui| {
        ui.colored_label(color, RichText::new(text).small());
        ui.add(bar);
    })
    .response
    .on_hover_text(tip);
}

/// The filmstrip, in place of the viewer: the frame playing, as shot or aligned, fitted, with its number.
fn film_view(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let Some(index) = app.film.as_ref().map(|f| f.index) else { return };
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    // Every frame's picture is asked for at once, so that playing doesn't wait for them.
    for i in 0..app.shell.frames.len() {
        if let Some(path) = app.film_frame(i) {
            app.thumbnail(&path, FILM_SIDE);
        }
    }
    let texture = app.film_frame(index).and_then(|path| app.thumbnail(&path, FILM_SIDE));
    if let Some(texture) = texture {
        let size = texture.size_vec2();
        let scale = (rect.width() / size.x.max(1.0)).min(rect.height() / size.y.max(1.0));
        let at = egui::Rect::from_center_size(rect.center(), size * scale);
        ui.painter().image(texture.id(), at, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
    let name = app.shell.frames.get(index).map(|f| f.name.clone()).unwrap_or_default();
    let how = if app.prefs.film_as_shot { tr("film-as-shot") } else { tr("film-aligned") };
    let caption = tr_args(
        "film-caption",
        &[("index", (index + 1).into()), ("count", app.shell.frames.len().into()), ("name", name.into()), ("how", how.into())],
    );
    let text = RichText::new(caption).color(egui::Color32::WHITE).background_color(egui::Color32::from_black_alpha(150));
    ui.put(egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 8.0), egui::vec2(rect.width() - 16.0, 20.0)), egui::Label::new(text));
    ui.allocate_rect(rect, Sense::hover());
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
    // The preset: a built-in one, or a file.
    let presets = app.built_in_presets();
    let name = |id: &str| presets.iter().find(|p| p["id"] == json!(id)).and_then(|p| p["name"].as_str()).unwrap_or(id).to_owned();
    let shown = app.batch.built_in.as_deref().map_or_else(|| tr("batch-preset-file"), name);
    egui::ComboBox::from_label(tr("batch-preset")).selected_text(shown).show_ui(ui, |ui| {
        for preset in &presets {
            let (Some(id), Some(label)) = (preset["id"].as_str(), preset["name"].as_str()) else { continue };
            let picked = app.batch.built_in.as_deref() == Some(id);
            if ui.selectable_label(picked, label).on_hover_text(preset["description"].as_str().unwrap_or("")).clicked() {
                app.batch.built_in = Some(id.to_owned());
            }
        }
        if ui.selectable_label(app.batch.built_in.is_none(), tr("batch-preset-file")).clicked() {
            app.batch.built_in = None;
        }
    });
    if app.batch.built_in.is_none() {
        folder(ui, &mut app.batch.preset, "batch-preset-path", true);
    }
    // Batches have no "both": they stack by the pyramid then.
    let method = if app.prefs.method == Method::Both { Method::Pyramid } else { app.prefs.method };
    ui.label(tr_args("batch-method", &[("method", tr(method.label_key()).into())]));
    let preset = app.batch.preset();
    let ready = !app.batch.input.is_empty() && !app.batch.output.is_empty() && preset.is_some();
    if ui.add_enabled(ready, egui::Button::new(tr("batch-run"))).clicked() {
        let form = app.batch.clone();
        let method = method.name();
        app.shell.run("batch.run", json!({ "input": form.input, "output": form.output, "preset": preset, "method": method }));
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

/// The dialogs of the presets and the preferences, drawn by their modules: whether it closes. Open, it keeps its state.
fn settings_dialog(app: &mut FloraStackApp, ui: &mut egui::Ui, dialog: Dialog) -> bool {
    let (close, kept) = match dialog {
        Dialog::ApplyPreset(applying) => {
            let (close, applying) = crate::presets::apply_dialog(app, ui, applying);
            (close, Dialog::ApplyPreset(applying))
        }
        Dialog::SavePreset(saving) => {
            let (close, saving) = crate::presets::save_dialog(app, ui, saving);
            (close, Dialog::SavePreset(saving))
        }
        Dialog::Preferences(form) => {
            let (close, form) = crate::preferences::dialog(app, ui, *form);
            (close, Dialog::Preferences(Box::new(form)))
        }
        other => (false, other),
    };
    if !close {
        app.dialog = Some(kept);
    }
    close
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
            dialog @ (Dialog::ApplyPreset(_) | Dialog::SavePreset(_) | Dialog::Preferences(_)) => close = settings_dialog(app, ui, dialog),
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
