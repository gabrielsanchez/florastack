//! Projects: creating, opening, saving and closing them, their name, and the cleanup of old temporary projects.

use std::path::Path;

use florastack_project::{Edit, Orphan, Orphans, ProjectFolder, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{absolute, check_len, check_name, check_path, display, reason};
use crate::error::{CommandError, Invalid};
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params, Requires};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::instant(
            Meta::new("project.new", "New project", "Creates a project folder and opens the new project, closing the project that is open.")
                .help(
                    "With a `name`, the project folder is `<name>.florastack` in `folder`; without one, the project is \
                     temporary (unnamed) and goes in the temporary-projects folder, where orphan cleanup offers to \
                     move it to the trash once it is old and still unnamed. The project that is open is closed first, \
                     which is refused while it has unsaved changes, unless `discardChanges`.",
                )
                .effects(Effects::Project)
                .examples(&[r#"{ "name": "Bee head", "folder": "C:/Stacks" }"#, "{}"]),
            new,
        ),
        CommandSpec::instant(
            Meta::new("project.open", "Open project", "Opens a project folder, closing the project that is open.")
                .help(
                    "`path` is the `.florastack` folder or its `project.json`. A project from an older version is \
                     upgraded in memory and written in the current format at the next save. With `readOnly`, the \
                     project isn't locked against other editors and can't be changed. Cache files that nothing in \
                     the project refers to any more are removed.",
                )
                .effects(Effects::Project)
                .examples(&[r#"{ "path": "C:/Stacks/Bee head.florastack" }"#, r#"{ "path": "C:/Stacks/Bee head.florastack", "readOnly": true }"#]),
            open,
        ),
        CommandSpec::instant(
            Meta::new("project.save", "Save project", "Saves the open project to its project.json.")
                .help("Saving is atomic: a failed save (a full disk) leaves the last good save as it was. The undo history stays.")
                .requires(Requires::WritableProject)
                .effects(Effects::Project),
            save,
        ),
        CommandSpec::instant(
            Meta::new("project.close", "Close project", "Closes the open project.")
                .help(
                    "Refused while the project has unsaved changes, unless `discardChanges`. Its running jobs are \
                     cancelled and waited for; then the cache files that nothing refers to are removed, the undo \
                     history is dropped and the project folder's lock is released.",
                )
                .effects(Effects::Project)
                .examples(&["{}", r#"{ "discardChanges": true }"#]),
            close,
        ),
        CommandSpec::query(
            Meta::new("project.info", "Project information", "Describes the open project: its name, folder, frames, outputs and undo state.")
                .requires(Requires::Project),
            info,
        ),
        CommandSpec::instant(
            Meta::new("project.rename", "Rename project", "Gives the open project a name, which makes a temporary project permanent.")
                .help("The project folder keeps its name; only the name in project.json changes.")
                .undoable()
                .examples(&[r#"{ "name": "Bee head, 2026" }"#]),
            rename,
        ),
        CommandSpec::query(
            Meta::new("project.findOrphans", "Find old temporary projects", "Lists the temporary projects old enough to be moved to the trash.")
                .help(
                    "Orphans are unnamed projects in the temporary-projects folder that no FloraStack has open and that \
                     haven't been saved or opened for `days` days (30 by default).",
                )
                .examples(&["{}", r#"{ "days": 7 }"#]),
            find_orphans,
        ),
        CommandSpec::instant(
            Meta::new("project.trashOrphans", "Move old temporary projects to the trash", "Moves orphaned temporary projects to the trash.")
                .help(
                    "Each path is checked again before it is moved: it must still be an orphan in the temporary-projects \
                     folder. Nothing is deleted; the projects can be restored from the trash.",
                )
                .effects(Effects::Files)
                .examples(&[r#"{ "paths": ["C:/Users/me/AppData/Local/FloraStack/projects/untitled-20260901120000-0.florastack"] }"#]),
            trash_orphans,
        ),
    ]
}

/// Creates a project and opens it.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NewParams {
    /// The project's name; its folder is `<name>.florastack` in `folder`. Leave it out for a temporary project.
    #[serde(default)]
    name: Option<String>,
    /// The folder to make the project folder in. Needed with `name`, and only with it.
    #[serde(default)]
    folder: Option<String>,
    /// Close the open project even if it has unsaved changes, which are lost.
    #[serde(default)]
    discard_changes: bool,
}

impl Params for NewParams {
    fn check(&self) -> Result<(), Invalid> {
        if let Some(name) = &self.name {
            check_name("name", name)?;
        }
        if let Some(folder) = &self.folder {
            check_path("folder", folder)?;
        }
        if self.name.is_some() != self.folder.is_some() {
            return Err(Invalid::new("folder", "goes with a name: give both, or neither for a temporary project"));
        }
        Ok(())
    }
}

/// A project just opened.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Opened {
    /// The project folder.
    path: String,
    name: Option<String>,
    temporary: bool,
    read_only: bool,
    /// The project format version the file was upgraded from, if it was older.
    #[serde(skip_serializing_if = "Option::is_none")]
    migrated_from: Option<u32>,
    /// How many cache files that nothing referred to were removed.
    cleaned: usize,
}

fn opened(session: &mut Session, folder: ProjectFolder, project: florastack_project::Project) -> Opened {
    let path = display(folder.path());
    let (name, temporary) = (project.name().map(str::to_owned), project.is_temporary());
    let (read_only, migrated_from) = (folder.is_read_only(), folder.migrated_from());
    let cleaned = session.open_project(folder, project);
    Opened { path, name, temporary, read_only, migrated_from, cleaned }
}

/// Refuses to go on when the open project has unsaved changes, unless they may be discarded.
fn check_closable(session: &Session, discard: bool) -> Result<(), CommandError> {
    if session.is_dirty() && !discard {
        return Err(session
            .unavailable("the open project has unsaved changes: save it first with `project.save`, or pass `discardChanges` to throw them away"));
    }
    Ok(())
}

fn new(session: &mut Session, p: NewParams) -> Result<Opened, CommandError> {
    check_closable(session, p.discard_changes)?;
    let (folder, project) = if let (Some(name), Some(folder)) = (&p.name, &p.folder) {
        ProjectFolder::create(&absolute(folder)?, name)?
    } else {
        let root = session
            .temporary_projects()
            .ok_or_else(|| session.unavailable("there is no folder for temporary projects here: give the project a `name` and a `folder`"))?;
        ProjectFolder::create_temporary(root)?
    };
    session.close_project(true)?;
    Ok(opened(session, folder, project))
}

/// Opens a project folder.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OpenParams {
    /// The `.florastack` project folder, or its `project.json`.
    path: String,
    /// Open it for reading only: not locked against other editors, and it can't be changed or saved.
    #[serde(default)]
    read_only: bool,
    /// Close the open project even if it has unsaved changes, which are lost.
    #[serde(default)]
    discard_changes: bool,
}

impl Params for OpenParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("path", &self.path)
    }
}

fn same_folder(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b || a.parent() == Some(b.as_path()),
        _ => false,
    }
}

fn open(session: &mut Session, p: OpenParams) -> Result<Opened, CommandError> {
    check_closable(session, p.discard_changes)?;
    let path = absolute(&p.path)?;
    // The project that is open holds its folder's lock: opening it again means closing it first.
    if session.folder().is_ok_and(|f| same_folder(&path, f.path())) {
        session.close_project(true)?;
    }
    let (folder, project) = if p.read_only { ProjectFolder::open_read_only(&path)? } else { ProjectFolder::open(&path)? };
    session.close_project(true)?;
    Ok(opened(session, folder, project))
}

/// The project saved.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Saved {
    path: String,
    /// When, in RFC 3339.
    saved_at: String,
}

fn save(session: &mut Session, _: NoParams) -> Result<Saved, CommandError> {
    let path = session.save()?;
    let saved_at = session.project()?.modified().to_string();
    Ok(Saved { path: display(&path), saved_at })
}

/// Closes the open project.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CloseParams {
    /// Close it even if it has unsaved changes, which are lost.
    #[serde(default)]
    discard_changes: bool,
}

impl Params for CloseParams {}

/// The project closed.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Closed {
    /// The folder of the project closed; none when no project was open.
    path: Option<String>,
    /// How many cache files that nothing referred to were removed.
    cleaned: usize,
}

fn close(session: &mut Session, p: CloseParams) -> Result<Closed, CommandError> {
    Ok(match session.close_project(p.discard_changes)? {
        Some((path, cleaned)) => Closed { path: Some(display(&path)), cleaned },
        None => Closed { path: None, cleaned: 0 },
    })
}

/// The open project, described.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Info {
    path: String,
    /// None for a temporary project.
    name: Option<String>,
    temporary: bool,
    read_only: bool,
    /// Whether it has unsaved changes.
    dirty: bool,
    frames: usize,
    enabled_frames: usize,
    outputs: usize,
    /// What an undo would undo now.
    undo: Option<String>,
    /// What a redo would redo now.
    redo: Option<String>,
    /// RFC 3339.
    created: String,
    /// When it was last saved, RFC 3339.
    modified: String,
    /// The software that last saved it.
    software: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migrated_from: Option<u32>,
}

