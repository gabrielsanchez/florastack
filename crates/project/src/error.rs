//! What can go wrong with a project ([`ProjectError`]), and the more detailed errors it keeps as its source: a
//! project file that can't be read ([`SchemaError`], [`MigrationError`]), a project that isn't consistent
//! ([`Problem`]), a cache file that is missing or damaged ([`CacheProblem`]) and a preset that can't be used
//! ([`PresetError`]). Every message says what failed, and the top-level ones what the user can do about it.

use std::path::PathBuf;

use crate::hash::ContentHash;
use crate::trash::TrashError;

/// Why a project, a preset or a cache file can't be used.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{} is not a FloraStack project: it has no project.json", .path.display())]
    NotAProject { path: PathBuf },
    #[error("{} already exists: choose another name or folder", .path.display())]
    Exists { path: PathBuf },
    #[error("cannot read {}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {}", .path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is {size} bytes, more than the {max} bytes such a file may have: it is not a FloraStack file", .path.display())]
    TooLarge { path: PathBuf, size: u64, max: u64 },
    #[error("the project file {} is damaged or is not a FloraStack project file", .path.display())]
    Damaged {
        path: PathBuf,
        #[source]
        source: SchemaError,
    },
    #[error(
        "{} was saved by a newer version of FloraStack (project format {found}; this version reads formats up to {supported}): \
         update FloraStack to open it",
        .path.display()
    )]
    Newer { path: PathBuf, found: u64, supported: u32 },
    #[error("cannot upgrade the project file {} from format {from} to the current one", .path.display())]
    Migration {
        path: PathBuf,
        from: u32,
        #[source]
        source: MigrationError,
    },
    #[error("the project {} is inconsistent, so it can't be opened or saved", .path.display())]
    Invalid {
        path: PathBuf,
        #[source]
        source: Problem,
    },
    #[error("{} is open in another FloraStack window or process: close it there first", .path.display())]
    InUse { path: PathBuf },
    #[error("{} was opened read-only, so it can't be changed", .path.display())]
    ReadOnly { path: PathBuf },
    #[error("the cached file {} can't be used, so what it holds must be computed again", .path.display())]
    Cache {
        path: PathBuf,
        #[source]
        source: CacheProblem,
    },
    #[error("{name:?} can't name a cache file: use 1 to 64 letters, digits, '-' and '_', not starting with '-'")]
    CacheName { name: String },
    #[error("a cache key of {len} bytes is too long (512 at most) or holds control characters")]
    CacheKey { len: usize },
    #[error("the path {} isn't valid Unicode, which a project file can't store: rename the file or folder", .path.display())]
    Path { path: PathBuf },
    #[error("the project name can't be used: {reason}")]
    Name { reason: &'static str },
    #[error("cannot use the preset {}", .path.display())]
    Preset {
        path: PathBuf,
        #[source]
        source: PresetError,
    },
    #[error("the {what} stored in the project can't be converted")]
    Data {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("cannot move {} to the trash; it was left where it is", .path.display())]
    Trash {
        path: PathBuf,
        #[source]
        source: TrashError,
    },
    #[error("{} was not moved to the trash: {reason}", .path.display())]
    NotOrphan { path: PathBuf, reason: &'static str },
}

/// Why the bytes of a project file aren't a project.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("it is not valid JSON")]
    Syntax(#[source] serde_json::Error),
    #[error("it doesn't start with \"format\": \"florastack-project\"")]
    NotAProject,
    #[error("its \"version\" is missing or isn't a whole number")]
    NoVersion,
    #[error("it is in project format {found}, newer than the formats up to {supported} that this version reads")]
    Newer { found: u64, supported: u32 },
    #[error("it can't be upgraded from format {from}")]
    Migration {
        from: u32,
        #[source]
        source: MigrationError,
    },
    #[error("its contents don't match project format {version}")]
    Content {
        version: u32,
        #[source]
        source: serde_json::Error,
    },
    #[error("it is inconsistent")]
    Invalid(#[source] Problem),
    #[error("it can't be written as JSON")]
    Encode(#[source] serde_json::Error),
}

/// Where and why an older project file couldn't be upgraded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("at {at}: expected {expected}")]
pub struct MigrationError {
    /// A JSON pointer to the value, such as `/frames/3/sha256`.
    pub at: String,
    pub expected: &'static str,
}

/// An inconsistency in a project, found before it is saved and after it is read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Problem {
    #[error("the id {id} is used twice")]
    DuplicateId { id: u64 },
    #[error("the id {id} is not below the next free id, {next}")]
    IdNotAllocated { id: u64, next: u64 },
    #[error("an id is 0, which is never given out")]
    ZeroId,
    #[error("the {what} {reason}")]
    Text { what: &'static str, reason: &'static str },
    #[error("a number in the {what} is not finite")]
    NotFinite { what: &'static str },
    #[error("output {output} is made from output {input}, which isn't older than it")]
    OutputOrder { output: u64, input: u64 },
    #[error("the preference {key} {reason}")]
    Preference { key: String, reason: &'static str },
}

/// What is wrong with a cache file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CacheProblem {
    #[error("it is missing")]
    Missing,
    #[error("it is not a regular file")]
    NotAFile,
    #[error("it has {found} bytes instead of {expected}")]
    Size { expected: u64, found: u64 },
    #[error("its contents changed (hash {found} instead of {expected})")]
    Hash { expected: ContentHash, found: ContentHash },
}

/// Why a preset file can't be used.
#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("it is not a valid preset")]
    Syntax(#[source] toml::de::Error),
    #[error("it is in preset format {found}, newer than the formats up to {supported} that this version reads: update FloraStack")]
    Newer { found: i64, supported: u32 },
    #[error("its preset format {found} is not a format number")]
    Format { found: i64 },
    #[error("the preference {key} {reason}")]
    Value { key: String, reason: &'static str },
    #[error("it is {size} bytes, more than the {max} bytes a preset may have")]
    TooLarge { size: u64, max: u64 },
    #[error("it can't be written as TOML")]
    Encode(#[source] toml::ser::Error),
}
