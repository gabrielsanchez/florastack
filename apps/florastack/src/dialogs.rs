//! File dialogs: the system's own (rfd) on the desktop. A browser page can't name the user's folders, so in the
//! browser build a folder to write to is a new folder of the in-memory file system whose files are downloaded once a
//! job has written them (`web.rs`), and the dialogs that read files open the browser's file picker, whose files come
//! to the app once read; the ones that need a real folder (opening a project, the batch's folders) aren't offered.

use std::path::{Path, PathBuf};

/// A file filter: its name and its extensions.
pub type Filter<'a> = (&'a str, &'a [&'a str]);

/// The folder to write to, starting in `start`; `None` when the user cancels.
pub fn write_folder(start: Option<&Path>) -> Option<PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dialog = rfd::FileDialog::new();
        let dialog = if let Some(start) = start { dialog.set_directory(start) } else { dialog };
        dialog.pick_folder()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = start;
        crate::web::download_folder()
    }
}

/// The file to save as `name`, starting in `start`.
pub fn save_file(name: &str, filter: Filter<'_>, start: Option<&Path>) -> Option<PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dialog = rfd::FileDialog::new().add_filter(filter.0, filter.1).set_file_name(name);
        let dialog = if let Some(start) = start { dialog.set_directory(start) } else { dialog };
        dialog.save_file()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (filter, start);
        Some(crate::web::download_folder()?.join(name))
    }
}

/// A folder to read from (a project, a folder of stacks, the batch's), on the desktop only.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_folder(start: Option<&Path>) -> Option<PathBuf> {
    let dialog = rfd::FileDialog::new();
    let dialog = if let Some(start) = start { dialog.set_directory(start) } else { dialog };
    dialog.pick_folder()
}

/// Files to read, starting in `start`, on the desktop only (the browser build's picker answers later: `web::pick`).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_files(start: Option<&Path>) -> Option<Vec<PathBuf>> {
    let dialog = rfd::FileDialog::new();
    let dialog = if let Some(start) = start { dialog.set_directory(start) } else { dialog };
    dialog.pick_files()
}

/// A file to read, on the desktop only.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_file(filter: Filter<'_>) -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter(filter.0, filter.1).pick_file()
}

/// Whether this build has the user's file system: what the dialogs that need a real folder are offered for.
pub const NATIVE_FILES: bool = cfg!(not(target_arch = "wasm32"));
