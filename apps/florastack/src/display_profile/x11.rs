//! The display under a point on Linux under X11 (see the module above), through x11rb's pure-Rust connection: the
//! `RandR` monitor that holds the point, and the profile color managers (colord, GNOME, KDE) set on the root window for
//! it, after the "ICC Profiles in X" convention: `_ICC_PROFILE` for the first monitor, `_ICC_PROFILE_<n>` for the
//! n-th. No display (Wayland without XWayland) or no property: `None` or no profile, and the viewer sends sRGB.

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

use super::{Display, MAX_PROFILE_BYTES};

pub(super) fn display_at(point: [f64; 2]) -> Option<Display> {
    std::env::var_os("DISPLAY")?;
    let (connection, screen) = x11rb::connect(None).ok()?;
    let root = connection.setup().roots.get(screen)?.root;
    let monitors = connection.randr_get_monitors(root, true).ok()?.reply().ok()?.monitors;
    let (x, y) = (point[0], point[1]);
    let index = monitors.iter().position(|m| {
        let (left, top) = (f64::from(m.x), f64::from(m.y));
        x >= left && y >= top && x < left + f64::from(m.width) && y < top + f64::from(m.height)
    })?;
    let name = if index == 0 { "_ICC_PROFILE".to_owned() } else { format!("_ICC_PROFILE_{index}") };
    let atom = connection.intern_atom(true, name.as_bytes()).ok()?.reply().ok()?.atom;
    let id = format!("monitor {index}");
    if atom == u32::from(AtomEnum::NONE) {
        return Some(Display { id, profile: None, system_managed: false });
    }
    // The property's length is in 32-bit units.
    let units = u32::try_from(MAX_PROFILE_BYTES / 4).unwrap_or(u32::MAX);
    let property = connection.get_property(false, root, atom, AtomEnum::ANY, 0, units).ok()?.reply().ok()?;
    let profile = (property.format == 8 && !property.value.is_empty()).then_some(property.value);
    Some(Display { id, profile, system_managed: false })
}
