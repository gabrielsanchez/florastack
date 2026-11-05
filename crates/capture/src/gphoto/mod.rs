//! libgphoto2, dynamically loaded at run time: the system's, or on macOS the copy that the app bundle carries. No
//! static link or LGPL Rust dependency.
//!
//! `cargo xtask dist` puts libgphoto2 into the macOS app as separate files (`Contents/Frameworks/libgphoto2.6.dylib`,
//! its drivers in `Contents/PlugIns/libgphoto2/`), which [`Bundled`] finds next to the executable. The library reads
//! its drivers' folders from `CAMLIBS` and `IOLIBS`, or else from the folders it was built for, which don't exist on
//! a user's Mac: [`prepare_bundled_drivers`] sets them. Any user may load another libgphoto2 instead by setting
//! [`LIBRARY_VARIABLE`] to its absolute path, as the LGPL requires (book/src/dev/packaging.md).

mod ffi;
pub use ffi::{Connection, discover};

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The variable that selects another libgphoto2: an absolute path to its shared library.
pub const LIBRARY_VARIABLE: &str = "FLORASTACK_GPHOTO_LIBRARY";

/// libgphoto2's variables for its camera drivers and port drivers.
const DRIVER_VARIABLES: [&str; 2] = ["CAMLIBS", "IOLIBS"];

/// The libgphoto2 inside a macOS app bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundled {
    pub library: PathBuf,
    pub camlibs: PathBuf,
    pub iolibs: PathBuf,
}

impl Bundled {
    /// The libraries of the app bundle whose `Contents/MacOS/` holds `executable`, if it carries them all.
    pub fn beside(executable: &Path) -> Option<Self> {
        let macos = executable.parent()?;
        if macos.file_name()? != "MacOS" {
            return None;
        }
        let contents = macos.parent()?;
        let bundled = Self {
            library: contents.join("Frameworks/libgphoto2.6.dylib"),
            camlibs: contents.join("PlugIns/libgphoto2/camlibs"),
            iolibs: contents.join("PlugIns/libgphoto2/iolibs"),
        };
        (bundled.library.is_file() && bundled.camlibs.is_dir() && bundled.iolibs.is_dir()).then_some(bundled)
    }

    /// The running app's, on macOS, through any symbolic link to its executable.
    pub fn current() -> Option<Self> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        Self::beside(&std::env::current_exe().ok()?.canonicalize().ok()?)
    }
}

/// Points the bundled libgphoto2 at its drivers; call it first thing in `main`, before any thread starts.
///
/// Setting a variable in a running process isn't sound once other threads may read the environment, so the app
/// starts itself again in place (the same process, through `exec`) with `CAMLIBS` and `IOLIBS` set, and this returns
/// only if that failed. It does nothing outside an app bundle that carries libgphoto2, when [`LIBRARY_VARIABLE`]
/// selects another library, or when either driver variable is set already (which also ends the restart).
///
/// # Errors
///
/// When the executable can't be started again; camera control then finds no camera, and nothing else changes.
pub fn prepare_bundled_drivers() -> std::io::Result<()> {
    let Some(bundled) = Bundled::current() else { return Ok(()) };
    match driver_environment(&bundled, |name| std::env::var_os(name)) {
        Some(environment) => restart(&environment),
        None => Ok(()),
    }
}

/// The driver variables to start again with, or `None` when the environment chooses already.
fn driver_environment(bundled: &Bundled, var: impl Fn(&str) -> Option<OsString>) -> Option<[(&'static str, PathBuf); 2]> {
    if var(LIBRARY_VARIABLE).is_some() || DRIVER_VARIABLES.iter().any(|name| var(name).is_some()) {
        return None;
    }
    Some([(DRIVER_VARIABLES[0], bundled.camlibs.clone()), (DRIVER_VARIABLES[1], bundled.iolibs.clone())])
}

#[cfg(target_os = "macos")]
fn restart(environment: &[(&str, PathBuf)]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut arguments = std::env::args_os();
    let mut command = std::process::Command::new(std::env::current_exe()?);
    if let Some(name) = arguments.next() {
        command.arg0(name);
    }
    Err(command.args(arguments).envs(environment.iter().map(|(name, value)| (*name, value))).exec())
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::unnecessary_wraps)] // The macOS version can fail; elsewhere there is no bundle to restart for.
fn restart(_: &[(&str, PathBuf)]) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Folder(PathBuf);
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An app bundle with the given parts of the camera libraries.
    fn bundle(name: &str, library: bool, drivers: bool) -> (Folder, PathBuf) {
        let root = std::env::temp_dir().join(format!("florastack-gphoto-{name}-{}", std::process::id()));
        let contents = root.join("App.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::write(contents.join("MacOS/app"), b"").unwrap();
        if library {
            std::fs::create_dir_all(contents.join("Frameworks")).unwrap();
            std::fs::write(contents.join("Frameworks/libgphoto2.6.dylib"), b"").unwrap();
        }
        if drivers {
            std::fs::create_dir_all(contents.join("PlugIns/libgphoto2/camlibs")).unwrap();
            std::fs::create_dir_all(contents.join("PlugIns/libgphoto2/iolibs")).unwrap();
        }
        (Folder(root), contents.join("MacOS/app"))
    }

    #[test]
    fn a_complete_bundle_is_found_next_to_the_executable() {
        let (folder, executable) = bundle("complete", true, true);
        let bundled = Bundled::beside(&executable).unwrap();
        let contents = folder.0.join("App.app/Contents");
        assert_eq!(bundled.library, contents.join("Frameworks/libgphoto2.6.dylib"));
        assert_eq!(bundled.camlibs, contents.join("PlugIns/libgphoto2/camlibs"));
        assert_eq!(bundled.iolibs, contents.join("PlugIns/libgphoto2/iolibs"));
    }

    #[test]
    fn incomplete_bundles_and_other_folders_are_not() {
        let (_folder, executable) = bundle("no-drivers", true, false);
        assert_eq!(Bundled::beside(&executable), None);
        let (_folder, executable) = bundle("no-library", false, true);
        assert_eq!(Bundled::beside(&executable), None);
        let (folder, _) = bundle("not-in-macos", true, true);
        assert_eq!(Bundled::beside(&folder.0.join("App.app/Contents/app")), None, "only an executable in Contents/MacOS");
        assert_eq!(Bundled::beside(Path::new("app")), None);
    }

    #[test]
    fn the_drivers_are_set_unless_the_environment_chooses() {
        let bundled = Bundled { library: "/A/l".into(), camlibs: "/A/c".into(), iolibs: "/A/i".into() };
        let none = |_: &str| None;
        assert_eq!(driver_environment(&bundled, none), Some([("CAMLIBS", PathBuf::from("/A/c")), ("IOLIBS", PathBuf::from("/A/i"))]));
        for set in [LIBRARY_VARIABLE, "CAMLIBS", "IOLIBS"] {
            let only = |name: &str| (name == set).then(|| OsString::from("/x"));
            assert_eq!(driver_environment(&bundled, only), None, "{set} set: no restart (and none after one)");
        }
    }
}
