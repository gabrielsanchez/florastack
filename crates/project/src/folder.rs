//! A project on disk: the folder `<name>.florastack/` ([`ProjectFolder`]).
//!
//! ```text
//! Bee head.florastack/
//!   project.json      the project (crate::schema), replaced atomically on every save
//!   .lock             locked while FloraStack has the project open
//!   cache/            lossless intermediates: aligned frames, outputs, depth and confidence maps, masks
//!   previews/         small JPEGs of frames and outputs
//! ```
//!
//! A project open for editing holds its folder's lock, so that a second window or process can't open it as well, and
//! orphan cleanup ([`crate::orphans`]) never takes a project in use. Saving writes `project.json` to a temporary file
//! and renames it over the old one, so that a crash or a full disk leaves the last good save.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use florastack_core::LockFile;
use florastack_core::branding::{DISPLAY_NAME, PROJECT_EXTENSION};

use crate::cache::{Area, CacheRef, CacheWriter, PART_SUFFIX};
use crate::error::{CacheProblem, ProjectError, SchemaError};
use crate::fsutil::{read_capped, write_atomic};
use crate::hash::ContentHash;
use crate::model::{Frame, Project};
use crate::schema::{self, FILE_NAME, MAX_FILE_BYTES};
use crate::time::Timestamp;

/// The lock file's name in a project folder.
pub const LOCK_FILE: &str = ".lock";

/// The name of the software that saves project files, as `software` records it.
pub fn software() -> String {
    format!("{DISPLAY_NAME} {}", env!("CARGO_PKG_VERSION"))
}

/// A folder name for a project called `name`: the name with the characters that some file system refuses replaced by
/// `_`, shortened to 100 characters, then the project extension (`Bee head.florastack`).
pub fn folder_name(name: &str) -> String {
    let mut base: String = name
        .trim()
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .take(100)
        .collect();
    // Windows drops trailing dots and spaces from names.
    while base.ends_with(['.', ' ']) {
        base.pop();
    }
    if base.is_empty() {
        base.push_str("project");
    }
    let upper = base.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4 && (upper.starts_with("COM") || upper.starts_with("LPT")) && upper.ends_with(|c: char| c.is_ascii_digit()));
    if reserved {
        base.push('_');
    }
    base + PROJECT_EXTENSION
}

/// Whether an input frame's file is still the one that was added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameState {
    /// Same size and time, or same contents.
    Unchanged,
    /// Its contents changed: it must be read, developed and aligned again.
    Changed,
    /// Not there: what the cache holds from it (its aligned copy) can still be used.
    Missing,
}

/// A `.part` file older than this is left over from a crash, not being written.
const STALE_PART: Duration = Duration::from_hours(1);

/// A project folder, open for editing (holding its lock) or read-only.
#[derive(Debug)]
pub struct ProjectFolder {
    root: PathBuf,
    lock: Option<LockFile>,
    read_only: bool,
    migrated_from: Option<u32>,
}

impl ProjectFolder {
    /// Creates `<parent>/<name>.florastack/` (see [`folder_name`]) with a new, empty project, saved.
    pub fn create(parent: &Path, name: &str) -> Result<(ProjectFolder, Project), ProjectError> {
        let mut project = Project::new(Some(name))?;
        let root = parent.join(folder_name(name));
        match std::fs::create_dir(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(ProjectError::Exists { path: root }),
            Err(source) => return Err(ProjectError::Write { path: root, source }),
        }
        let folder = ProjectFolder::init(root, &mut project)?;
        Ok((folder, project))
    }

