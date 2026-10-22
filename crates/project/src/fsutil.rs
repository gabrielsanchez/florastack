//! File helpers: reading a file of bounded size, and replacing a file atomically (a temporary file in the same folder,
//! flushed to disk, then renamed over the old one), so that a crash or a full disk never leaves a half-written
//! project file or preset.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::ProjectError;

/// Reads `path` whole, refusing a file larger than `max` bytes before reading it.
pub(crate) fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, ProjectError> {
    let read = |source| ProjectError::Read { path: path.to_path_buf(), source };
    let file = std::fs::File::open(path).map_err(read)?;
    let size = file.metadata().map_err(read)?.len();
    if size > max {
        return Err(ProjectError::TooLarge { path: path.to_path_buf(), size, max });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    // One byte past the limit at most: a file that grew since is caught below rather than read whole.
    file.take(max.saturating_add(1)).read_to_end(&mut bytes).map_err(read)?;
    if bytes.len() as u64 > max {
        return Err(ProjectError::TooLarge { path: path.to_path_buf(), size: bytes.len() as u64, max });
    }
    Ok(bytes)
}

/// Replaces `path` with `bytes` atomically.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_file_name(format!(".{name}.{}.{n}.part", std::process::id()));
    let failed = |source| {
        let _ = std::fs::remove_file(&temp);
        ProjectError::Write { path: path.to_path_buf(), source }
    };
    // The file is closed at the end of this block, before it is renamed.
    {
        let mut file = std::fs::File::create(&temp).map_err(|source| ProjectError::Write { path: path.to_path_buf(), source })?;
        file.write_all(bytes).map_err(failed)?;
        file.sync_all().map_err(failed)?;
    }
    std::fs::rename(&temp, path).map_err(failed)?;
    sync_parent(path);
    Ok(())
}

/// Flushes the folder holding `path`, so that a rename into it survives a power cut. Unix only: Windows can't open a
/// folder as a file, and NTFS journals the rename itself.
fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}
