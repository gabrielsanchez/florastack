//! The viewer's display transform for the monitor it is on ([`Choice`], [`decide`], [`Follower`]): the display's ICC
//! profile as the system has it (Automatic), sRGB, or a profile file the user picks, made into the table the
//! viewer's shader applies after sRGB's curve (florastack-engine's `view::display_lut`).
//!
//! - **The system's lookup** ([`display_at`]): the display under a point of the desktop, by the platform's own
//!   interfaces. Windows: the monitor's device and its default profile (`GetICMProfileW`), and whether advanced color
//!   (automatic color management, or HDR) is on, when the compositor converts sRGB windows itself (`windows.rs`).
//!   macOS: the display's color space (`CGDisplayCopyColorSpace`) and its ICC data (`macos.rs`). Linux on X11: the
//!   `_ICC_PROFILE` property that color managers set on the root window, for each monitor by its `RandR` index
//!   (`x11.rs`, no `unsafe`). Wayland and the browser have none: sRGB.
//! - **Applied once** ([`decide`]): where the system converts sRGB windows to the display itself (Windows' advanced
//!   color), the viewer sends sRGB whatever the choice, so that the two transforms never stack. On macOS, the window's
//!   Metal layer is tagged with the space the viewer sends ([`tag_output`]): the display's own in Automatic, so that
//!   Core Animation leaves it as it is, sRGB or the chosen file's otherwise, which Core Animation converts from.
//!   A surface that applies sRGB's curve gets linear values from the shader (`viewer.wgsl`).
//! - **Following the window** ([`Follower`]): the display under the window's center is looked up again when the
//!   window moves to another display or the choice changes, at most twice a second; a profile that is missing or
//!   can't be used falls back to sRGB, and says why.
//!
//! `unsafe` is allowed in `windows.rs` and `macos.rs` alone (AGENTS.md, Never crash): each call into the system says
//! what makes it sound.

use std::path::{Path, PathBuf};

use florastack_engine::view::{DisplayLut, display_lut};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(all(unix, not(target_os = "macos")))]
mod x11;

/// The largest profile file read: florastack-codecs' limit.
const MAX_PROFILE_BYTES: u64 = florastack_engine::view::MAX_DISPLAY_PROFILE_BYTES as u64;

/// Where the viewer's display transform comes from, as Preferences sets it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    /// The profile the system has for the display the window is on.
    #[default]
    Automatic,
    /// sRGB, whatever the display.
    Srgb,
    /// An ICC profile file, for every display.
    File(PathBuf),
}

/// A display, as the system describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Display {
    /// What tells it from the others (its device name), to notice when the window moves to another.
    pub id: String,
    /// Its ICC profile, as the system has it; none when it has none or it can't be read.
    pub profile: Option<Vec<u8>>,
    /// Whether the system converts sRGB windows to the display itself (Windows' advanced color).
    pub system_managed: bool,
}

/// Why the viewer sends sRGB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// sRGB was chosen.
    Chosen,
    /// The system converts sRGB to the display itself.
    SystemManaged,
    /// The display has no profile, or the system can't say which display the window is on.
    NoProfile,
    /// The profile can't be read or used: the reason.
    Unusable(String),
}

/// What the viewer shows through: a profile's table, or sRGB and why.
#[derive(Clone, Debug, PartialEq)]
pub enum Decided {
    Profile {
        lut: DisplayLut,
        /// The profile's ICC bytes, for the window's tag on macOS.
        icc: Vec<u8>,
        /// The profile's description, or the file's name.
        name: String,
    },
    Srgb(Why),
}

impl Decided {
    /// What Preferences says of it.
    pub fn describe(&self) -> String {
        use crate::i18n::{tr, tr_args};
        match self {
            Decided::Profile { name, .. } => tr_args("display-profile-using", &[("name", name.clone().into())]),
            Decided::Srgb(Why::Chosen) => tr("display-srgb-chosen"),
            Decided::Srgb(Why::SystemManaged) => tr("display-srgb-system"),
            Decided::Srgb(Why::NoProfile) => tr("display-srgb-none"),
            Decided::Srgb(Why::Unusable(reason)) => tr_args("display-srgb-unusable", &[("reason", reason.clone().into())]),
        }
    }
}

