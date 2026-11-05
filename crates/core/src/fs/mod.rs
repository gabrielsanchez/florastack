//! The file system, in a browser too.
//!
//! On native targets this is the standard library's `std::fs`, with a few helpers for what `Path` and `std::env` do
//! there: [`path_exists`], [`path_is_file`], [`path_is_dir`] and [`temp_dir`]. A browser page has no file system (the
//! standard library's calls fail there, and `std::env::temp_dir` panics), so the browser build's `fs` is an in-memory
//! one, [`MemoryFs`], with the same functions and types: it holds the files the page was given (dropped or picked),
//! the temporary projects with their caches, and the exports until they are downloaded, for as long as the page is
//! open. Spilled tiles don't go there but to the browser's origin-private file system ([`crate::TempFileStore`]).
//!
//! The crates that may run in the browser use this module rather than `std::fs` and `Path`'s file-system methods.

#[cfg(any(target_arch = "wasm32", test))]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code, reason = "the browser build's; native targets only test it"))]
mod memory;

#[cfg(any(target_arch = "wasm32", test))]
pub use memory::MemoryFs;

#[cfg(not(target_arch = "wasm32"))]
pub use std::fs::*;

#[cfg(target_arch = "wasm32")]
pub use memory::global::*;
#[cfg(target_arch = "wasm32")]
pub use memory::{DirEntry, File, FileType, Metadata, OpenOptions, ReadDir};
// The standard library's error type for `File::try_lock`, which the in-memory files return too.
#[cfg(target_arch = "wasm32")]
pub use std::fs::TryLockError;

use std::path::{Path, PathBuf};

/// Whether something is at `path` ([`Path::exists`]).
pub fn path_exists(path: &Path) -> bool {
    metadata(path).is_ok()
}

/// Whether `path` is a file, following links ([`Path::is_file`]).
pub fn path_is_file(path: &Path) -> bool {
    metadata(path).is_ok_and(|m| m.is_file())
}

/// Whether `path` is a folder, following links ([`Path::is_dir`]).
pub fn path_is_dir(path: &Path) -> bool {
    metadata(path).is_ok_and(|m| m.is_dir())
}

/// `path` made absolute, from the current folder ([`std::path::absolute`]); in the browser's in-memory file system,
/// from `/`, its only folder to start from (where the standard library's fails).
pub fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::path::absolute(path)
    }
    #[cfg(target_arch = "wasm32")]
    {
        Ok(memory::normal(path))
    }
}

/// The folder for temporary files: the system's ([`std::env::temp_dir`]), or `/tmp` in the browser's in-memory file
/// system, created when first asked for.
pub fn temp_dir() -> PathBuf {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::temp_dir()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let dir = PathBuf::from("/tmp");
        let _ = create_dir_all(&dir);
        dir
    }
}

/// The bytes the in-memory file system holds, in the browser build; `None` on native targets, whose files aren't in
/// memory.
pub fn bytes_in_memory() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
    #[cfg(target_arch = "wasm32")]
    {
        Some(memory::global::stored_bytes())
    }
}
