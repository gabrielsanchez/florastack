//! Lock files: an exclusive lock on a file that the operating system releases when the process that holds it ends,
//! whether it exits or crashes. A folder that FloraStack is using holds one, so that a later run can tell a folder in
//! use from one a crashed run left behind: if the lock can be taken, no running process holds it.
//!
//! The locks are advisory, through the standard library's [`File::try_lock`]: `flock` on Unix, `LockFileEx` on
//! Windows. Both lock per open file, so a second lock on the same file fails within one process too.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// An exclusive lock on a file, held until it is dropped.
#[derive(Debug)]
pub struct LockFile {
    /// The open handle, which holds the lock.
    file: File,
    path: PathBuf,
}

/// Whether a lock file is held, from [`LockFile::probe`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockState {
    /// No lock file.
    Missing,
    /// The file exists and nobody holds its lock.
    Free,
    /// A running process holds it (this one, possibly).
    Held,
}

impl LockFile {
    /// Takes the lock at `path`, creating the file if it doesn't exist; `None` when someone else holds it. An error
    /// means the lock couldn't be tried at all, as on file systems without locks.
    pub fn try_acquire(path: &Path) -> std::io::Result<Option<LockFile>> {
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(LockFile { file, path: path.to_path_buf() })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }

    /// Whether the lock at `path` is held, without creating the file or keeping the lock.
    pub fn probe(path: &Path) -> std::io::Result<LockState> {
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(LockState::Missing),
            Err(e) => return Err(e),
        };
        match file.try_lock() {
            // Dropping the file releases the lock at once.
            Ok(()) => Ok(LockState::Free),
            Err(TryLockError::WouldBlock) => Ok(LockState::Held),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }

    /// The lock file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sets the lock file's modification time to now, to record when its folder was last used.
    pub fn touch(&self) -> std::io::Result<()> {
        self.file.set_modified(std::time::SystemTime::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_exclusive_until_dropped() {
        let dir = std::env::temp_dir().join(format!("florastack-core-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".lock");
        assert_eq!(LockFile::probe(&path).unwrap(), LockState::Missing);
        let lock = LockFile::try_acquire(&path).unwrap().expect("nobody holds it");
        assert_eq!(lock.path(), path);
        lock.touch().unwrap();
        assert_eq!(LockFile::probe(&path).unwrap(), LockState::Held);
        assert!(LockFile::try_acquire(&path).unwrap().is_none(), "a second handle can't take it");
        drop(lock);
        assert_eq!(LockFile::probe(&path).unwrap(), LockState::Free);
        let again = LockFile::try_acquire(&path).unwrap();
        assert!(again.is_some());
        drop(again);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