fn info(session: &Session, _: NoParams) -> Result<Info, CommandError> {
    let (folder, project) = (session.folder()?, session.project()?);
    Ok(Info {
        path: display(folder.path()),
        name: project.name().map(str::to_owned),
        temporary: project.is_temporary(),
        read_only: folder.is_read_only(),
        dirty: session.is_dirty(),
        frames: project.frames.len(),
        enabled_frames: project.frames.iter().filter(|f| f.enabled).count(),
        outputs: project.outputs.len(),
        undo: session.undo_label().map(str::to_owned),
        redo: session.redo_label().map(str::to_owned),
        created: project.created().to_string(),
        modified: project.modified().to_string(),
        software: project.software().map(str::to_owned),
        migrated_from: folder.migrated_from(),
    })
}

/// Names the open project.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenameParams {
    /// The new name: at most 200 characters, no control characters.
    name: String,
}

impl Params for RenameParams {
    fn check(&self) -> Result<(), Invalid> {
        check_name("name", &self.name)
    }
}

/// The project's new name.
#[derive(Debug, Serialize, JsonSchema)]
struct Renamed {
    name: String,
}

fn rename(session: &mut Session, p: RenameParams) -> Result<Renamed, CommandError> {
    if session.project()?.name() != Some(p.name.as_str()) {
        session.apply(Edit::SetName(Some(p.name.clone())))?;
    }
    Ok(Renamed { name: p.name })
}

