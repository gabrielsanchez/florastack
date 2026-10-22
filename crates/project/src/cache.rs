//! The project cache: files in the project folder that project.json refers to by [`CacheRef`], so that a reopened
//! project needs no recomputation. `cache/` holds lossless intermediates (aligned frames, outputs, depth, confidence
//! and motion maps, masks), `previews/` small JPEGs for the frame list and the outputs list ([`Area`]).
//!
//! This crate owns the files' names, their references and their integrity; it never looks inside them. The engine
//! encodes and decodes them with florastack-codecs and hands this crate bytes: [`CacheWriter`] (from
//! `ProjectFolder::writer`) streams them to a temporary file while hashing them, then gives the file its final name,
//! `<stem>-<first 16 hex digits of its hash>.<extension>`, so that a file is never changed once it has a name: a new
//! version of an aligned frame or an output is a new file, and the old one stays valid until the project no longer
//! refers to it. A crash leaves at worst a temporary or unreferenced file behind, never a damaged referenced one.
//! Reading a file back checks its size and hash against its reference.

use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ProjectError;
use crate::hash::{ContentHash, Hasher};

/// Which folder of the project a cache file is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Area {
    /// `cache/`: lossless intermediates.
    Cache,
    /// `previews/`: small JPEGs.
    Previews,
}

impl Area {
    pub const ALL: [Area; 2] = [Area::Cache, Area::Previews];

    /// The folder's name in the project folder.
    pub fn dir(self) -> &'static str {
        match self {
            Area::Cache => "cache",
            Area::Previews => "previews",
        }
    }
}

/// A cache file's path relative to the project folder, such as `cache/output-7-3f9a1c2b4d5e6f70.tiff`: one of the
/// [`Area`] folders and a plain file name in it, so that a hostile project file can't point outside its folder.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CachePath {
    area: Area,
    file: String,
}

/// The device names that Windows reserves in every folder, with or without an extension.
const RESERVED: [&str; 4] = ["con", "prn", "aux", "nul"];

/// Why `file` can't be a cache file's name, if it can't.
fn file_name_problem(file: &str) -> Option<&'static str> {
    if file.is_empty() || file.len() > 128 {
        return Some("a cache file name has 1 to 128 characters");
    }
    if !file.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) {
        return Some("a cache file name has only letters, digits, '.', '-' and '_'");
    }
    if file.starts_with(['.', '-']) || file.contains("..") {
        return Some("a cache file name doesn't start with '.' or '-', or hold \"..\"");
    }
    let stem = file.split('.').next().unwrap_or_default().to_ascii_lowercase();
    let device = RESERVED.contains(&stem.as_str())
        || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.ends_with(|c: char| c.is_ascii_digit()));
    device.then_some("a cache file name is not a device name that Windows reserves")
}

impl CachePath {
    pub(crate) fn new(area: Area, file: &str) -> Option<CachePath> {
        file_name_problem(file).is_none().then(|| CachePath { area, file: file.to_owned() })
    }

    /// Parses `cache/<file>` or `previews/<file>`.
    pub fn parse(text: &str) -> Result<CachePath, &'static str> {
        let (dir, file) = text.split_once('/').ok_or("a cache path is cache/<file> or previews/<file>")?;
        let area = Area::ALL.into_iter().find(|a| a.dir() == dir).ok_or("a cache path is cache/<file> or previews/<file>")?;
        match file_name_problem(file) {
            None => Ok(CachePath { area, file: file.to_owned() }),
            Some(problem) => Err(problem),
        }
    }

    pub fn area(&self) -> Area {
        self.area
    }

    pub fn file_name(&self) -> &str {
        &self.file
    }

    /// The file's path in the project folder at `root`.
    pub fn resolve(&self, root: &Path) -> PathBuf {
        root.join(self.area.dir()).join(&self.file)
    }
}

impl fmt::Display for CachePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.area.dir(), self.file)
    }
}

impl fmt::Debug for CachePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl Serialize for CachePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for CachePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<CachePath, D::Error> {
        let text = String::deserialize(deserializer)?;
        CachePath::parse(&text).map_err(|e| serde::de::Error::custom(format!("{text:?}: {e}")))
    }
}

/// A file in the project cache, as project.json refers to it: where it is, and its size and hash, which are checked
/// whenever it is read.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheRef {
    pub path: CachePath,
    pub size: u64,
    pub hash: ContentHash,
    /// What the file was made from, as its maker fingerprints it (such as the develop settings' and the alignment's),
    /// so that the engine can tell a stale file from a current one. This crate only stores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// The longest cache key, in bytes.
pub(crate) const MAX_KEY_BYTES: usize = 512;

/// Checks a stem that names cache files: 1 to 64 letters, digits, `-` and `_`, not starting with `-`.
fn check_stem(stem: &str) -> Result<(), ProjectError> {
    let ok = (1..=64).contains(&stem.len()) && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') && !stem.starts_with('-');
    if ok { Ok(()) } else { Err(ProjectError::CacheName { name: stem.to_owned() }) }
}

/// Checks a file extension: 1 to 8 lower-case letters and digits.
fn check_extension(extension: &str) -> Result<(), ProjectError> {
    let ok = (1..=8).contains(&extension.len()) && extension.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if ok { Ok(()) } else { Err(ProjectError::CacheName { name: extension.to_owned() }) }
}

