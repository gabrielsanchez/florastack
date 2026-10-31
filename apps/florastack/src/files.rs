//! Files that come to the window: what a drop or a paste holds ([`Dropped`], [`paths_from_text`]), when to autosave
//! ([`Autosave`]), and which windowing system to run on ([`use_x11`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use florastack_engine::branding::PROJECT_EXTENSION;

/// What files dropped or pasted onto the window are for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// A project folder (or its `project.json`) to open.
    Project(PathBuf),
    /// Image and RAW files, and folders of them, to add as frames: the engine reads the ones it can.
    Frames(Vec<PathBuf>),
}

/// Whether `path` is a project: a `.florastack` folder, or the `project.json` in one.
fn is_project(path: &Path) -> bool {
    let folder = path.is_dir() && path.extension().and_then(|e| e.to_str()).is_some_and(|e| PROJECT_EXTENSION.trim_start_matches('.') == e);
    let file = path.file_name().is_some_and(|n| n == "project.json") && path.parent().is_some_and(is_project);
    folder || file
}

impl Dropped {
    /// What `paths` are for: the first project among them, or else all of them as frames; `None` for none.
    pub fn of(paths: Vec<PathBuf>) -> Option<Dropped> {
        if let Some(project) = paths.iter().find(|p| is_project(p)) {
            let folder = if project.is_file() { project.parent().unwrap_or(project) } else { project };
            return Some(Dropped::Project(folder.to_path_buf()));
        }
        (!paths.is_empty()).then_some(Dropped::Frames(paths))
    }
}

/// `%XX` escapes decoded, as in a `file://` URI; text that isn't UTF-8 once decoded is kept as it was.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |j: usize| bytes.get(j).and_then(|&c| (c as char).to_digit(16));
        if let (b'%', Some(hi), Some(lo)) = (b, hex(i + 1), hex(i + 2)) {
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// The paths in pasted text that exist: one a line, plain or quoted, or as `file://` URIs (as file managers copy
/// them on Linux and macOS). A file copied in Windows Explorer isn't text, so it can't be pasted: drop it instead.
pub fn paths_from_text(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(|l| l.trim().trim_matches('"').trim_matches('\''))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.strip_prefix("file://") {
            // `file:///C:/x` on Windows, `file:///home/x` elsewhere; a host part (`file://server/x`) is kept.
            Some(rest) => {
                let decoded = percent_decoded(rest);
                let windows_drive = decoded.len() > 2 && decoded.starts_with('/') && decoded.as_bytes().get(2) == Some(&b':');
                PathBuf::from(if windows_drive { decoded[1..].to_owned() } else { decoded })
            }
            None => PathBuf::from(l),
        })
        .filter(|p| p.exists())
        .collect()
}

/// When to save a project on its own: a while after its first unsaved change, when no job is running (a job's
/// result changes it again soon). See [`Autosave::due`].
#[derive(Clone, Debug)]
pub struct Autosave {
    interval: Duration,
    /// When the project first had unsaved changes since it was last saved.
    since: Option<Instant>,
}

impl Autosave {
    pub fn new(interval: Duration) -> Autosave {
        Autosave { interval, since: None }
    }

    /// Whether to save now, given whether the project has unsaved changes and whether a job is running.
    pub fn due(&mut self, now: Instant, dirty: bool, busy: bool) -> bool {
        if !dirty {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if busy || now.duration_since(since) < self.interval {
            return false;
        }
        self.since = None;
        true
    }
}

/// Whether to run through X11 rather than Wayland (Linux and the BSDs): winit has no file drag-and-drop on Wayland,
/// so the app runs through XWayland where there is an X server (`DISPLAY` set), unless `FLORASTACK_WAYLAND` asks for
/// native Wayland. LightCraft's desktop app does the same.
pub fn use_x11(display: Option<&std::ffi::OsStr>, force_wayland: Option<&std::ffi::OsStr>) -> bool {
    display.is_some_and(|d| !d.is_empty()) && force_wayland.is_none_or(std::ffi::OsStr::is_empty)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn x11_unless_wayland_is_asked_for_or_there_is_no_x_server() {
        assert!(use_x11(Some(OsStr::new(":0")), None));
        assert!(!use_x11(Some(OsStr::new(":0")), Some(OsStr::new("1"))));
        assert!(use_x11(Some(OsStr::new(":0")), Some(OsStr::new(""))), "an empty variable is unset");
        assert!(!use_x11(None, None), "no X server: Wayland");
    }

    #[test]
    fn autosave_waits_for_its_interval_and_for_jobs() {
        let start = Instant::now();
        let mut autosave = Autosave::new(Duration::from_secs(10));
        assert!(!autosave.due(start, false, false), "nothing to save");
        assert!(!autosave.due(start, true, false), "the first change starts the wait");
        assert!(!autosave.due(start + Duration::from_secs(9), true, false));
        assert!(!autosave.due(start + Duration::from_secs(11), true, true), "not while a job runs");
        assert!(autosave.due(start + Duration::from_secs(12), true, false));
        assert!(!autosave.due(start + Duration::from_secs(13), true, false), "saved: the wait starts again");
        assert!(!autosave.due(start + Duration::from_secs(14), false, false));
    }

    #[test]
    fn pasted_text_gives_the_paths_that_exist() {
        let dir = std::env::temp_dir().join(format!("florastack-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b.tif");
        std::fs::write(&file, b"x").unwrap();
        let uri = format!("file://{}{}", if cfg!(windows) { "/" } else { "" }, file.display().to_string().replace('\\', "/").replace(' ', "%20"));
        let text = format!("{}\n\"{}\"\n{uri}\n/no/such/file\n", file.display(), file.display());
        let paths = paths_from_text(&text);
        assert_eq!(paths.len(), 3, "{paths:?}");
        assert!(paths.iter().all(|p| p.is_file()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_project_among_the_dropped_files_is_opened() {
        let dir = std::env::temp_dir().join(format!("florastack-drop-{}", std::process::id()));
        let project = dir.join(format!("Bee{PROJECT_EXTENSION}"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("project.json"), b"{}").unwrap();
        let photo = dir.join("a.tif");
        assert_eq!(Dropped::of(vec![photo.clone(), project.join("project.json")]), Some(Dropped::Project(project.clone())));
        assert_eq!(Dropped::of(vec![photo.clone()]), Some(Dropped::Frames(vec![photo])));
        assert_eq!(Dropped::of(Vec::new()), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
