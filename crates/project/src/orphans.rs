//! Orphan cleanup: temporary projects, which FloraStack makes in a folder of its own when the user starts stacking
//! without naming a project ([`crate::ProjectFolder::create_temporary`]), pile up there once nobody needs them.
//! [`Orphans::find`] lists the ones that are old enough to go, for the app to offer at startup (and the CLI's
//! `project clean-orphans`), and [`Orphans::trash`] moves one to the trash, never deleting it, so that the user can
//! still restore it.
//!
//! A folder counts as an orphan only when all of these hold, checked again just before it is moved:
//!
//! - it is a folder (not a link) directly in the temporary-projects folder, named `*.florastack`;
//! - its `project.json` is a FloraStack project file (of any version) with no name;
//! - no FloraStack holds its lock: it isn't open anywhere;
//! - it hasn't been saved or opened for the given age (by the times of `project.json` and of its lock file, which
//!   opening a project touches).
//!
//! A folder whose project file can't be read is left alone. The tile folders that crashed runs leave in the
//! temporary directory are the other half of startup cleanup, and are removed outright by
//! `florastack_core::TempFileStore::remove_abandoned`: they hold only spilled tiles, never the user's work.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use florastack_core::branding::PROJECT_EXTENSION;
use florastack_core::{LockFile, LockState};

use crate::error::ProjectError;
use crate::folder::LOCK_FILE;
use crate::fsutil::read_capped;
use crate::schema::{self, FILE_NAME, MAX_FILE_BYTES};
use crate::trash::Trash;

/// A temporary project old enough to go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Orphan {
    pub path: PathBuf,
    /// When it was last saved or opened.
    pub last_used: SystemTime,
    /// The bytes of its files, for telling the user how much space it frees.
    pub bytes: u64,
}

/// Finds and removes orphaned temporary projects in one folder. See the module documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Orphans {
    root: PathBuf,
    max_age: Duration,
}

impl Orphans {
    /// The age after which a temporary project is offered for removal, unless the preferences say otherwise.
    pub const DEFAULT_DAYS: u32 = 30;

    /// Orphans in `root`, the temporary-projects folder: those unused for `days` days.
    pub fn new(root: impl Into<PathBuf>, days: u32) -> Orphans {
        Orphans { root: root.into(), max_age: Duration::from_hours(u64::from(days) * 24) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The orphans now, oldest first. A missing temporary-projects folder has none.
    pub fn find(&self) -> Result<Vec<Orphan>, ProjectError> {
        self.find_at(SystemTime::now())
    }

    /// The orphans as if the time were `now`.
    pub fn find_at(&self, now: SystemTime) -> Result<Vec<Orphan>, ProjectError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(ProjectError::Read { path: self.root.clone(), source }),
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            match self.check(&path, now) {
                Ok(orphan) => found.push(orphan),
                Err(reason) => tracing::debug!("{} is not an orphan: {reason}", path.display()),
            }
        }
        found.sort_by(|a, b| a.last_used.cmp(&b.last_used).then_with(|| a.path.cmp(&b.path)));
        Ok(found)
    }

    /// Moves an orphan to `trash` after checking again that it still is one.
    pub fn trash(&self, orphan: &Orphan, trash: &dyn Trash) -> Result<(), ProjectError> {
        self.trash_at(orphan, trash, SystemTime::now())
    }

    /// [`Orphans::trash`] as if the time were `now`.
    pub fn trash_at(&self, orphan: &Orphan, trash: &dyn Trash, now: SystemTime) -> Result<(), ProjectError> {
        let path = &orphan.path;
        // The path must be one of this folder's entries, whatever the caller passes.
        let inside = path.parent().zip(path.file_name()).is_some_and(|(parent, _)| same_dir(parent, &self.root));
        if !inside {
            return Err(ProjectError::NotOrphan { path: path.clone(), reason: "it isn't in the temporary-projects folder" });
        }
        self.check(path, now).map_err(|reason| ProjectError::NotOrphan { path: path.clone(), reason })?;
        trash.trash(path).map_err(|source| ProjectError::Trash { path: path.clone(), source })?;
        tracing::info!("moved the temporary project {} to the trash", path.display());
        Ok(())
    }

    /// The orphan at `path`, or why it isn't one.
    fn check(&self, path: &Path, now: SystemTime) -> Result<Orphan, &'static str> {
        let meta = std::fs::symlink_metadata(path).map_err(|_| "it can't be examined")?;
        if !meta.is_dir() {
            return Err("it isn't a folder");
        }
        if !path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.len() > PROJECT_EXTENSION.len() && n.ends_with(PROJECT_EXTENSION)) {
            return Err("it isn't named like a project folder");
        }
        let file = path.join(FILE_NAME);
        let bytes = read_capped(&file, MAX_FILE_BYTES).map_err(|_| "its project.json can't be read")?;
        let header = schema::header(&bytes).map_err(|_| "its project.json isn't a project file")?;
        if header.name.is_some() {
            return Err("it has a name, so it isn't temporary");
        }
        let lock = path.join(LOCK_FILE);
        match LockFile::probe(&lock) {
            Ok(LockState::Free | LockState::Missing) => {}
            Ok(LockState::Held) => return Err("it is open in FloraStack"),
            Err(_) => return Err("it can't be told whether it is open"),
        }
        let times = [std::fs::metadata(&file), std::fs::metadata(&lock)];
        let last_used = times.iter().filter_map(|m| m.as_ref().ok()?.modified().ok()).max().ok_or("its age can't be told")?;
        // A time in the future (a clock set back) counts as now.
        if now.duration_since(last_used).unwrap_or(Duration::ZERO) < self.max_age {
            return Err("it was used recently");
        }
        Ok(Orphan { path: path.to_path_buf(), last_used, bytes: folder_bytes(path, 0) })
    }
}

/// Whether two paths name the same folder.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The bytes of the files in a folder and its subfolders, without following links, eight levels deep at most.
fn folder_bytes(dir: &Path, depth: usize) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match std::fs::symlink_metadata(e.path()) {
            Ok(m) if m.is_file() => m.len(),
            Ok(m) if m.is_dir() && depth < 8 => folder_bytes(&e.path(), depth + 1),
            _ => 0,
        })
        .fold(0, u64::saturating_add)
}
