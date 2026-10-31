//! The project model and its storage: the ordered frame list, the develop settings and their resolution, the
//! alignment results, the outputs with their parameters, cache references and retouch layers, and every processing
//! preference, saved as a versioned project folder whose cache of lossless intermediates lets a reopened project be
//! retouched without recomputing anything; plus layered preference presets and the cleanup of old temporary
//! projects. It may depend only on `florastack-core`.
//!
//! - [`model`]: [`Project`] and its parts ([`Frame`], [`Alignment`], [`Output`], [`RetouchLayer`], ...).
//! - [`edit`]: [`Edit`], a change to a project as a value; [`Project::apply`] makes it and returns the edit that undoes
//!   it, which the engine's undo history keeps.
//! - [`folder`]: [`ProjectFolder`], the `<name>.florastack/` folder with `project.json`, `cache/` and `previews/`,
//!   its lock, atomic saves and the cache's files.
//! - [`schema`]: the format of `project.json`, its versions and the migrations from older ones.
//! - [`cache`]: [`CacheRef`], how project.json refers to a cache file (path, size, content hash), [`CacheWriter`],
//!   which writes one, and [`CacheDir`], which reads and writes them from any thread.
//! - [`prefs`]: [`Preferences`] by [`Group`], and TOML [`Preset`] files that hold any subset of them and layer.
//! - [`orphans`]: finding old unnamed temporary projects and moving them to the [`Trash`] ([`trash`]).
//!
//! # What this crate owns, and what it leaves to others
//!
//! A project holds things that other crates define: the EXIF summary (florastack-codecs), the develop settings and
//! their resolution (florastack-raw), the alignment and stacking settings and an output's parameters (florastack-align
//! and florastack-stack) and the retouch strokes (florastack-retouch, which depends on this crate). This crate stores
//! each of them as JSON data ([`Value`]) in a place of its own in the model, and never interprets it; the engine, which
//! depends on every crate, converts between that data and the crates' types (`florastack_engine::project`). So the
//! layering stays as it is, with no cycle through florastack-retouch, and this crate builds for every target without
//! LightCraft. The JSON is versioned with the project file: the migrations ([`schema`]) work on the JSON tree and can
//! rewrite any part of it, and the engine's tests pin the shape of every type stored this way.
//!
//! Files work the same way. This crate owns the project folder's layout, the cache files' names and references,
//! their hashes and their integrity checks, and the project file's schema; it reads and writes bytes. The engine
//! encodes the pixels (lossless TIFF for intermediates, JPEG for previews) and decodes them, with florastack-codecs.
//!
//! # Never crash
//!
//! A project file is untrusted input: it is read only up to [`schema::MAX_FILE_BYTES`], parsed with serde (whose
//! nesting limit stops a stack overflow), refused when it is from a newer version, upgraded when it is older, and
//! checked ([`Project::validate`]) before anything uses it. Cache paths can't leave the project folder, and a cache
//! file's size and hash are checked before its bytes are used.

pub mod cache;
pub mod edit;
pub mod error;
pub mod folder;
mod fsutil;
pub mod hash;
pub mod model;
pub mod orphans;
pub mod prefs;
pub mod schema;
pub mod time;
pub mod trash;

pub use cache::{Area, CacheDir, CachePath, CacheRef, CacheWriter};
pub use edit::{AlignedFrame, Aspects, Edit, EditError};
pub use error::{CacheProblem, MigrationError, PresetError, Problem, ProjectError, SchemaError};
pub use folder::{FrameState, MoveFailed, ProjectFolder, folder_name};
pub use hash::ContentHash;
pub use model::{
    Alignment, AlignmentRun, Develop, Frame, FrameFile, FrameId, Input, LayerId, MapName, Maps, NewOutput, Output, OutputId, Project, RetouchLayer,
    TRUSTED_AGE, from_data, to_data,
};
pub use orphans::{Orphan, Orphans};
pub use prefs::{Group, Preferences, Preset, Table};
/// The JSON data that holds what other crates define.
pub use serde_json::{Map, Value};
pub use time::Timestamp;
#[cfg(not(target_arch = "wasm32"))]
pub use trash::SystemTrash;
pub use trash::{FolderTrash, Trash, TrashError};
