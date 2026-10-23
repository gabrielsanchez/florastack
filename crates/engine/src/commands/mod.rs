//! FloraStack's commands, by area (the registry's module documentation says how commands are made and named):
//!
//! - [`project`]: creating, opening, saving and closing projects, their name, and the cleanup of old temporary
//!   projects (`project.new`, `project.open`, `project.save`, `project.close`, `project.info`, `project.rename`,
//!   `project.findOrphans`, `project.trashOrphans`);
//! - [`frames`]: the frame list (`project.addFrames`, a job, `project.removeFrames`, `project.moveFrames`,
//!   `project.setFramesEnabled`, `project.frames`);
//! - [`filmstrip`]: every frame's previews, as shot and aligned, with what moved in it (`filmstrip.list`), and making
//!   the ones missing or out of date (`filmstrip.update`, a preview job);
//! - [`develop`]: the RAW develop settings (`develop.get`, `develop.set`, `develop.reset`);
//! - [`align`]: the alignment settings (`align.get`, `align.set`, `align.reset`), aligning the stack and sorting it by
//!   scale (`align.run` and `align.sortByScale`, jobs), and the alignment the project holds (`align.info`);
//! - [`defects`]: the stack's defect mask, the dust and hot pixels that alignment inpaints (`defects.detect` and
//!   `defects.import`, jobs, `defects.export`, a job, `defects.clear`, `defects.info`);
//! - [`preset`]: preference presets (`preset.apply`, `preset.save`, `preset.read`);
//! - [`output`]: the outputs (`output.list`, `output.rename`, `output.remove`, and `output.save`, a job);
//! - [`edit`]: undo and redo (`edit.undo`, `edit.redo`, `edit.history`);
//! - [`job`]: the jobs (`job.list`, `job.cancel`).
//!
//! Stacking and retouching add their areas in their phases (`stack.*`, `retouch.*`).

// Every command function has the signature the registry's constructors take: its parameters by value (most commands
// consume them) and a `Result` (most can fail), so that commands are written alike.
#![allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]

pub mod align;
pub mod defects;
pub mod develop;
pub mod edit;
pub mod filmstrip;
pub mod frames;
pub mod job;
pub mod output;
pub mod preset;
pub mod project;

use std::path::{Path, PathBuf};

use crate::error::{CommandError, Invalid};
use crate::registry::CommandSpec;

/// Every command of this crate.
pub(crate) fn all() -> Vec<CommandSpec> {
    let mut all = Vec::new();
    all.extend(project::specs());
    all.extend(frames::specs());
    all.extend(filmstrip::specs());
    all.extend(develop::specs());
    all.extend(align::specs());
    all.extend(defects::specs());
    all.extend(preset::specs());
    all.extend(output::specs());
    all.extend(edit::specs());
    all.extend(job::specs());
    all
}

/// Old names of renamed commands, with their new ones. None yet.
pub(crate) const ALIASES: &[(&str, &str)] = &[];

/// The longest path a parameter may hold, in bytes.
pub(crate) const MAX_PATH_BYTES: usize = 32 * 1024;
/// The longest name (of a project, an output, a preset), in characters.
pub(crate) const MAX_NAME_CHARS: usize = 200;
/// The most items in a list parameter (frames, paths).
pub(crate) const MAX_ITEMS: usize = 100_000;

/// Checks a path parameter: not empty, not too long, no NUL.
pub(crate) fn check_path(field: &str, path: &str) -> Result<(), Invalid> {
    if path.trim().is_empty() {
        return Err(Invalid::new(field, "is empty"));
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(Invalid::new(field, "is longer than 32 KiB"));
    }
    if path.contains('\0') {
        return Err(Invalid::new(field, "holds a NUL character"));
    }
    Ok(())
}

/// Checks a name parameter as the project checks names: not blank, at most 200 characters, no control characters.
pub(crate) fn check_name(field: &str, name: &str) -> Result<(), Invalid> {
    if name.trim().is_empty() {
        return Err(Invalid::new(field, "is empty"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(Invalid::new(field, "is longer than 200 characters"));
    }
    if name.chars().any(char::is_control) {
        return Err(Invalid::new(field, "holds control characters"));
    }
    Ok(())
}

/// Checks a list parameter's length.
pub(crate) fn check_len<T>(field: &str, list: &[T], min: usize) -> Result<(), Invalid> {
    if list.len() < min {
        return Err(Invalid::new(field, if min == 1 { "is empty".to_owned() } else { format!("has fewer than {min} items") }));
    }
    if list.len() > MAX_ITEMS {
        return Err(Invalid::new(field, "has more than 100000 items"));
    }
    Ok(())
}

/// Checks that a list has no item twice.
pub(crate) fn check_unique<T: Ord>(field: &str, list: &[T]) -> Result<(), Invalid> {
    let mut seen = std::collections::BTreeSet::new();
    if list.iter().all(|item| seen.insert(item)) { Ok(()) } else { Err(Invalid::new(field, "lists an item twice")) }
}

/// A path parameter made absolute (relative to the current folder).
pub(crate) fn absolute(path: &str) -> Result<PathBuf, CommandError> {
    std::path::absolute(Path::new(path))
        .map_err(|source| CommandError::from(florastack_project::ProjectError::Read { path: PathBuf::from(path), source }))
}

/// A path as text, for results.
pub(crate) fn display(path: &Path) -> String {
    path.display().to_string()
}

/// An error and its causes, in one line.
pub(crate) fn reason(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
