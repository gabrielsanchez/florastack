//! Orphan cleanup: temporary projects, which FloraStack makes in a folder of its own when the user starts stacking
//! without naming a project ([`crate::ProjectFolder::create_temporary`]), pile up there once nobody needs them.
//! [`Orphans::find`] lists the ones that may go, for the app to offer at startup (and the CLI's
//! `project clean-orphans`), and [`Orphans::trash`] moves one to the trash, never deleting it, so that the user can
//! still restore it.
//!
//! A folder is a temporary project when all of these hold, checked again just before it is moved:
//!
//! - it is a folder (not a link) directly in the temporary-projects folder, named `*.florastack`;
//! - its `project.json` is a FloraStack project file (of any version) with no name.
//!
//! A temporary project is an orphan when no FloraStack holds its lock (it isn't open anywhere) and either:
//!
//! - it hasn't been saved or opened for the given age (by the times of `project.json` and of its lock file, which
//!   opening a project touches), or
//! - it is beyond the disk budget ([`Orphans::with_budget`]): the temporary projects are taken from the most recently
//!   used, the open ones first, and once one doesn't fit within the budget, it and every older one are orphans. The
//!   most recently used closed project is always kept, whatever its size, so that the last run can be recovered.
//!
//! A folder whose project file can't be read is left alone, and neither it nor a named project counts toward the
//! budget. The tile folders that crashed runs leave in the temporary directory are the other half of startup cleanup,
//! and are removed outright by `florastack_core::TempFileStore::remove_abandoned`: they hold only spilled tiles,
//! never the user's work.

use florastack_core::time::{Duration, SystemTime};
use std::path::{Path, PathBuf};

use florastack_core::branding::PROJECT_EXTENSION;
use florastack_core::{LockFile, LockState};

use crate::error::ProjectError;
use crate::folder::LOCK_FILE;
use crate::fsutil::read_capped;
use crate::schema::{self, FILE_NAME, MAX_FILE_BYTES};
use crate::trash::Trash;

/// A temporary project that may go.
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
    /// The age after which a temporary project goes, if age counts.
    max_age: Option<Duration>,
    /// The bytes the temporary projects may take, if their size counts.
    budget: Option<u64>,
}

/// A temporary project as [`Orphans::examine`] found it.
struct Examined {
    path: PathBuf,
    last_used: SystemTime,
    /// Whether a FloraStack holds its lock.
    open: bool,
}

impl Orphans {
    /// The age after which a temporary project is offered for removal, unless the preferences say otherwise.
    pub const DEFAULT_DAYS: u32 = 30;

    /// Orphans in `root`, the temporary-projects folder: those unused for `days` days.
    pub fn new(root: impl Into<PathBuf>, days: u32) -> Orphans {
        Orphans { root: root.into(), max_age: Some(Duration::from_hours(u64::from(days) * 24)), budget: None }
    }

    /// Orphans in `root` by size alone: those beyond a budget of `bytes`, whatever their age.
    pub fn beyond_budget(root: impl Into<PathBuf>, bytes: u64) -> Orphans {
        Orphans { root: root.into(), max_age: None, budget: Some(bytes) }
    }

