//! Presets in the app: the Presets menu (the built-in presets, a preset file, saving the project's settings as one) and
//! its two dialogs, which of a preset's groups to apply and which of the project's to save, so that a preset can be
//! whole or partial (alignment only, output only). Each is one engine command: `preset.list`, `preset.read`,
//! `preset.apply` and `preset.save`; the Batch tab names a preset as `batch.run` takes it ([`batch_preset`]).

use std::path::{Path, PathBuf};

use florastack_engine::{Value, json};

use crate::app::{Dialog, FloraStackApp};
use crate::i18n::{tr, tr_args};

/// The preference groups, as presets name them, with their labels' keys.
pub const GROUPS: [(&str, &str); 4] =
    [("develop", "preset-group-develop"), ("align", "preset-group-align"), ("stack", "preset-group-stack"), ("output", "preset-group-output")];

/// The prefix that names a built-in preset where `batch.run` takes a preset's path.
const BUILT_IN_PREFIX: &str = "builtin:";

/// Where a preset comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A built-in one, by its id.
    BuiltIn(String),
    File(PathBuf),
}

/// A preset about to be applied: where it comes from, its name and description, and the groups it holds, each picked
/// or not.
#[derive(Clone, Debug, PartialEq)]
pub struct Applying {
    pub source: Source,
    pub name: String,
    pub description: String,
    pub groups: Vec<(String, bool)>,
}

/// The project's settings about to be saved as a preset: its name and description, the groups to save, and whether
/// with every setting of them or only those changed from their defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct Saving {
    pub name: String,
    pub description: String,
    pub groups: Vec<(String, bool)>,
    pub complete: bool,
}

/// A group's label.
fn group_label(group: &str) -> String {
    GROUPS.iter().find(|(g, _)| *g == group).map_or_else(|| group.to_owned(), |(_, key)| tr(key))
}

/// The Presets menu.
pub fn menu(app: &mut FloraStackApp, ui: &mut egui::Ui) {
    let project = app.has_project();
    ui.menu_button(tr("menu-presets"), |ui| {
        ui.label(egui::RichText::new(tr("presets-built-in")).weak());
        for preset in app.built_in_presets() {
            let (Some(id), Some(name)) = (preset["id"].as_str(), preset["name"].as_str()) else { continue };
            let response = ui.add_enabled(project, egui::Button::new(name)).on_hover_text(preset["description"].as_str().unwrap_or(""));
            if response.clicked() {
                app.offer_preset(Source::BuiltIn(id.to_owned()));
            }
        }
        ui.separator();
        if ui.add_enabled(project, egui::Button::new(tr("presets-apply-file"))).clicked()
            && let Some(path) = rfd::FileDialog::new().add_filter("TOML", &["toml"]).pick_file()
        {
            app.offer_preset(Source::File(path));
        }
        if ui.add_enabled(project, egui::Button::new(tr("presets-save"))).clicked() {
            app.dialog = Some(Dialog::SavePreset(Saving {
                name: String::new(),
                description: String::new(),
                groups: GROUPS.iter().map(|(g, _)| ((*g).to_owned(), true)).collect(),
                complete: true,
            }));
        }
    });
}

/// The groups' checkboxes.
fn group_boxes(ui: &mut egui::Ui, groups: &mut [(String, bool)]) {
    for (group, on) in groups.iter_mut() {
        ui.checkbox(on, group_label(group));
    }
}

/// The dialog that applies a preset, some of its groups or all: whether it closes, and the state it keeps.
pub fn apply_dialog(app: &mut FloraStackApp, ui: &mut egui::Ui, mut applying: Applying) -> (bool, Applying) {
    let mut close = false;
    ui.heading(tr_args("preset-apply-title", &[("name", applying.name.clone().into())]));
    if !applying.description.is_empty() {
        ui.label(&applying.description);
    }
    ui.add_space(4.0);
    ui.label(tr("preset-apply-groups"));
    group_boxes(ui, &mut applying.groups);
    let any = applying.groups.iter().any(|(_, on)| *on);
    ui.horizontal(|ui| {
        if ui.add_enabled(any, egui::Button::new(tr("preset-apply"))).clicked() {
            close = true;
            app.apply_preset(&applying);
        }
        if ui.button(tr("cancel")).clicked() {
            close = true;
        }
    });
    (close, applying)
}

