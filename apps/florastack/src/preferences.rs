//! The Preferences dialog ([`Form`]): how much memory stacking may use (by default a quarter of the memory installed,
//! with why), the compute device and the GPU's self-test, the cache folder and what it takes, the autosave interval,
//! the theme and the language. The machine's settings go to the engine (`compute.set`, `cache.setFolder`; the GPU's
//! test is `compute.test`) and are remembered in [`crate::prefs::Prefs`], which the app hands to the engine again at
//! startup. FloraStack sends nothing anywhere: no telemetry, no crash reports, no update
//! checks, and the dialog says so.

use std::path::PathBuf;

use florastack_engine::{Device, Value, json};

use crate::app::{Dialog, FloraStackApp};
use crate::i18n::{tr, tr_args};
use crate::prefs::{DEFAULT_AUTOSAVE_MINUTES, Theme};

/// The memory budgets the dialog allows, in MiB.
const MEMORY_RANGE: std::ops::RangeInclusive<u32> = 256..=262_144;

/// The Preferences dialog's state: what it shows and what the user changes, applied by OK.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub memory_mb: u32,
    pub device: Device,
    /// The cache folder; empty for the app's local data folder.
    pub cache_folder: String,
    /// The memory budget, the device and the cache folder when the dialog opened: each changes only when the user
    /// changes it.
    opened: (u32, Device, String),
    pub autosave_minutes: u32,
    pub theme: Theme,
    pub language: String,
    /// What the engine said of its device and memory (`compute.info`) and of its caches (`cache.info`).
    pub compute: Value,
    pub caches: Value,
}

impl Form {
    /// The form for the app's current settings.
    pub fn new(app: &mut FloraStackApp) -> Form {
        let compute = app.shell.run("compute.info", json!({})).unwrap_or(Value::Null);
        let caches = app.shell.run("cache.info", json!({})).unwrap_or(Value::Null);
        let budget = compute["memory"]["budgetMb"].as_u64().and_then(|m| u32::try_from(m).ok()).unwrap_or(1024);
        let device = serde_json::from_value(compute["preference"].clone()).unwrap_or_default();
        let cache_folder = app.prefs.cache_folder.as_ref().map(|f| f.display().to_string()).unwrap_or_default();
        let memory_mb = app.prefs.memory_mb.unwrap_or(budget);
        Form {
            memory_mb,
            device,
            opened: (memory_mb, device, cache_folder.clone()),
            cache_folder,
            autosave_minutes: app.prefs.autosave_minutes.unwrap_or(DEFAULT_AUTOSAVE_MINUTES),
            theme: app.prefs.theme,
            language: app.prefs.language.clone().unwrap_or_else(|| crate::i18n::LANGUAGES[0].0.to_owned()),
            compute,
            caches,
        }
    }

    fn suggested_mb(&self) -> u32 {
        self.compute["memory"]["suggestedMb"].as_u64().and_then(|m| u32::try_from(m).ok()).unwrap_or(1024)
    }
}

/// A heading of the dialog's sections.
fn section(ui: &mut egui::Ui, key: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tr(key)).strong());
}

/// The dialog: whether it closes, and the state it keeps.
pub fn dialog(app: &mut FloraStackApp, ui: &mut egui::Ui, mut form: Form) -> (bool, Form) {
    let mut close = false;
    ui.heading(tr("prefs-title"));
    egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
        memory(ui, &mut form);
        compute(app, ui, &mut form);
        cache(app, ui, &mut form);
        section(ui, "prefs-autosave");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut form.autosave_minutes).range(0..=120).suffix(tr("prefs-minutes")));
            ui.weak(tr("prefs-autosave-help"));
        });
        section(ui, "prefs-appearance");
        ui.horizontal(|ui| {
            ui.label(tr("prefs-theme"));
            ui.radio_value(&mut form.theme, Theme::Dark, tr("prefs-theme-dark"));
            ui.radio_value(&mut form.theme, Theme::Light, tr("prefs-theme-light"));
        });
        ui.horizontal(|ui| {
            ui.label(tr("prefs-language"));
            let shown = crate::i18n::LANGUAGES.iter().find(|(id, _)| *id == form.language).map_or(form.language.as_str(), |(_, name)| name);
            egui::ComboBox::from_id_salt("prefs.language").selected_text(shown).show_ui(ui, |ui| {
                for (id, name) in crate::i18n::LANGUAGES {
                    ui.selectable_value(&mut form.language, id.to_owned(), name);
                }
            });
        });
        section(ui, "prefs-privacy");
        ui.label(tr_args("prefs-no-telemetry", &[("name", florastack_engine::branding::DISPLAY_NAME.into())]));
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button(tr("prefs-ok")).clicked() {
            close = app.apply_preferences(ui.ctx(), &form);
        }
        if ui.button(tr("cancel")).clicked() {
            close = true;
        }
    });
    (close, form)
}

/// The memory budget, with the one suggested for this machine and what it does.
fn memory(ui: &mut egui::Ui, form: &mut Form) {
    section(ui, "prefs-memory");
    let suggested = form.suggested_mb();
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut form.memory_mb).range(MEMORY_RANGE).speed(64.0).suffix(" MiB"));
        if ui.add_enabled(form.memory_mb != suggested, egui::Button::new(tr("prefs-memory-suggested"))).clicked() {
            form.memory_mb = suggested;
        }
    });
    let installed = form.compute["memory"]["installedMb"].as_u64();
    let line = match installed {
        Some(mb) => {
            #[allow(clippy::cast_precision_loss, reason = "a size in gigabytes, shown rounded")]
            let gb = mb as f64 / 1024.0;
            tr_args("prefs-memory-installed", &[("installed", format!("{gb:.0}").into()), ("suggested", suggested.into())])
        }
        None => tr_args("prefs-memory-unknown", &[("suggested", suggested.into())]),
    };
    ui.weak(line);
    ui.label(tr("prefs-memory-help"));
}

