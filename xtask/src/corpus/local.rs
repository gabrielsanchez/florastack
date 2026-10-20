//! Local stacks: `corpus/local/<name>/`, a folder of frames with a `stack.toml` that describes them. They are the
//! user's own and never downloaded, verified against a hash or changed by xtask.

use std::path::Path;

use super::manifest::{StackToml, is_file_name};

/// Extensions (lower case) of the files taken as frames when `stack.toml` doesn't list them.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "exr", "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "rw2", "rwl", "orf",
    "pef", "srw", "3fr", "iiq",
];

/// A local stack, read and checked.
#[derive(Debug)]
pub struct LocalStack {
    pub name: String,
    pub info: StackToml,
    /// The frame file names, in focus order.
    pub frames: Vec<String>,
}

/// A folder of `corpus/local/`: its stack, or its name and the reason it isn't one.
pub type Entry = Result<LocalStack, (String, String)>;

/// Every folder in `local`, in name order, as a stack or the reason it isn't one. A missing `local` folder has no
/// stacks; folders whose names start with a dot are skipped.
pub fn load_all(local: &Path) -> Result<Vec<Entry>, String> {
    let entries = match std::fs::read_dir(local) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", local.display())),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", local.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_ok_and(|t| t.is_dir()) && !name.starts_with('.') {
            dirs.push((name, entry.path()));
        }
    }
    dirs.sort();
    Ok(dirs.into_iter().map(|(name, dir)| load(&name, &dir).map_err(|problem| (name, problem))).collect())
}

/// The stack in `dir`.
pub fn load(name: &str, dir: &Path) -> Result<LocalStack, String> {
    let toml_path = dir.join("stack.toml");
    let text = match std::fs::read_to_string(&toml_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("no stack.toml: add one with at least `subject` and `frame_order` (see corpus/README.md)".to_owned());
        }
        Err(e) => return Err(format!("cannot read stack.toml: {e}")),
    };
    let info: StackToml = toml::from_str(&text).map_err(|e| format!("stack.toml: {e}"))?;
    let reference = info.ground_truth.as_ref().and_then(|truth| truth.reference.clone());
    let is_file = |file: &str| is_file_name(file) && std::fs::symlink_metadata(dir.join(file)).is_ok_and(|meta| meta.is_file());
    if let Some(reference) = &reference
        && !is_file(reference)
    {
        return Err(format!("the ground-truth reference `{reference}` is not a file in the folder"));
    }
    let frames = if let Some(listed) = &info.frames {
        if let Some(missing) = listed.iter().find(|frame| !is_file(frame)) {
            return Err(format!("stack.toml lists the frame `{missing}`, which is not a file in the folder"));
        }
        let mut seen = std::collections::BTreeSet::new();
        if let Some(twice) = listed.iter().find(|frame| !seen.insert(frame.to_ascii_lowercase())) {
            return Err(format!("stack.toml lists the frame `{twice}` twice"));
        }
        listed.clone()
    } else {
        {
            let mut found = Vec::new();
            let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read the folder: {e}"))?;
            for entry in entries {
                let entry = entry.map_err(|e| format!("cannot read the folder: {e}"))?;
                let file = entry.file_name().to_string_lossy().into_owned();
                let is_image = Path::new(&file)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
                if is_image && entry.file_type().is_ok_and(|t| t.is_file()) && reference.as_deref() != Some(file.as_str()) {
                    found.push(file);
                }
            }
            found.sort();
            found
        }
    };
    if frames.is_empty() {
        return Err(format!("no frames: put the images in the folder (extensions: {}) or list them in `frames`", IMAGE_EXTENSIONS.join(", ")));
    }
    Ok(LocalStack { name: name.to_owned(), info, frames })
}
