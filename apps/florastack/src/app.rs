//! The app ([`FloraStackApp`], an eframe app): its state, what each action does, and the frame loop. It shows what
//! the engine holds and dispatches what the user asks for; the panels are drawn by `panels.rs`.
//!
//! At startup it removes the tile folders crashed runs left (`cache.removeAbandoned`) and offers to move old
//! temporary projects to the trash (`project.findOrphans`, `project.trashOrphans`). When a project opens it checks
//! its frames' files (`project.checkFrames`). It saves on its own a while after the first unsaved change
//! ([`crate::files::Autosave`]), and remembers the recent projects, the theme and the tab ([`crate::prefs`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use florastack_engine::{Compute, EngineConfig, Format, Value, branding, json};

use crate::actions::Action;
use crate::files::{Autosave, Dropped, paths_from_text};
use crate::i18n::{tr, tr_args};
use crate::loader::{Loader, Request};
use crate::palette::{Palette, Picked};
use crate::panels::BatchForm;
use crate::prefs::{Method, Prefs, Tab, Theme};
use crate::shell::Shell;
use crate::viewer::{GpuViewer, Viewer};

/// The longest side of a frame list thumbnail, in pixels.
pub const THUMBNAIL_SIDE: usize = 96;

/// How the app starts: the engine's settings, and files to open or add.
pub struct Options {
    pub engine: EngineConfig,
    /// Files given on the command line or dropped on the executable.
    pub files: Vec<PathBuf>,
}

impl Options {
    /// The desktop app's defaults: temporary projects in the app's local data folder ([`local_data`]), the GPU when
    /// it passes its self-test.
    pub fn standard(files: Vec<PathBuf>) -> Options {
        let data = local_data().unwrap_or_else(|| std::env::temp_dir().join(branding::APP_ID));
        let engine = EngineConfig { temporary_projects: Some(data.join("projects")), compute: Compute::default(), ..EngineConfig::default() };
        Options { engine, files }
    }
}

/// The app's folder for data that stays on this machine: temporary projects, whose caches can be large. Not the
/// system's temporary folder, which cleaners may empty, and on Windows not the roaming profile (eframe's storage
/// folder, where the window layout goes), which follows the user from machine to machine.
pub fn local_data() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|l| !l.is_empty()) {
        return Some(PathBuf::from(local).join(branding::APP_ID));
    }
    eframe::storage_dir(branding::APP_ID)
}

/// What the viewer shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Nothing,
    Frame(u64),
    Output(u64),
    Preview,
}

/// A dialog open over the window.
#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    /// The project has unsaved changes: save, discard or cancel before `then`.
    Unsaved {
        then: Then,
    },
    SaveAs {
        name: String,
        folder: String,
    },
    Shortcuts,
    About,
    /// The stacks `project.findStacks` found in a folder, to pick one to add.
    Stacks {
        stacks: Vec<Vec<PathBuf>>,
    },
}

/// What waits for unsaved changes to be dealt with.
#[derive(Clone, Debug, PartialEq)]
pub enum Then {
    New,
    Open(PathBuf),
    Close,
    Quit,
}

/// A notice shown until dismissed: old temporary projects, or frames missing or changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Banner {
    Orphans { paths: Vec<String>, bytes: u64 },
    Frames { missing: usize, changed: usize },
}

/// See the module documentation.
pub struct FloraStackApp {
    pub shell: Shell,
    pub prefs: Prefs,
    pub selection: Selection,
    pub viewer: Viewer,
    pub palette: Palette,
    pub dialog: Option<Dialog>,
    pub banners: Vec<Banner>,
    /// The thumbnails read, by file.
    pub thumbnails: std::collections::HashMap<PathBuf, egui::TextureHandle>,
    thumbnail_requests: std::collections::HashSet<PathBuf>,
    view_loader: Loader,
    thumb_loader: Loader,
    /// What the viewer loaded or is loading, and which selection it is for.
    pub loading: Option<(PathBuf, Selection)>,
    autosave: Autosave,
    /// The project the frames were checked for.
    checked: Option<PathBuf>,
    /// Whether the app may quit: unsaved changes were dealt with.
    quitting: bool,
    /// The project path last seen, to notice openings.
    last_project: Option<PathBuf>,
    /// The stacking settings (`stack.get`), with the shell's revision they were read at.
    pub settings: Option<(u64, Value)>,
    /// The format the Export tab writes.
    pub export_format: Format,
    pub batch: BatchForm,
    /// Whether the viewer switches to the next preview when it comes in: one was asked for since the user last
    /// picked something else.
    pub follow_preview: bool,
    /// The preview file shown last.
    shown_preview: Option<PathBuf>,
    /// The folder of temporary projects, which the recent projects leave out.
    temporary: Option<PathBuf>,
}