/// The dialog that saves the project's settings as a preset: whether it closes, and the state it keeps.
pub fn save_dialog(app: &mut FloraStackApp, ui: &mut egui::Ui, mut saving: Saving) -> (bool, Saving) {
    let mut close = false;
    ui.heading(tr("presets-save-title"));
    ui.label(tr("preset-name"));
    ui.text_edit_singleline(&mut saving.name);
    ui.label(tr("preset-description"));
    ui.add(egui::TextEdit::multiline(&mut saving.description).desired_rows(2));
    ui.label(tr("preset-save-groups"));
    group_boxes(ui, &mut saving.groups);
    ui.checkbox(&mut saving.complete, tr("preset-complete")).on_hover_text(tr("tip-preset-complete"));
    let ready = saving.groups.iter().any(|(_, on)| *on) && !saving.name.trim().is_empty();
    ui.horizontal(|ui| {
        if ui.add_enabled(ready, egui::Button::new(tr("preset-save"))).clicked() {
            let file = format!("{}.toml", saving.name.trim());
            let mut dialog = rfd::FileDialog::new().add_filter("TOML", &["toml"]).set_file_name(file);
            if let Some(folder) = &app.prefs.last_folder {
                dialog = dialog.set_directory(folder);
            }
            if let Some(path) = dialog.save_file() {
                close = app.save_preset(&saving, &path);
            }
        }
        if ui.button(tr("cancel")).clicked() {
            close = true;
        }
    });
    (close, saving)
}

/// The preset the Batch tab runs with, as `batch.run` names it: a built-in one's `builtin:<id>`, or a file's path.
pub fn batch_preset(source: &Source) -> String {
    match source {
        Source::BuiltIn(id) => format!("{BUILT_IN_PREFIX}{id}"),
        Source::File(path) => path.display().to_string(),
    }
}

impl FloraStackApp {
    /// The built-in presets (`preset.list`), asked of the engine once.
    pub fn built_in_presets(&mut self) -> Vec<Value> {
        if self.built_in.is_none() {
            let listed = self.shell.run("preset.list", json!({})).unwrap_or(Value::Null);
            self.built_in = Some(listed["presets"].as_array().cloned().unwrap_or_default());
        }
        self.built_in.clone().unwrap_or_default()
    }

    /// Opens the dialog that applies the preset from `source`, with the groups it holds.
    pub fn offer_preset(&mut self, source: Source) {
        let (name, description, groups) = match &source {
            Source::BuiltIn(id) => {
                let Some(preset) = self.built_in_presets().into_iter().find(|p| p["id"] == json!(id)) else { return };
                let groups: Vec<String> = preset["groups"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
                (preset["name"].as_str().unwrap_or(id).to_owned(), preset["description"].as_str().unwrap_or("").to_owned(), groups)
            }
            Source::File(path) => {
                self.remember_folder(path);
                let Some(read) = self.shell.run("preset.read", json!({ "path": path })) else { return };
                let name = read["name"].as_str().map_or_else(|| file_stem(path), str::to_owned);
                let groups = read["preferences"].as_object().map(|p| p.keys().cloned().collect()).unwrap_or_default();
                (name, read["description"].as_str().unwrap_or("").to_owned(), groups)
            }
        };
        if groups.is_empty() {
            self.shell.notify(tr_args("notice-preset-empty", &[("name", name.into())]), true);
            return;
        }
        let groups = GROUPS.iter().filter(|(g, _)| groups.iter().any(|h| h == g)).map(|(g, _)| ((*g).to_owned(), true)).collect();
        self.dialog = Some(Dialog::ApplyPreset(Applying { source, name, description, groups }));
    }

    /// Applies the groups picked of a preset (`preset.apply`).
    pub fn apply_preset(&mut self, applying: &Applying) {
        let groups: Vec<&str> = applying.groups.iter().filter(|(_, on)| *on).map(|(g, _)| g.as_str()).collect();
        let params = match &applying.source {
            Source::BuiltIn(id) => json!({ "builtin": id, "groups": groups }),
            Source::File(path) => json!({ "path": path, "groups": groups }),
        };
        if self.shell.run("preset.apply", params).is_some() {
            self.shell.notify(tr_args("notice-preset-applied", &[("name", applying.name.clone().into())]), false);
        }
    }

    /// Saves the groups picked of the project's settings as a preset at `path` (`preset.save`, replacing the file:
    /// the file dialog asked). Whether it was saved.
    pub fn save_preset(&mut self, saving: &Saving, path: &Path) -> bool {
        self.remember_folder(path);
        let groups: Vec<&str> = saving.groups.iter().filter(|(_, on)| *on).map(|(g, _)| g.as_str()).collect();
        let description = Some(saving.description.trim()).filter(|d| !d.is_empty());
        let params = json!({ "path": path, "name": saving.name.trim(), "description": description, "groups": groups, "complete": saving.complete, "overwrite": true });
        let saved = self.shell.run("preset.save", params).is_some();
        if saved {
            self.shell.notify(tr_args("notice-preset-saved", &[("path", path.display().to_string().into())]), false);
        }
        saved
    }
}

/// A file's name without its extension.
fn file_stem(path: &Path) -> String {
    path.file_stem().map_or_else(|| path.display().to_string(), |s| s.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_names_a_built_in_preset_by_its_id() {
        assert_eq!(batch_preset(&Source::BuiltIn("landscape".to_owned())), "builtin:landscape");
        assert_eq!(batch_preset(&Source::File(PathBuf::from("macro.toml"))), "macro.toml");
    }
}