/// The display transform for `choice` on `display` (none when the system can't say), reading a chosen file with
/// `read`. See the module documentation: the system's own conversion comes first.
pub fn decide(choice: &Choice, display: Option<&Display>, read: impl Fn(&Path) -> std::io::Result<Vec<u8>>) -> Decided {
    if display.is_some_and(|d| d.system_managed) {
        return Decided::Srgb(Why::SystemManaged);
    }
    let (icc, fallback_name) = match choice {
        Choice::Srgb => return Decided::Srgb(Why::Chosen),
        Choice::Automatic => match display.and_then(|d| d.profile.clone()) {
            Some(icc) => (icc, display.map(|d| d.id.clone()).unwrap_or_default()),
            None => return Decided::Srgb(Why::NoProfile),
        },
        Choice::File(path) => match read(path) {
            Ok(icc) => (icc, path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            Err(e) => return Decided::Srgb(Why::Unusable(format!("{}: {e}", path.display()))),
        },
    };
    match display_lut(&icc) {
        Ok(lut) => {
            let name = lut.description.clone().unwrap_or(fallback_name);
            Decided::Profile { lut, icc, name }
        }
        Err(e) => {
            let reason =
                std::iter::successors(Some(&e as &dyn std::error::Error), |e| e.source()).map(ToString::to_string).collect::<Vec<_>>().join(": ");
            Decided::Srgb(Why::Unusable(reason))
        }
    }
}

/// Reads a profile file, refusing one larger than any profile.
pub fn read_profile(path: &Path) -> std::io::Result<Vec<u8>> {
    let size = florastack_engine::fs::metadata(path)?.len();
    if size > MAX_PROFILE_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "larger than any ICC profile"));
    }
    florastack_engine::fs::read(path)
}

/// The display under `point`, the window's center: in physical pixels of the desktop on Windows and X11, in points on
/// macOS. `None` where the system can't say (Wayland, a lookup that failed).
#[cfg(target_os = "windows")]
pub fn display_at(point: [f64; 2]) -> Option<Display> {
    windows::display_at(point)
}

/// See the Windows version.
#[cfg(target_os = "macos")]
pub fn display_at(point: [f64; 2]) -> Option<Display> {
    macos::display_at(point)
}

/// See the Windows version.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn display_at(point: [f64; 2]) -> Option<Display> {
    x11::display_at(point)
}

/// In a browser, the system says nothing of its displays.
#[cfg(not(any(unix, target_os = "windows")))]
pub fn display_at(_point: [f64; 2]) -> Option<Display> {
    None
}

/// Tags the window's output with the space the viewer sends it in, on macOS (see the module documentation): `icc`,
/// or sRGB without one. Nothing elsewhere.
pub fn tag_output(window: &impl raw_window_handle::HasWindowHandle, icc: Option<&[u8]>) {
    #[cfg(target_os = "macos")]
    macos::tag_output(window, icc);
    #[cfg(not(target_os = "macos"))]
    let _ = (window, icc);
}

/// How often the display under the window is looked up at most.
const EVERY: std::time::Duration = std::time::Duration::from_millis(500);

/// Follows the window from display to display. See the module documentation.
#[derive(Default)]
pub struct Follower {
    /// When the display was last looked up, the display's id then and the choice it was decided for.
    checked: Option<florastack_engine::time::Instant>,
    shown: Option<(Option<String>, Choice)>,
    /// What the viewer shows through now.
    pub decided: Option<Decided>,
}

impl Follower {
    /// Looks the display under `point` up again when it is time, and decides again when the display or `choice`
    /// changed: the new decision, to apply, or `None` when nothing changed.
    pub fn follow(&mut self, point: Option<[f64; 2]>, choice: &Choice) -> Option<&Decided> {
        let now = florastack_engine::time::Instant::now();
        let changed_choice = self.shown.as_ref().is_none_or(|(_, c)| c != choice);
        if !changed_choice && self.checked.is_some_and(|t| now.saturating_duration_since(t) < EVERY) {
            return None;
        }
        self.checked = Some(now);
        let display = point.and_then(display_at);
        let id = display.as_ref().map(|d| d.id.clone());
        let key = (id, choice.clone());
        if self.shown.as_ref() == Some(&key) {
            return None;
        }
        self.decided = Some(decide(choice, display.as_ref(), read_profile));
        self.shown = Some(key);
        self.decided.as_ref()
    }