/// The suffix of the temporary files that cache files are written to before they get their names.
pub(crate) const PART_SUFFIX: &str = ".part";

/// A cache file being written: bytes go to a temporary file in its folder and into its hash, and [`CacheWriter::commit`]
/// gives it its final name and returns its reference. Dropped without a commit, it removes the temporary file.
pub struct CacheWriter {
    file: Option<BufWriter<File>>,
    temp: PathBuf,
    dir: PathBuf,
    area: Area,
    stem: String,
    extension: String,
    hasher: Hasher,
}

impl CacheWriter {
    /// A writer for a file in `area` of the project folder at `root`, to be named after `stem` and `extension`.
    pub(crate) fn create(root: &Path, area: Area, stem: &str, extension: &str) -> Result<CacheWriter, ProjectError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        check_stem(stem)?;
        check_extension(extension)?;
        let dir = root.join(area.dir());
        std::fs::create_dir_all(&dir).map_err(|source| ProjectError::Write { path: dir.clone(), source })?;
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".{stem}.{}.{n}{PART_SUFFIX}", std::process::id()));
        let file = File::create(&temp).map_err(|source| ProjectError::Write { path: temp.clone(), source })?;
        Ok(CacheWriter {
            file: Some(BufWriter::with_capacity(1 << 20, file)),
            temp,
            dir,
            area,
            stem: stem.to_owned(),
            extension: extension.to_owned(),
            hasher: Hasher::new(),
        })
    }

    /// The bytes written so far.
    pub fn len(&self) -> u64 {
        self.hasher.len
    }

    pub fn is_empty(&self) -> bool {
        self.hasher.len == 0
    }

    /// Flushes the file to disk, names it, and returns its reference, with `key` (see [`CacheRef::key`]).
    pub fn commit(mut self, key: Option<String>) -> Result<CacheRef, ProjectError> {
        if let Some(key) = &key
            && (key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control))
        {
            return Err(ProjectError::CacheKey { len: key.len() });
        }
        let failed = |temp: &Path, source| {
            let _ = std::fs::remove_file(temp);
            ProjectError::Write { path: temp.to_path_buf(), source }
        };
        let Some(writer) = self.file.take() else {
            return Err(ProjectError::Write { path: self.temp.clone(), source: std::io::Error::other("the cache file was already committed") });
        };
        // The file is closed at the end of this block, before it is renamed.
        {
            let file = writer.into_inner().map_err(|e| failed(&self.temp, e.into_error()))?;
            file.sync_all().map_err(|e| failed(&self.temp, e))?;
        }
        let hasher = std::mem::replace(&mut self.hasher, Hasher::new());
        let size = hasher.len;
        let hash = hasher.finish();
        let name = format!("{}-{}.{}", self.stem, hash.short(), self.extension);
        let path = CachePath::new(self.area, &name).ok_or_else(|| ProjectError::CacheName { name: name.clone() })?;
        let target = self.dir.join(&name);
        // A file with this name has these very bytes (its name holds their hash), so replacing it changes nothing.
        std::fs::rename(&self.temp, &target).map_err(|e| failed(&self.temp, e))?;
        Ok(CacheRef { path, size, hash, key })
    }
}

impl Write for CacheWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let file = self.file.as_mut().ok_or_else(|| std::io::Error::other("the cache file was already committed"))?;
        let n = file.write(buf)?;
        self.hasher.update(buf.get(..n).unwrap_or_default());
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            drop(file);
            let _ = std::fs::remove_file(&self.temp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_stay_inside_their_folder() {
        let ok = CachePath::parse("cache/output-7-3f9a1c2b4d5e6f70.tiff").unwrap();
        assert_eq!((ok.area(), ok.file_name()), (Area::Cache, "output-7-3f9a1c2b4d5e6f70.tiff"));
        assert_eq!(ok.to_string(), "cache/output-7-3f9a1c2b4d5e6f70.tiff");
        assert_eq!(CachePath::parse("previews/frame-1.jpg").unwrap().area(), Area::Previews);
        for bad in [
            "",
            "output.tiff",
            "cache/",
            "cache/../project.json",
            "cache/..",
            "cache/a/b.tiff",
            "cache\\a.tiff",
            "/cache/a.tiff",
            "cache/.hidden",
            "cache/-x",
            "tmp/a.tiff",
            "Cache/a.tiff",
            "cache/a b.tiff",
            "cache/é.tiff",
            "cache/CON.tiff",
            "cache/nul",
            "cache/com1.tiff",
            "cache/LPT9",
            "cache/C:a.tiff",
        ] {
            assert!(CachePath::parse(bad).is_err(), "{bad:?}");
        }
        assert!(CachePath::parse(&format!("cache/{}", "a".repeat(129))).is_err());
        assert!(CachePath::parse("cache/console.tiff").is_ok(), "only the device names themselves are reserved");
    }

    #[test]
    fn stems_and_extensions_are_checked() {
        assert!(check_stem("output-7").is_ok() && check_stem("frame_3-aligned").is_ok());
        for bad in ["", "-x", "a.b", "a/b", "a b", &"s".repeat(65)] {
            assert!(check_stem(bad).is_err(), "{bad:?}");
        }
        assert!(check_extension("tiff").is_ok() && check_extension("exr").is_ok());
        for bad in ["", "TIFF", ".tif", "toolongext"] {
            assert!(check_extension(bad).is_err(), "{bad:?}");
        }
    }
}