impl FloraStackApp {
    /// The app, in eframe's creation context.
    pub fn new(cc: &eframe::CreationContext<'_>, options: Options) -> FloraStackApp {
        let ctx = cc.egui_ctx.clone();
        let gpu = cc.wgpu_render_state.as_ref().and_then(GpuViewer::install);
        let prefs = Prefs::load(cc.storage);
        FloraStackApp::with_context(&ctx, options, prefs, gpu)
    }

    /// The app on `ctx`, with `prefs`, and the GPU viewer's largest side when it has one: what tests build.
    pub fn with_context(ctx: &egui::Context, options: Options, mut prefs: Prefs, gpu: Option<u32>) -> FloraStackApp {
        prefs.forget_missing();
        apply_theme(ctx, prefs.theme);
        let temporary = options.engine.temporary_projects.clone();
        let waker_ctx = ctx.clone();
        let waker: florastack_engine::Waker = Arc::new(move || waker_ctx.request_repaint());
        let mut app = FloraStackApp {
            shell: Shell::new(options.engine, Some(waker)),
            prefs,
            selection: Selection::Nothing,
            viewer: Viewer::new(gpu),
            palette: Palette::default(),
            dialog: None,
            banners: Vec::new(),
            thumbnails: std::collections::HashMap::new(),
            thumbnail_requests: std::collections::HashSet::new(),
            view_loader: Loader::latest_only("viewer", ctx),
            thumb_loader: Loader::every("thumbnails", ctx),
            loading: None,
            autosave: Autosave::new(Autosave::DEFAULT_INTERVAL),
            checked: None,
            quitting: false,
            last_project: None,
            settings: None,
            export_format: Format::Tiff,
            batch: BatchForm::default(),
            follow_preview: false,
            shown_preview: None,
            temporary,
        };
        app.startup();
        if !options.files.is_empty() {
            app.dropped(options.files);
        }
        app
    }

    /// The startup cleanup: abandoned tile folders, and old temporary projects offered for the trash.
    fn startup(&mut self) {
        let _ = self.shell.run("cache.removeAbandoned", json!({}));
        if let Some(found) = self.shell.run("project.findOrphans", json!({})) {
            let orphans = found["orphans"].as_array().cloned().unwrap_or_default();
            if !orphans.is_empty() {
                let bytes = orphans.iter().filter_map(|o| o["bytes"].as_u64()).sum();
                let paths = orphans.iter().filter_map(|o| o["path"].as_str().map(str::to_owned)).collect();
                self.banners.push(Banner::Orphans { paths, bytes });
            }
        }
    }

    /// The open project's name, or "Untitled".
    pub fn project_title(&self) -> Option<String> {
        let project = self.shell.engine().project()?;
        Some(project.name().map_or_else(|| tr("untitled"), str::to_owned))
    }

    pub fn has_project(&self) -> bool {
        self.shell.engine().project().is_some()
    }