    /// These orphans and also those beyond a budget of `bytes` (see the module documentation).
    #[must_use]
    pub fn with_budget(mut self, bytes: u64) -> Orphans {
        self.budget = Some(bytes);
        self
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
        let entries = match florastack_core::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(ProjectError::Read { path: self.root.clone(), source }),
        };
        let mut projects = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            match Self::examine(&path) {
                Ok(project) => projects.push(project),
                Err(reason) => tracing::debug!("{} is not an orphan: {reason}", path.display()),
            }
        }
        // The most recently used first, for the budget.
        projects.sort_by(|a, b| b.last_used.cmp(&a.last_used).then_with(|| a.path.cmp(&b.path)));
        let sizes: Vec<Option<u64>> = projects.iter().map(|p| self.budget.map(|_| folder_bytes(&p.path, 0))).collect();
        let mut used = projects.iter().zip(&sizes).filter(|(p, _)| p.open).filter_map(|(_, s)| *s).fold(0, u64::saturating_add);
        let (mut kept_one, mut over) = (false, false);
        let mut found = Vec::new();
        for (project, size) in projects.into_iter().zip(sizes) {
            if project.open {
                continue;
            }
            // A time in the future (a clock set back) counts as now.
            let age = now.duration_since(project.last_used).unwrap_or(Duration::ZERO);
            let old = self.max_age.is_some_and(|max| age >= max);
            let beyond = match (self.budget, size) {
                (Some(budget), Some(size)) => {
                    over = over || (kept_one && used.saturating_add(size) > budget);
                    if !over {
                        used = used.saturating_add(size);
                        kept_one = true;
                    }
                    over
                }
                _ => false,
            };
            if old || beyond {
                let bytes = size.unwrap_or_else(|| folder_bytes(&project.path, 0));
                found.push(Orphan { path: project.path, last_used: project.last_used, bytes });
            } else {
                tracing::debug!("{} is not an orphan: {}", project.path.display(), self.recent());
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
        self.trash_all_at(std::slice::from_ref(&orphan.path), trash, now)?.into_iter().next().unwrap_or(Ok(()))
    }

    /// Moves the orphans at `paths` to `trash`, each after checking again that it still is one: the orphans are found
    /// again once, and each folder is examined again just before it is moved. One result per path, in their order;
    /// the error is the temporary-projects folder's, when it can't be read.
    pub fn trash_all(&self, paths: &[PathBuf], trash: &dyn Trash) -> Result<Vec<Result<(), ProjectError>>, ProjectError> {
        self.trash_all_at(paths, trash, SystemTime::now())
    }

    /// [`Orphans::trash_all`] as if the time were `now`.
    pub fn trash_all_at(&self, paths: &[PathBuf], trash: &dyn Trash, now: SystemTime) -> Result<Vec<Result<(), ProjectError>>, ProjectError> {
        let found = self.find_at(now)?;
        Ok(paths.iter().map(|path| self.trash_one(path, &found, trash)).collect())
    }

    fn trash_one(&self, path: &Path, found: &[Orphan], trash: &dyn Trash) -> Result<(), ProjectError> {
        let refuse = |reason| ProjectError::NotOrphan { path: path.to_path_buf(), reason };
        // The path must be one of this folder's entries, whatever the caller passes.
        let inside = path.parent().zip(path.file_name()).is_some_and(|(parent, _)| same_dir(parent, &self.root));
        if !inside {
            return Err(refuse("it isn't in the temporary-projects folder"));
        }
        let project = Self::examine(path).map_err(refuse)?;
        if project.open {
            return Err(refuse("it is open in FloraStack"));
        }
        let orphan = found.iter().find(|o| o.path.file_name() == path.file_name()).ok_or_else(|| refuse(self.recent()))?;
        if orphan.last_used != project.last_used {
            return Err(refuse("it was used since it was found"));
        }
        trash.trash(path).map_err(|source| ProjectError::Trash { path: path.to_path_buf(), source })?;
        tracing::info!("moved the temporary project {} to the trash", path.display());
        Ok(())
    }

    /// Why a closed temporary project isn't an orphan.
    fn recent(&self) -> &'static str {
        if self.budget.is_some() { "it was used recently and fits within the disk budget" } else { "it was used recently" }
    }

    /// The temporary project at `path`, or why it isn't one.
    fn examine(path: &Path) -> Result<Examined, &'static str> {
        let meta = florastack_core::fs::symlink_metadata(path).map_err(|_| "it can't be examined")?;
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
        let open = match LockFile::probe(&lock) {
            Ok(LockState::Free | LockState::Missing) => false,
            Ok(LockState::Held) => true,
            Err(_) => return Err("it can't be told whether it is open"),
        };
        let times = [florastack_core::fs::metadata(&file), florastack_core::fs::metadata(&lock)];
        let last_used = times.iter().filter_map(|m| m.as_ref().ok()?.modified().ok()).max().ok_or("its age can't be told")?;
        Ok(Examined { path: path.to_path_buf(), last_used, open })
    }
}

/// Whether two paths name the same folder.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (florastack_core::fs::canonicalize(a), florastack_core::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The bytes of the files in a folder and its subfolders, without following links, eight levels deep at most.
fn folder_bytes(dir: &Path, depth: usize) -> u64 {
    let Ok(entries) = florastack_core::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match florastack_core::fs::symlink_metadata(e.path()) {
            Ok(m) if m.is_file() => m.len(),
            Ok(m) if m.is_dir() && depth < 8 => folder_bytes(&e.path(), depth + 1),
            _ => 0,
        })
        .fold(0, u64::saturating_add)
}