/// The compute device and the GPU's self-test, with what it found.
fn compute(app: &mut FloraStackApp, ui: &mut egui::Ui, form: &mut Form) {
    section(ui, "prefs-compute");
    for (device, label, tip) in [
        (Device::Auto, "prefs-device-auto", "tip-device-auto"),
        (Device::Cpu, "prefs-device-cpu", "tip-device-cpu"),
        (Device::Gpu, "prefs-device-gpu", "tip-device-gpu"),
    ] {
        ui.radio_value(&mut form.device, device, tr(label)).on_hover_text(tr(tip));
    }
    let testing = app.shell.running(&["compute.test"]);
    ui.horizontal(|ui| {
        if ui.add_enabled(!testing, egui::Button::new(tr("prefs-test-gpu"))).on_hover_text(tr("tip-test-gpu")).clicked() {
            let device = if form.device == Device::Cpu { Device::Auto } else { form.device };
            app.shell.run("compute.test", json!({ "device": device }));
        }
        if testing {
            ui.spinner();
        }
    });
    let tested = app.shell.tested.as_ref().unwrap_or(&form.compute);
    let result = match tested["device"].as_str() {
        Some("gpu") => tr_args("prefs-gpu-passed", &[("name", tested["adapter"]["name"].as_str().unwrap_or("").to_owned().into())]),
        Some(_) => tr_args("prefs-gpu-unused", &[("reason", tested["reason"].as_str().unwrap_or("").to_owned().into())]),
        None => String::new(),
    };
    ui.weak(result);
}

/// The cache folder, and what the caches take, with a clean-up of what crashed runs left.
fn cache(app: &mut FloraStackApp, ui: &mut egui::Ui, form: &mut Form) {
    section(ui, "prefs-cache");
    ui.horizontal(|ui| {
        let default = crate::app::local_data().map(|d| d.display().to_string()).unwrap_or_default();
        ui.add(egui::TextEdit::singleline(&mut form.cache_folder).hint_text(default).desired_width(300.0));
        if ui.button(tr("browse")).clicked()
            && let Some(folder) = rfd::FileDialog::new().pick_folder()
        {
            form.cache_folder = folder.display().to_string();
        }
    });
    ui.weak(tr("prefs-cache-help"));
    let size = |key: &str| form.caches[key]["bytes"].as_u64().map(crate::export_tab::size_text);
    let rows = [("prefs-cache-projects", size("temporaryProjects")), ("prefs-cache-tiles", size("tiles")), ("prefs-cache-project", size("project"))];
    egui::Grid::new("prefs.caches").show(ui, |ui| {
        for (label, size) in rows {
            if let Some(size) = size {
                ui.label(tr(label));
                ui.label(size);
                ui.end_row();
            }
        }
    });
    if ui.button(tr("prefs-cache-clean")).on_hover_text(tr("tip-cache-clean")).clicked() {
        app.shell.run("cache.removeAbandoned", json!({}));
        form.caches = app.shell.run("cache.info", json!({})).unwrap_or(Value::Null);
    }
}

impl FloraStackApp {
    /// Opens the Preferences dialog.
    pub fn open_preferences(&mut self) {
        self.shell.tested = None;
        let form = Form::new(self);
        self.dialog = Some(Dialog::Preferences(Box::new(form)));
    }

    /// Applies the dialog's settings and remembers them: the device and the memory budget when they were changed
    /// (`compute.set`), the cache folder when it was changed (`cache.setFolder`; the app's local data folder when it
    /// was emptied), autosave, the theme and the language. Whether the engine took them all: the dialog stays open
    /// otherwise, with its message.
    pub fn apply_preferences(&mut self, ctx: &egui::Context, form: &Form) -> bool {
        let (memory_mb, device, opened_folder) = &form.opened;
        let mut applied = true;
        if (form.memory_mb, form.device) != (*memory_mb, *device) {
            applied = self.shell.run("compute.set", json!({ "device": form.device, "memoryMb": form.memory_mb })).is_some();
            self.shell.forget_compute();
            if applied {
                self.prefs.memory_mb = (form.memory_mb != form.suggested_mb()).then_some(form.memory_mb);
                self.prefs.device = (form.device != Device::Auto).then_some(form.device);
            }
        }
        let folder = form.cache_folder.trim();
        if folder != opened_folder.trim() {
            let target = if folder.is_empty() { crate::app::local_data() } else { Some(PathBuf::from(folder)) };
            if let Some(target) = target
                && self.set_cache_folder(&target)
            {
                self.prefs.cache_folder = (!folder.is_empty()).then(|| PathBuf::from(folder));
            } else {
                applied = false;
            }
        }
        self.prefs.autosave_minutes = (form.autosave_minutes != DEFAULT_AUTOSAVE_MINUTES).then_some(form.autosave_minutes);
        self.set_autosave();
        self.prefs.language = Some(form.language.clone()).filter(|l| l != crate::i18n::LANGUAGES[0].0);
        if self.prefs.theme != form.theme {
            self.prefs.theme = form.theme;
            crate::app::apply_theme(ctx, form.theme);
        }
        applied
    }
}