    /// Decides again at the next look, as after a change of the profile file itself.
    pub fn refresh(&mut self) {
        self.shown = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use florastack_engine::view::{StandardDisplay, standard_display_profile};

    fn p3() -> Vec<u8> {
        standard_display_profile(StandardDisplay::DisplayP3).unwrap()
    }

    fn display(profile: Option<Vec<u8>>, system_managed: bool) -> Display {
        Display { id: "DISPLAY1".to_owned(), profile, system_managed }
    }

    fn no_file(_: &Path) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))
    }

    #[test]
    fn automatic_uses_the_display_s_profile_and_srgb_without_one() {
        match decide(&Choice::Automatic, Some(&display(Some(p3()), false)), no_file) {
            Decided::Profile { lut, icc, name } => {
                assert_eq!((lut.size, icc), (florastack_engine::view::LUT_SIZE, p3()));
                assert_ne!(name, "");
            }
            other @ Decided::Srgb(_) => panic!("{other:?}"),
        }
        assert_eq!(decide(&Choice::Automatic, Some(&display(None, false)), no_file), Decided::Srgb(Why::NoProfile));
        assert_eq!(decide(&Choice::Automatic, None, no_file), Decided::Srgb(Why::NoProfile));
        assert_eq!(decide(&Choice::Srgb, Some(&display(Some(p3()), false)), no_file), Decided::Srgb(Why::Chosen));
    }

    #[test]
    fn where_the_system_converts_srgb_itself_the_viewer_sends_srgb_whatever_the_choice() {
        // Windows' advanced color: the compositor converts sRGB windows to the display, so a profile applied by the
        // viewer as well would be applied twice.
        let managed = display(Some(p3()), true);
        let file = |_: &Path| Ok(p3());
        for choice in [Choice::Automatic, Choice::Srgb, Choice::File(PathBuf::from("p3.icc"))] {
            assert_eq!(decide(&choice, Some(&managed), file), Decided::Srgb(Why::SystemManaged), "{choice:?}");
        }
    }

    #[test]
    fn a_chosen_file_overrides_the_display_s_and_an_unusable_one_falls_back_to_srgb() {
        let srgb_display = display(Some(standard_display_profile(StandardDisplay::Srgb).unwrap()), false);
        match decide(&Choice::File(PathBuf::from("wide.icc")), Some(&srgb_display), |_| Ok(p3())) {
            Decided::Profile { icc, .. } => assert_eq!(icc, p3()),
            other @ Decided::Srgb(_) => panic!("{other:?}"),
        }
        let Decided::Srgb(Why::Unusable(reason)) = decide(&Choice::File(PathBuf::from("gone.icc")), None, no_file) else { panic!() };
        assert!(reason.contains("gone.icc"), "{reason}");
        // A display profile that isn't one a display can use.
        let Decided::Srgb(Why::Unusable(reason)) = decide(&Choice::Automatic, Some(&display(Some(vec![7; 300]), false)), no_file) else { panic!() };
        assert!(reason.contains("can't be used"), "{reason}");
    }

    #[test]
    fn the_follower_decides_again_only_when_the_display_or_the_choice_changes() {
        let mut follower = Follower::default();
        // No point: no display, sRGB.
        assert_eq!(follower.follow(None, &Choice::Srgb), Some(&Decided::Srgb(Why::Chosen)));
        assert_eq!(follower.follow(None, &Choice::Srgb), None, "nothing changed");
        assert_eq!(follower.follow(None, &Choice::Automatic), Some(&Decided::Srgb(Why::NoProfile)), "the choice changed");
        follower.refresh();
        assert!(follower.follow(None, &Choice::Automatic).is_some(), "asked to decide again");
    }

    #[test]
    fn the_system_names_a_display_without_failing() {
        // Whatever the machine has (CI runners may have no monitor, or no X server): no panic, and a display found
        // has a name. Its profile, when it has one, is one a display can use or the viewer falls back to sRGB.
        if let Some(display) = display_at([100.0, 100.0]) {
            assert_ne!(display.id, "");
            println!("{}: {} bytes of profile, system-managed {}", display.id, display.profile.as_ref().map_or(0, Vec::len), display.system_managed);
            let decided = decide(&Choice::Automatic, Some(&display), read_profile);
            println!("{}", decided.describe());
        }
    }

    #[test]
    fn profile_files_larger_than_any_profile_are_refused() {
        let dir = std::env::temp_dir().join(format!("florastack-profile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.icc");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_PROFILE_BYTES + 1).unwrap();
        assert!(read_profile(&big).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
