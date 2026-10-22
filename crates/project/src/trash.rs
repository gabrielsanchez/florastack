//! Moving things to the trash instead of deleting them: [`Trash`], with the operating system's trash
//! ([`SystemTrash`], on native targets) and a folder that stands in for it ([`FolderTrash`]), for tests and for
//! systems without a trash.
//!
//! Orphan cleanup ([`crate::orphans`]) takes a `&dyn Trash`, so that its tests never put anything in the real Recycle
//! Bin of the machine they run on: they use [`FolderTrash`]. Nothing in this crate deletes a project.
//!
//! [`SystemTrash`] is the one place FloraStack touches the `trash` crate: the shell's `IFileOperation` on Windows
//! (through the `windows` crate), `NSFileManager` on macOS (through objc2) and the freedesktop.org trash specification
//! on Linux and the BSDs. No C code is built for it; the libraries it calls are the operating systems' own.

use std::fmt;
use std::path::{Path, PathBuf};

/// Somewhere to move files and folders that the user may still want back.
pub trait Trash {
    /// Moves the file or folder at `path` to the trash. An implementation must never delete anything instead: when it
    /// can't move `path`, it returns an error and leaves `path` where it is.
    fn trash(&self, path: &Path) -> Result<(), TrashError>;
}

/// Why something couldn't be moved to the trash.
pub struct TrashError(Kind);

enum Kind {
    #[cfg(not(target_arch = "wasm32"))]
    System(::trash::Error),
    Io(std::io::Error),
    /// The trash library panicked (caught on its own thread).
    #[cfg(not(target_arch = "wasm32"))]
    Panicked,
}

impl fmt::Display for TrashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            #[cfg(not(target_arch = "wasm32"))]
            Kind::System(e) => write!(f, "the system trash failed: {e}"),
            Kind::Io(e) => write!(f, "{e}"),
            #[cfg(not(target_arch = "wasm32"))]
            Kind::Panicked => f.write_str("the system trash failed unexpectedly"),
        }
    }
}

impl fmt::Debug for TrashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            #[cfg(not(target_arch = "wasm32"))]
            Kind::System(e) => f.debug_tuple("TrashError::System").field(e).finish(),
            Kind::Io(e) => f.debug_tuple("TrashError::Io").field(e).finish(),
            #[cfg(not(target_arch = "wasm32"))]
            Kind::Panicked => f.write_str("TrashError::Panicked"),
        }
    }
}

impl std::error::Error for TrashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            #[cfg(not(target_arch = "wasm32"))]
            Kind::System(e) => Some(e),
            Kind::Io(e) => Some(e),
            #[cfg(not(target_arch = "wasm32"))]
            Kind::Panicked => None,
        }
    }
}

impl From<std::io::Error> for TrashError {
    fn from(e: std::io::Error) -> TrashError {
        TrashError(Kind::Io(e))
    }
}

/// The operating system's trash: the Recycle Bin on Windows, the Trash on macOS and the freedesktop.org trash on Linux,
/// where the user can restore what was moved there.
///
/// Each call runs on a thread of its own, so that the Windows implementation initializes COM on a thread where nobody
/// else has (a UI toolkit may have chosen another threading model on the caller's), and so that a panic in the library
/// becomes an error. On macOS it uses `NSFileManager`, which needs no permission to control Finder.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTrash;

#[cfg(not(target_arch = "wasm32"))]
impl Trash for SystemTrash {
    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        let path = path.to_path_buf();
        let worker = std::thread::Builder::new().name("florastack-trash".into()).spawn(move || {
            #[cfg(target_os = "macos")]
            let context = {
                use ::trash::macos::{DeleteMethod, TrashContextExtMacos};
                let mut context = ::trash::TrashContext::default();
                context.set_delete_method(DeleteMethod::NsFileManager);
                context
            };
            #[cfg(not(target_os = "macos"))]
            let context = ::trash::TrashContext::default();
            context.delete(&path)
        })?;
        match worker.join() {
            Ok(result) => result.map_err(|e| TrashError(Kind::System(e))),
            Err(_) => Err(TrashError(Kind::Panicked)),
        }
    }
}

/// A trash that is a folder: everything moved to it keeps its name (with a number added when the name is taken).
/// Tests use it in place of [`SystemTrash`]; it can also serve where there is no system trash. It renames, so the
/// folder must be on the same volume as what is moved there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderTrash {
    dir: PathBuf,
}

impl FolderTrash {
    pub fn new(dir: impl Into<PathBuf>) -> FolderTrash {
        FolderTrash { dir: dir.into() }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Trash for FolderTrash {
    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        std::fs::create_dir_all(&self.dir)?;
        let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "nothing to move"))?;
        // A free name: renaming onto an existing empty folder would replace it on some systems.
        for n in 0..10_000u32 {
            let mut candidate = name.to_os_string();
            if n > 0 {
                candidate.push(format!(" ({n})"));
            }
            let target = self.dir.join(candidate);
            if std::fs::symlink_metadata(&target).is_err() {
                std::fs::rename(path, &target)?;
                return Ok(());
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name in the trash folder").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_trash_moves_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!("florastack-project-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = FolderTrash::new(root.join("bin"));
        for content in [&b"first"[..], b"second"] {
            let dir = root.join("old.florastack");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("project.json"), content).unwrap();
            bin.trash(&dir).unwrap();
            assert!(!dir.exists());
        }
        assert_eq!(std::fs::read(bin.path().join("old.florastack").join("project.json")).unwrap(), b"first");
        assert_eq!(std::fs::read(bin.path().join("old.florastack (1)").join("project.json")).unwrap(), b"second");
        let missing = bin.trash(&root.join("missing")).unwrap_err();
        assert!(matches!(&missing.0, Kind::Io(e) if e.kind() == std::io::ErrorKind::NotFound), "{missing:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
