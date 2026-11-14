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
                 that nothing is computed twice; cleaned of what the project no longer refers to when it closes). \
                 `temporaryProjects.drive` says what that folder is on: from a `removable` or `network` drive the \
                 system may delete what orphan cleanup moves to the trash instead of keeping it.",
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
    session.tiling().spill_dir.unwrap_or_else(florastack_core::fs::temp_dir)
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

/// The temporary-projects folder, what its files take and what it is on.
#[derive(Debug, Serialize, JsonSchema)]
struct ProjectsFolder {
    folder: String,
    bytes: u64,
    /// What the folder is on. From a `removable` or `network` drive, the system may delete what orphan cleanup moves
    /// to the trash instead of keeping it: Windows has no Recycle Bin there.
    drive: Drive,
}

/// What a folder is on, as far as the system says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Drive {
    /// A drive built into the computer, or one the system treats as such.
    Local,
    /// A drive that can be taken out: a memory card, a USB stick.
    Removable,
    /// Another computer's folder.
    Network,
    /// The system doesn't say.
    Unknown,
}

/// A mounted volume: where, whether it can be taken out, and its file system's name.
type Volume = (PathBuf, bool, String);

impl Drive {
    /// What `folder` is on. A folder that doesn't exist yet is judged by the nearest one above it that does.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn of(folder: &Path) -> Drive {
        let Some(resolved) = folder.ancestors().find_map(|p| florastack_core::fs::canonicalize(p).ok()) else { return Drive::Unknown };
        let volumes: Vec<Volume> = sysinfo::Disks::new_with_refreshed_list_specifics(sysinfo::DiskRefreshKind::nothing())
            .map(|disks| {
                disks.iter().map(|d| (d.mount_point().to_path_buf(), d.is_removable(), d.file_system().to_string_lossy().into_owned())).collect()
            })
            .unwrap_or_default();
        Drive::among(&resolved, &volumes)
    }

    /// The browser's storage is the page's own.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn of(_: &Path) -> Drive {
        Drive::Unknown
    }

    /// What `resolved`, a path with its links and drive mappings resolved, is on, given the volumes mounted.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code, reason = "the browser has no volumes"))]
    fn among(resolved: &Path, volumes: &[Volume]) -> Drive {
        use std::path::{Component, Prefix};
        // Windows resolves a share, and a drive letter mapped to one, to `\\server\share`.
        let mut plain = PathBuf::new();
        for component in resolved.components() {
            match component {
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::UNC(..) | Prefix::VerbatimUNC(..) => return Drive::Network,
                    Prefix::VerbatimDisk(letter) | Prefix::Disk(letter) => plain.push(format!("{}:", char::from(letter.to_ascii_uppercase()))),
                    _ => return Drive::Unknown,
                },
                other => plain.push(other.as_os_str()),
            }
        }
        let within = |mount: &Path| {
            let mut inner = plain.components();
            mount
                .components()
                .all(|m| inner.next().is_some_and(|p| if cfg!(windows) { p.as_os_str().eq_ignore_ascii_case(m.as_os_str()) } else { p == m }))
        };
        // The volume mounted deepest on the way to the folder is the one it is on.
        let Some((_, removable, file_system)) =
            volumes.iter().filter(|(mount, ..)| within(mount)).max_by_key(|(mount, ..)| mount.components().count())
        else {
            return Drive::Unknown;
        };
        let name = file_system.to_ascii_lowercase();
        let shared = ["nfs", "nfs4", "cifs", "smb3", "smbfs", "afpfs", "webdav", "9p", "fuse.sshfs"].contains(&name.as_str());
        if shared {
            Drive::Network
        } else if *removable {
            Drive::Removable
        } else {
            Drive::Local
        }
    }
}

/// What the caches take.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct CacheInfo {
    /// Temporary projects; none when the engine has no folder for them.
    temporary_projects: Option<ProjectsFolder>,
    /// Tiles of running stacks, and of crashed runs. In the system's temporary directory, only FloraStack's tile
    /// folders count.
    tiles: FolderSize,
    /// The open project's `cache/` and `previews/` folders; none without a project.
    project: Option<FolderSize>,
}

/// The size of the files under `path`, following no links; 0 for what can't be read.
fn size_of(path: &Path, only: Option<&str>) -> u64 {
    let Ok(entries) = florastack_core::fs::read_dir(path) else { return 0 };
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
    let temporary_projects =
        session.temporary_projects().map(|p| ProjectsFolder { folder: display(p), bytes: size_of(p, None), drive: Drive::of(p) });
    let tiles = if let Some(folder) = session.tiling().spill_dir {
        FolderSize { bytes: size_of(&folder, None), folder: display(&folder) }
    } else {
        // In the system's temporary directory, only the tile folders, named after the app.
        let folder = florastack_core::fs::temp_dir();
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
        florastack_core::fs::create_dir_all(made).map_err(|e| session.unavailable(format!("{} can't be made: {e}", made.display())))?;
    }
    session.set_temporary_projects(projects);
    let mut tiling = session.tiling();
    tiling.spill_dir = Some(tiles);
    session.set_tiling(tiling);
    info(session, NoParams::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_on_the_volume_mounted_deepest_on_its_way() {
        let volume = |mount: &str, removable: bool, file_system: &str| (PathBuf::from(mount), removable, file_system.to_owned());
        #[cfg(windows)]
        {
            let volumes = [volume("C:\\", false, "NTFS"), volume("E:\\", true, "exFAT"), volume("C:\\mounted\\card", true, "FAT32")];
            for (path, drive) in [
                ("\\\\?\\C:\\Users\\me\\AppData", Drive::Local),
                ("c:\\users\\me", Drive::Local),
                ("\\\\?\\E:\\cache\\projects", Drive::Removable),
                ("\\\\?\\C:\\Mounted\\Card\\cache", Drive::Removable),
                ("\\\\?\\UNC\\nas\\photos\\cache", Drive::Network),
                ("\\\\nas\\photos\\cache", Drive::Network),
                ("\\\\?\\Z:\\cache", Drive::Unknown),
            ] {
                assert_eq!(Drive::among(Path::new(path), &volumes), drive, "{path}");
            }
        }
        #[cfg(not(windows))]
        {
            let volumes =
                [volume("/", false, "ext4"), volume("/media/card", true, "vfat"), volume("/mnt/nas", false, "nfs4"), volume("/home", false, "apfs")];
            for (path, drive) in [
                ("/var/tmp/cache", Drive::Local),
                ("/home/me/.cache", Drive::Local),
                ("/media/card/cache", Drive::Removable),
                ("/media/cards", Drive::Local),
                ("/mnt/nas/photos", Drive::Network),
            ] {
                assert_eq!(Drive::among(Path::new(path), &volumes), drive, "{path}");
            }
            assert_eq!(Drive::among(Path::new("/var/tmp"), &[]), Drive::Unknown);
        }
        // Whatever this machine's temporary folder is on, asking never fails.
        let _ = Drive::of(&florastack_core::fs::temp_dir().join("not").join("there").join("yet"));
    }
}
