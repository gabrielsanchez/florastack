//! Files kept outside the project and what the caches hold: removing the tile folders of runs that crashed
//! (`cache.removeAbandoned`, which the desktop app does at startup), how much the caches take (`cache.info`), and the
//! folder that holds temporary projects and spilled tiles (`cache.setFolder`).

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{check_path, display};
use crate::error::{CommandError, Invalid};
use crate::registry::{CommandSpec, Effects, Meta, NoParams, Params};
use crate::session::Session;

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::instant(
            Meta::new("cache.removeAbandoned", "Remove abandoned tile files", "Removes the tile folders that crashed runs left in the tile folder.")
                .help(
                    "Stacks keep their accumulators' tiles in a folder of the tile folder (the system's temporary \
                     directory unless `compute.set` chose another) when they exceed their memory budget, and remove it \
                     when they end; a run that crashes leaves it behind. This removes every such folder that no run \
                     holds and that hasn't changed for a minute (a folder from before tile folders had locks, once it \
                     is a day old). They hold nothing a run can use again, so they are deleted, not moved to the trash. \
                     The desktop app runs this at startup.",
                )
                .effects(Effects::Files),
            remove_abandoned,
        ),
        CommandSpec::query(
            Meta::new("cache.info", "Cache sizes", "How much the caches take: temporary projects, spilled tiles and the open project's cache.").help(
                "Walks the folders and adds up their files: the temporary projects' folder (unnamed projects, which \
                 orphan cleanup moves to the trash once they are old), the tile folder (tiles of running stacks, and \
                 those crashed runs left), and the open project's `cache/` and `previews/` folders (results kept so \
                 that nothing is computed twice; cleaned of what the project no longer refers to when it closes).",
            ),
            info,
        ),
        CommandSpec::instant(
            Meta::new("cache.setFolder", "Set cache folder", "Puts temporary projects and spilled tiles in a folder of your choice.")
                .help(
                    "Temporary projects go into `projects` and spilled tiles into `tiles` under `folder`, made if they \
                     don't exist, for projects and stacks started from now on: a project open now stays where it is. A \
                     fast local drive with room to spare is best. For this engine; it isn't stored in the project.",
                )
                .effects(Effects::Files)
                .examples(&[r#"{ "folder": "D:/FloraStack cache" }"#]),
            set_folder,
        ),
    ]
}

/// The tile folders removed.
#[derive(Debug, Serialize, JsonSchema)]
struct Removed {
    /// The folder looked in: the tile folder.
    folder: String,
    removed: Vec<String>,
}

/// The tile folder: the one set, or the system's temporary directory.
fn tile_folder(session: &Session) -> PathBuf {
    session.tiling().spill_dir.unwrap_or_else(std::env::temp_dir)
}

fn remove_abandoned(session: &mut Session, _: NoParams) -> Result<Removed, CommandError> {
    let folder = tile_folder(session);
    let removed = florastack_core::TempFileStore::remove_abandoned(&folder).map_err(florastack_stack::StackError::from)?;
    Ok(Removed { folder: display(&folder), removed: removed.iter().map(|p| display(p)).collect() })
}

/// A folder and what its files take.
#[derive(Debug, Serialize, JsonSchema)]
struct FolderSize {
    folder: String,
    bytes: u64,
}

/// What the caches take.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct CacheInfo {
    /// Temporary projects; none when the engine has no folder for them.
    temporary_projects: Option<FolderSize>,
    /// Tiles of running stacks, and of crashed runs. In the system's temporary directory, only FloraStack's tile
    /// folders count.
    tiles: FolderSize,
    /// The open project's `cache/` and `previews/` folders; none without a project.
    project: Option<FolderSize>,
}

/// The size of the files under `path`, following no links; 0 for what can't be read.
fn size_of(path: &Path, only: Option<&str>) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if only.is_some_and(|prefix| !name.to_string_lossy().starts_with(prefix)) {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            total = total.saturating_add(size_of(&entry.path(), None));
        } else if kind.is_file() {
            total = total.saturating_add(entry.metadata().map_or(0, |m| m.len()));
        }
    }
    total
}

fn info(session: &Session, _: NoParams) -> Result<CacheInfo, CommandError> {
    let temporary_projects = session.temporary_projects().map(|p| FolderSize { folder: display(p), bytes: size_of(p, None) });
    let tiles = if let Some(folder) = session.tiling().spill_dir {
        FolderSize { bytes: size_of(&folder, None), folder: display(&folder) }
    } else {
        // In the system's temporary directory, only the tile folders, named after the app.
        let folder = std::env::temp_dir();
        let prefix = format!("{}-tiles-", florastack_core::branding::GUI_BINARY);
        FolderSize { bytes: size_of(&folder, Some(&prefix)), folder: display(&folder) }
    };
    let project = session.folder().ok().map(florastack_project::ProjectFolder::path).map(|root| FolderSize {
        folder: display(root),
        bytes: size_of(&root.join("cache"), None).saturating_add(size_of(&root.join("previews"), None)),
    });
    Ok(CacheInfo { temporary_projects, tiles, project })
}

/// The cache folder.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FolderParams {
    /// The folder: temporary projects go into its `projects` folder, spilled tiles into its `tiles` folder.
    folder: String,
}

impl Params for FolderParams {
    fn check(&self) -> Result<(), Invalid> {
        check_path("folder", &self.folder)
    }
}

fn set_folder(session: &mut Session, p: FolderParams) -> Result<CacheInfo, CommandError> {
    let folder = super::absolute(&p.folder)?;
    let (projects, tiles) = (folder.join("projects"), folder.join("tiles"));
    for made in [&projects, &tiles] {
        std::fs::create_dir_all(made).map_err(|e| session.unavailable(format!("{} can't be made: {e}", made.display())))?;
    }
    session.set_temporary_projects(projects);
    let mut tiling = session.tiling();
    tiling.spill_dir = Some(tiles);
    session.set_tiling(tiling);
    info(session, NoParams::default())
}