    /// Runs `action`.
    pub fn perform(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::New => self.after_unsaved(Then::New),
            Action::Open => {
                if let Some(path) = self.dialog_folder().pick_folder() {
                    self.after_unsaved(Then::Open(path));
                }
            }
            Action::AddFrames => {
                if let Some(paths) = self.dialog_folder().pick_files() {
                    self.add_frames(&paths);
                }
            }
            Action::ImportStacks => {
                if let Some(folder) = self.dialog_folder().pick_folder() {
                    self.remember_folder(&folder);
                    self.shell.run("project.findStacks", json!({ "paths": [folder] }));
                }
            }
            Action::Save => self.save(),
            Action::SaveAs => {
                if self.has_project() {
                    let name = self.shell.engine().project().and_then(|p| p.name().map(str::to_owned)).unwrap_or_default();
                    let folder = self.prefs.last_folder.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    self.dialog = Some(Dialog::SaveAs { name, folder });
                }
            }
            Action::Close => self.after_unsaved(Then::Close),
            Action::Quit => self.after_unsaved(Then::Quit),
            Action::Undo => {
                self.shell.run("edit.undo", json!({}));
            }
            Action::Redo => {
                self.shell.run("edit.redo", json!({}));
            }
            Action::Palette => self.palette.open(),
            Action::Align => {
                self.shell.run("align.run", json!({}));
            }
            Action::Preview => {
                let method = self.prefs.method.name();
                self.shell.run("stack.preview", json!({ "method": method }));
                self.follow_preview = true;
            }
            Action::Stack => {
                self.shell.run(self.prefs.method.command(), json!({}));
            }
            Action::CancelJobs => {
                let running: Vec<u64> = self.shell.jobs.values().filter(|j| !j.state.is_finished()).map(|j| j.id.0).collect();
                for job in running {
                    self.shell.run("job.cancel", json!({ "job": job }));
                }
            }
            Action::ZoomFit => self.viewer.zoom_fit(),
            Action::Zoom100 => self.viewer.zoom_to(1.0),
            Action::Zoom200 => self.viewer.zoom_to(2.0),
            Action::ZoomIn => self.viewer.zoom_by(2f32.sqrt()),
            Action::ZoomOut => self.viewer.zoom_by(1.0 / 2f32.sqrt()),
            Action::NextFrame => self.step_frame(1),
            Action::PreviousFrame => self.step_frame(-1),
            Action::RemoveFrame => {
                if let Selection::Frame(id) = self.selection {
                    self.shell.run("project.removeFrames", json!({ "frames": [id] }));
                    self.select(Selection::Nothing);
                }
            }
            Action::ToggleTheme => {
                self.prefs.theme = if self.prefs.theme == Theme::Dark { Theme::Light } else { Theme::Dark };
                apply_theme(ctx, self.prefs.theme);
            }
            Action::Shortcuts => self.dialog = Some(Dialog::Shortcuts),
        }
    }

    /// A file dialog starting in the folder last used.
    fn dialog_folder(&self) -> rfd::FileDialog {
        let dialog = rfd::FileDialog::new();
        match &self.prefs.last_folder {
            Some(folder) => dialog.set_directory(folder),
            None => dialog,
        }
    }

    fn remember_folder(&mut self, path: &std::path::Path) {
        let folder = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        self.prefs.last_folder = Some(folder.to_path_buf());
    }

    /// Asks about unsaved changes first, if there are any, then does `then`.
    fn after_unsaved(&mut self, then: Then) {
        if self.shell.engine().is_dirty() {
            self.dialog = Some(Dialog::Unsaved { then });
        } else {
            self.then(then, false);
        }
    }

    /// Does `then`, throwing unsaved changes away when `discard`.
    pub fn then(&mut self, then: Then, discard: bool) {
        match then {
            Then::New => {
                self.shell.run("project.new", json!({ "discardChanges": discard }));
            }
            Then::Open(path) => {
                self.remember_folder(&path);
                self.shell.run("project.open", json!({ "path": path, "discardChanges": discard }));
            }
            Then::Close => {
                self.shell.run("project.close", json!({ "discardChanges": discard }));
            }
            Then::Quit => self.quitting = true,
        }
        self.select(Selection::Nothing);
    }

    /// Saves the project.
    pub fn save(&mut self) {
        if self.has_project() {
            self.shell.run("project.save", json!({}));
        }
    }

    /// Names the project and saves it in `folder` (Save As).
    pub fn save_as(&mut self, name: &str, folder: &str) {
        let current = self.shell.engine().project().and_then(|p| p.name().map(str::to_owned));
        if current.as_deref() != Some(name) && self.shell.run("project.rename", json!({ "name": name })).is_none() {
            return;
        }
        if let Some(saved) = self.shell.run("project.saveAs", json!({ "folder": folder })) {
            self.remember_folder(std::path::Path::new(folder));
            if let Some(path) = saved["path"].as_str() {
                self.shell.notify(tr_args("notice-saved-as", &[("path", path.into())]), false);
            }
        }
    }

    /// Adds files and folders as frames, in a new temporary project when none is open.
    pub fn add_frames(&mut self, paths: &[PathBuf]) {
        if let Some(first) = paths.first() {
            self.remember_folder(first);
        }
        if !self.has_project() && self.shell.run("project.new", json!({})).is_none() {
            return;
        }
        self.shell.run("project.addFrames", json!({ "paths": paths }));
    }

    /// Files dropped on the window, given on the command line or pasted.
    pub fn dropped(&mut self, paths: Vec<PathBuf>) {
        match Dropped::of(paths) {
            Some(Dropped::Project(path)) => self.after_unsaved(Then::Open(path)),
            Some(Dropped::Frames(paths)) => self.add_frames(&paths),
            None => {}
        }
    }

    /// Selects what the viewer shows, and loads it.
    pub fn select(&mut self, selection: Selection) {
        if selection != Selection::Preview {
            self.follow_preview = false;
        }
        let path = match &selection {
            Selection::Frame(id) => self.shell.frames.iter().find(|f| f.id == *id).map(|f| f.file.clone()),
            Selection::Output(id) => self.shell.outputs.iter().find(|o| o.id == *id).map(|o| o.image.clone()),
            Selection::Preview => self.shell.preview.clone(),
            Selection::Nothing => None,
        };
        self.selection = selection.clone();
        if let Some(path) = path {
            self.view_loader.request(Request { path: path.clone(), max_side: Some(self.viewer.max_side()) });
            self.loading = Some((path, selection));
        } else {
            self.viewer.clear();
            self.loading = None;
        }
    }

    /// Selects the frame `step` places after (or before) the one selected.
    fn step_frame(&mut self, step: isize) {
        let frames = &self.shell.frames;
        if frames.is_empty() {
            return;
        }
        let at = match self.selection {
            Selection::Frame(id) => frames.iter().position(|f| f.id == id),
            _ => None,
        };
        let next = match at {
            Some(i) => i.saturating_add_signed(step).min(frames.len() - 1),
            None => 0,
        };
        let id = frames[next].id;
        self.select(Selection::Frame(id));
    }

    /// Wants a thumbnail of `path` read.
    pub fn want_thumbnail(&mut self, path: &std::path::Path) {
        if !self.thumbnails.contains_key(path) && self.thumbnail_requests.insert(path.to_path_buf()) {
            self.thumb_loader.request(Request { path: path.to_path_buf(), max_side: Some(THUMBNAIL_SIDE) });
        }
    }

    /// The engine's news, loaded images, autosave, and what follows a project's opening.
    fn tick(&mut self, ctx: &egui::Context) {
        if self.shell.poll() {
            ctx.request_repaint();
        }
        for opened in std::mem::take(&mut self.shell.opened) {
            if self.temporary.as_ref().is_none_or(|t| !opened.starts_with(t)) {
                self.prefs.remember(&opened);
            }
        }
        let project = self.shell.engine().project_path().map(std::path::Path::to_path_buf);
        if project != self.last_project {
            self.last_project.clone_from(&project);
            self.select(Selection::Nothing);
            self.banners.retain(|b| !matches!(b, Banner::Frames { .. }));
        }
        if project.is_some() && self.checked != project {
            self.checked.clone_from(&project);
            if let Some(checked) = self.shell.run("project.checkFrames", json!({})) {
                let states: Vec<&str> = checked["frames"].as_array().into_iter().flatten().filter_map(|f| f["state"].as_str()).collect();
                let missing = states.iter().filter(|s| **s == "missing").count();
                if !states.is_empty() {
                    self.banners.push(Banner::Frames { missing, changed: states.len() - missing });
                }
            }
        }
        self.found_stacks();
        if let Some(output) = self.shell.made.take() {
            self.select(Selection::Output(output));
        }
        // With nothing picked, the viewer shows the first frame.
        if self.selection == Selection::Nothing
            && let Some(first) = self.shell.frames.first().map(|f| f.id)
        {
            self.select(Selection::Frame(first));
        }
        for done in self.view_loader.done() {
            if self.loading.as_ref().is_some_and(|(path, _)| *path == done.request.path) {
                match done.image {
                    Ok(image) => self.viewer.show(image),
                    Err(e) => self.shell.notify(e, true),
                }
                self.loading = None;
            }
        }
        for done in self.thumb_loader.done() {
            if let Ok(image) = done.image {
                let pixels = egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.to_srgb8());
                let texture = ctx.load_texture(done.request.path.display().to_string(), pixels, egui::TextureOptions::LINEAR);
                self.thumbnails.insert(done.request.path.clone(), texture);
            }
        }
        // A preview that just came in replaces the one shown, or takes the viewer when one was asked for.
        if self.shell.preview.is_some() && self.shell.preview != self.shown_preview {
            self.shown_preview.clone_from(&self.shell.preview);
            if self.follow_preview || self.selection == Selection::Preview {
                self.follow_preview = false;
                self.select(Selection::Preview);
            }
        }
        let writable = self.shell.engine().project().is_some();
        if writable && self.autosave.due(Instant::now(), self.shell.engine().is_dirty(), self.shell.busy()) {
            self.shell.run("project.save", json!({}));
        }
        if self.shell.busy() || self.loading.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// The stacks a `project.findStacks` job found, offered for picking.
    fn found_stacks(&mut self) {
        if let Some(result) = self.shell.stacks_found.take() {
            let stacks: Vec<Vec<PathBuf>> = result["stacks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| s["files"].as_array().into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from).collect())
                .filter(|files: &Vec<PathBuf>| !files.is_empty())
                .collect();
            match stacks.len() {
                0 => self.shell.notify(tr("notice-no-stacks"), true),
                1 => {
                    let files = stacks.into_iter().next().unwrap_or_default();
                    self.add_frames(&files);
                }
                _ => self.dialog = Some(Dialog::Stacks { stacks }),
            }
        }
    }

    /// Handles the shortcuts, the files dropped and the text pasted.
    fn input(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        if !self.palette.is_open() && self.dialog.is_none() {
            for (action, shortcut) in Action::by_shortcut() {
                let plain = shortcut.modifiers.is_none();
                if plain && typing {
                    continue;
                }
                if ctx.input_mut(|i| i.consume_shortcut(&shortcut)) {
                    self.perform(ctx, action);
                }
            }
        }
        let (dropped, pasted) = ctx.input(|i| {
            let dropped: Vec<PathBuf> = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect();
            let pasted: Vec<String> =
                i.events.iter().filter_map(|e| if let egui::Event::Paste(text) = e { Some(text.clone()) } else { None }).collect();
            (dropped, pasted)
        });
        if !dropped.is_empty() {
            self.dropped(dropped);
        }
        if !typing {
            for text in pasted {
                let paths = paths_from_text(&text);
                if !paths.is_empty() {
                    self.dropped(paths);
                }
            }
        }
    }

    /// Runs what the palette picked.
    fn picked(&mut self, ctx: &egui::Context, picked: Picked) {
        match picked {
            Picked::Action(action) => self.perform(ctx, action),
            Picked::Command { name, params } => {
                if let Some(result) = self.shell.run(&name, params) {
                    tracing::debug!("{name}: {result}");
                }
            }
        }
    }

    /// Sets the stacking method, as the Stack tab picks it.
    pub fn set_method(&mut self, method: Method) {
        self.prefs.method = method;
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.prefs.tab = tab;
    }
}

/// Applies `theme` to the UI.
fn apply_theme(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::Dark => egui::Theme::Dark,
        Theme::Light => egui::Theme::Light,
    });
}

impl eframe::App for FloraStackApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            if self.shell.engine().is_dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dialog = Some(Dialog::Unsaved { then: Then::Quit });
            } else {
                self.quitting = true;
            }
        }
        if self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.input(&ctx);
        crate::panels::show(self, ui);
        let registry = self.shell.engine().registry();
        if let Some(picked) = self.palette.ui(&ctx, registry) {
            self.picked(&ctx, picked);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.prefs.save(storage);
    }
}