    /// Creates a new temporary project, unnamed, in `root`, the folder for temporary projects (created if needed):
    /// `untitled-<date>-<time>-<n>.florastack`. Orphan cleanup offers to move it to the trash once it is old and the
    /// user hasn't named it.
    pub fn create_temporary(root: &Path) -> Result<(ProjectFolder, Project), ProjectError> {
        std::fs::create_dir_all(root).map_err(|source| ProjectError::Write { path: root.to_path_buf(), source })?;
        let stamp: String = Timestamp::now().to_string().chars().filter(char::is_ascii_digit).take(14).collect();
        for n in 0..1000u32 {
            let dir = root.join(format!("untitled-{stamp}-{n}{PROJECT_EXTENSION}"));
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    let mut project = Project::new(None)?;
                    let folder = ProjectFolder::init(dir, &mut project)?;
                    return Ok((folder, project));
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(source) => return Err(ProjectError::Write { path: dir, source }),
            }
        }
        Err(ProjectError::Exists { path: root.join(format!("untitled-{stamp}-*{PROJECT_EXTENSION}")) })
    }

    /// Takes a new folder's lock and saves its project.
    fn init(root: PathBuf, project: &mut Project) -> Result<ProjectFolder, ProjectError> {
        for area in Area::ALL {
            let dir = root.join(area.dir());
            std::fs::create_dir_all(&dir).map_err(|source| ProjectError::Write { path: dir, source })?;
        }
        let lock = ProjectFolder::lock(&root)?;
        let mut folder = ProjectFolder { root, lock, read_only: false, migrated_from: None };
        folder.save(project)?;
        Ok(folder)
    }

    /// The folder's lock: an error when another process holds it, none (with a warning) on a file system without locks.
    fn lock(root: &Path) -> Result<Option<LockFile>, ProjectError> {
        match LockFile::try_acquire(&root.join(LOCK_FILE)) {
            Ok(Some(lock)) => {
                // When the project was last opened, for orphan cleanup.
                if let Err(e) = lock.touch() {
                    tracing::debug!("cannot touch the lock file of {}: {e}", root.display());
                }
                Ok(Some(lock))
            }
            Ok(None) => Err(ProjectError::InUse { path: root.to_path_buf() }),
            Err(e) => {
                tracing::warn!("cannot lock {}, so another FloraStack could open it at the same time: {e}", root.display());
                Ok(None)
            }
        }
    }

    /// Opens the project in the folder at `path` for editing (or the folder holding `path`, a `project.json`),
    /// upgrading an older project file in memory. The file is rewritten in the current format at the next save.
    pub fn open(path: &Path) -> Result<(ProjectFolder, Project), ProjectError> {
        ProjectFolder::open_with(path, false)
    }

    /// Opens a project without taking its lock, for reading only (`project info`, a preview): it can't be saved.
    pub fn open_read_only(path: &Path) -> Result<(ProjectFolder, Project), ProjectError> {
        ProjectFolder::open_with(path, true)
    }

    fn open_with(path: &Path, read_only: bool) -> Result<(ProjectFolder, Project), ProjectError> {
        let root = if path.file_name().is_some_and(|n| n == FILE_NAME) && path.is_file() {
            path.parent().unwrap_or(path).to_path_buf()
        } else {
            path.to_path_buf()
        };
        let file = root.join(FILE_NAME);
        if !file.is_file() {
            return Err(ProjectError::NotAProject { path: root });
        }
        let lock = if read_only { None } else { ProjectFolder::lock(&root)? };
        let bytes = read_capped(&file, MAX_FILE_BYTES)?;
        let decoded = schema::decode(&bytes).map_err(|e| schema_error(&file, e))?;
        if let Some(from) = decoded.migrated_from {
            tracing::info!("upgraded {} from project format {from} to {}", file.display(), schema::VERSION);
        }
        Ok((ProjectFolder { root, lock, read_only, migrated_from: decoded.migrated_from }, decoded.project))
    }

    /// Saves `project` to `project.json`, atomically, after checking it, and records the time and the software.
    /// The first save of an upgraded project keeps the old file as `project.v<version>.json`.
    pub fn save(&mut self, project: &mut Project) -> Result<(), ProjectError> {
        if self.read_only {
            return Err(ProjectError::ReadOnly { path: self.root.clone() });
        }
        project.validate().map_err(|source| ProjectError::Invalid { path: self.root.clone(), source })?;
        let mut saved = project.clone();
        saved.mark_saved(Timestamp::now(), software());
        let file = self.project_file();
        let bytes = schema::encode(&saved).map_err(|e| schema_error(&file, e))?;
        if let Some(from) = self.migrated_from {
            let backup = self.root.join(format!("project.v{from}.json"));
            if !backup.exists() {
                std::fs::copy(&file, &backup).map_err(|source| ProjectError::Write { path: backup, source })?;
            }
        }
        write_atomic(&file, &bytes)?;
        self.migrated_from = None;
        *project = saved;
        Ok(())
    }

    /// The project folder.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// The project file, `project.json`.
    pub fn project_file(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Whether the folder holds its lock (not when read-only, or on a file system without locks).
    pub fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// The format version the project file was upgraded from when it was opened, until the next save.
    pub fn migrated_from(&self) -> Option<u32> {
        self.migrated_from
    }

    /// Where a frame's file is: its path, or for a relative one, that path from the folder holding the project folder.
    pub fn frame_path(&self, frame: &Frame) -> PathBuf {
        let path = Path::new(&frame.path);
        if path.is_absolute() { path.to_path_buf() } else { self.root.parent().unwrap_or(&self.root).join(path) }
    }

    /// Whether a frame's file is still the one that was added: by size and modification time, and by hash when the
    /// time changed.
    pub fn check_frame(&self, frame: &Frame) -> Result<FrameState, ProjectError> {
        let path = self.frame_path(frame);
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(FrameState::Missing),
            Err(source) => return Err(ProjectError::Read { path, source }),
        };
        if !meta.is_file() {
            return Ok(FrameState::Missing);
        }
        if meta.len() != frame.size {
            return Ok(FrameState::Changed);
        }
        let modified = meta.modified().ok().map(Timestamp::from_system_time);
        if modified.is_some() && modified == frame.modified {
            return Ok(FrameState::Unchanged);
        }
        let file = std::fs::File::open(&path).map_err(|source| ProjectError::Read { path: path.clone(), source })?;
        let (hash, _) = ContentHash::of_reader(file).map_err(|source| ProjectError::Read { path, source })?;
        Ok(if hash == frame.hash { FrameState::Unchanged } else { FrameState::Changed })
    }

    fn writable(&self) -> Result<(), ProjectError> {
        if self.read_only { Err(ProjectError::ReadOnly { path: self.root.clone() }) } else { Ok(()) }
    }

    /// A writer for a new cache file in `area`, named after `stem` (such as `output-7` or `frame-3-aligned`) and
    /// `extension` (such as `tiff`), which the encoder chooses.
    pub fn writer(&self, area: Area, stem: &str, extension: &str) -> Result<CacheWriter, ProjectError> {
        self.writable()?;
        CacheWriter::create(&self.root, area, stem, extension)
    }

    /// Stores `bytes` as a new cache file (see [`ProjectFolder::writer`]).
    pub fn store(&self, area: Area, stem: &str, extension: &str, bytes: &[u8], key: Option<String>) -> Result<CacheRef, ProjectError> {
        use std::io::Write as _;
        let mut writer = self.writer(area, stem, extension)?;
        writer.write_all(bytes).map_err(|source| ProjectError::Write { path: self.root.join(area.dir()), source })?;
        writer.commit(key)
    }

    /// The size of a cache file, checked against its reference.
    fn open_cached(&self, reference: &CacheRef) -> Result<(std::fs::File, PathBuf), ProjectError> {
        let path = reference.path.resolve(&self.root);
        let problem = |source| ProjectError::Cache { path: path.clone(), source };
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(problem(CacheProblem::Missing)),
            Err(source) => return Err(ProjectError::Read { path, source }),
        };
        let meta = file.metadata().map_err(|source| ProjectError::Read { path: path.clone(), source })?;
        if !meta.is_file() {
            return Err(problem(CacheProblem::NotAFile));
        }
        if meta.len() != reference.size {
            return Err(problem(CacheProblem::Size { expected: reference.size, found: meta.len() }));
        }
        Ok((file, path))
    }

    /// Reads a cache file whole, after checking its size and hash against its reference.
    pub fn read(&self, reference: &CacheRef) -> Result<Vec<u8>, ProjectError> {
        let (file, path) = self.open_cached(reference)?;
        // The file's real size, checked above: a reference can't make this allocate more than the file holds.
        let mut bytes = Vec::with_capacity(usize::try_from(reference.size).unwrap_or(0));
        file.take(reference.size.saturating_add(1)).read_to_end(&mut bytes).map_err(|source| ProjectError::Read { path: path.clone(), source })?;
        let found = bytes.len() as u64;
        if found != reference.size {
            return Err(ProjectError::Cache { path, source: CacheProblem::Size { expected: reference.size, found } });
        }
        let hash = ContentHash::of(&bytes);
        if hash != reference.hash {
            return Err(ProjectError::Cache { path, source: CacheProblem::Hash { expected: reference.hash, found: hash } });
        }
        Ok(bytes)
    }

    /// Checks a cache file's size and hash without keeping its bytes.
    pub fn verify(&self, reference: &CacheRef) -> Result<(), ProjectError> {
        let (file, path) = self.open_cached(reference)?;
        let (hash, found) = ContentHash::of_reader(file.take(reference.size.saturating_add(1)))
            .map_err(|source| ProjectError::Read { path: path.clone(), source })?;
        if found != reference.size {
            return Err(ProjectError::Cache { path, source: CacheProblem::Size { expected: reference.size, found } });
        }
        if hash != reference.hash {
            return Err(ProjectError::Cache { path, source: CacheProblem::Hash { expected: reference.hash, found: hash } });
        }
        Ok(())
    }

    /// The project's cache files that can't be used, each with why: every one is checked for its presence and size,
    /// and with `full`, for its hash (which reads them all).
    pub fn check_cache(&self, project: &Project, full: bool) -> Vec<(CacheRef, ProjectError)> {
        let mut seen = BTreeSet::new();
        let mut issues = Vec::new();
        for reference in project.cache_refs() {
            if !seen.insert(&reference.path) {
                continue;
            }
            let result = if full { self.verify(reference) } else { self.open_cached(reference).map(drop) };
            if let Err(e) = result {
                issues.push((reference.clone(), e));
            }
        }
        issues
    }

    /// The files in `cache/` and `previews/` that the project doesn't refer to: left by removed outputs, by frames
    /// aligned again, or by a crash (temporary files at least an hour old). The engine removes them with
    /// [`ProjectFolder::remove_unreferenced`] once nothing, the undo history included, can refer to them again.
    pub fn unreferenced(&self, project: &Project) -> Result<Vec<PathBuf>, ProjectError> {
        let referenced: BTreeSet<_> = project.cache_refs().into_iter().map(|r| r.path.resolve(&self.root)).collect();
        let now = SystemTime::now();
        let mut out = Vec::new();
        for area in Area::ALL {
            let dir = self.root.join(area.dir());
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(source) => return Err(ProjectError::Read { path: dir, source }),
            };
            for entry in entries {
                let entry = entry.map_err(|source| ProjectError::Read { path: dir.clone(), source })?;
                let path = entry.path();
                // Files only: folders (a tile store's, say) aren't the cache's.
                let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
                if meta.is_dir() || referenced.contains(&path) {
                    continue;
                }
                let is_part = entry.file_name().to_str().is_some_and(|n| n.starts_with('.') && n.ends_with(PART_SUFFIX));
                if is_part && meta.modified().ok().and_then(|m| now.duration_since(m).ok()).is_none_or(|age| age < STALE_PART) {
                    continue;
                }
                out.push(path);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Deletes the files [`ProjectFolder::unreferenced`] lists and returns them. They are FloraStack's own
    /// intermediates, which nothing refers to, not the user's files.
    pub fn remove_unreferenced(&self, project: &Project) -> Result<Vec<PathBuf>, ProjectError> {
        self.writable()?;
        let files = self.unreferenced(project)?;
        for file in &files {
            match std::fs::remove_file(file) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(source) => return Err(ProjectError::Write { path: file.clone(), source }),
            }
        }
        Ok(files)
    }
}

/// The error for a project file that can't be read or written.
fn schema_error(file: &Path, e: SchemaError) -> ProjectError {
    let path = file.to_path_buf();
    match e {
        SchemaError::Newer { found, supported } => ProjectError::Newer { path, found, supported },
        SchemaError::Migration { from, source } => ProjectError::Migration { path, from, source },
        SchemaError::Invalid(source) => ProjectError::Invalid { path, source },
        other => ProjectError::Damaged { path, source: other },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_are_safe_everywhere() {
        assert_eq!(folder_name("Bee head"), "Bee head.florastack");
        assert_eq!(folder_name("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j.florastack");
        assert_eq!(folder_name("  dots...  "), "dots.florastack");
        assert_eq!(folder_name(".."), "project.florastack");
        assert_eq!(folder_name("con"), "con_.florastack");
        assert_eq!(folder_name("LPT1.x"), "LPT1.x_.florastack");
        assert_eq!(folder_name("Käfer 🐞"), "Käfer 🐞.florastack");
        assert_eq!(folder_name(&"x".repeat(300)).chars().count(), 100 + PROJECT_EXTENSION.len());
    }
}