/// Lists the orphaned temporary projects.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FindOrphansParams {
    /// How many days unused a temporary project must be (30 by default), at most 36500.
    #[serde(default)]
    #[schemars(range(max = 36500))]
    days: Option<u32>,
}

impl Params for FindOrphansParams {
    fn check(&self) -> Result<(), Invalid> {
        match self.days {
            Some(d) if d > 36_500 => Err(Invalid::new("days", "is more than 36500")),
            _ => Ok(()),
        }
    }
}

/// An orphaned temporary project.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct OrphanInfo {
    path: String,
    /// When it was last saved or opened, RFC 3339.
    last_used: String,
    /// The size of its files.
    bytes: u64,
}

/// The orphans found.
#[derive(Debug, Serialize, JsonSchema)]
struct OrphanList {
    /// The temporary-projects folder looked in.
    folder: String,
    orphans: Vec<OrphanInfo>,
}

fn orphans_in(session: &Session, days: Option<u32>) -> Result<Orphans, CommandError> {
    let root = session.temporary_projects().ok_or_else(|| session.unavailable("there is no folder for temporary projects here"))?;
    Ok(Orphans::new(root, days.unwrap_or(Orphans::DEFAULT_DAYS)))
}

fn find_orphans(session: &Session, p: FindOrphansParams) -> Result<OrphanList, CommandError> {
    let orphans = orphans_in(session, p.days)?;
    let found = orphans.find()?;
    Ok(OrphanList {
        folder: display(orphans.root()),
        orphans: found
            .into_iter()
            .map(|o| OrphanInfo { path: display(&o.path), last_used: Timestamp::from_system_time(o.last_used).to_string(), bytes: o.bytes })
            .collect(),
    })
}

/// Moves orphaned temporary projects to the trash.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TrashOrphansParams {
    /// The project folders, as `project.findOrphans` lists them.
    paths: Vec<String>,
    /// How many days unused each must still be (30 by default).
    #[serde(default)]
    #[schemars(range(max = 36500))]
    days: Option<u32>,
}

impl Params for TrashOrphansParams {
    fn check(&self) -> Result<(), Invalid> {
        check_len("paths", &self.paths, 1)?;
        self.paths.iter().try_for_each(|p| check_path("paths", p))?;
        FindOrphansParams { days: self.days }.check()
    }
}

/// A path that wasn't moved, and why.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct NotDone {
    pub(crate) path: String,
    pub(crate) reason: String,
}

/// What was moved to the trash.
#[derive(Debug, Serialize, JsonSchema)]
struct Trashed {
    trashed: Vec<String>,
    failed: Vec<NotDone>,
}

fn trash_orphans(session: &mut Session, p: TrashOrphansParams) -> Result<Trashed, CommandError> {
    let orphans = orphans_in(session, p.days)?;
    let (mut trashed, mut failed) = (Vec::new(), Vec::new());
    for path in p.paths {
        let path = absolute(&path)?;
        // Everything about it is checked again from its path.
        let orphan = Orphan { path: path.clone(), last_used: std::time::SystemTime::now(), bytes: 0 };
        match orphans.trash(&orphan, session.trash()) {
            Ok(()) => trashed.push(display(&path)),
            Err(e) => failed.push(NotDone { path: display(&path), reason: reason(&e) }),
        }
    }
    Ok(Trashed { trashed, failed })
}
