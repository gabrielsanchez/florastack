//! The display under a point on Windows (see the module above): the monitor there (`MonitorFromPoint`) and its GDI
//! device name, the default ICC profile Windows associates with that device (`GetICMProfileW` on a device context for
//! it), and whether advanced color is on for it (`DisplayConfigGetDeviceInfo`): automatic color management or HDR,
//! when the compositor converts sRGB windows to the display itself.
//!
//! Every call here is a plain Win32 function given structures this module owns, sized as the function asks; each
//! `unsafe` block says what else it relies on.

#![allow(unsafe_code)]

use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, POINT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
};
use windows_sys::Win32::UI::ColorSystem::GetICMProfileW;

use super::{Display, read_profile};

/// The longest profile path `GetICMProfileW` is asked for, in UTF-16 units.
const MAX_PATH_UNITS: u32 = 32_768;

/// A wide string up to its first NUL.
fn until_nul(units: &[u16]) -> &[u16] {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    units.get(..end).unwrap_or(units)
}

#[allow(clippy::cast_possible_truncation, reason = "desktop coordinates, which are 32-bit on Windows")]
pub(super) fn display_at(point: [f64; 2]) -> Option<Display> {
    let at = POINT { x: point[0].round() as i32, y: point[1].round() as i32 };
    // SAFETY: MonitorFromPoint takes the point by value and a flag; with MONITOR_DEFAULTTONEAREST it always returns a
    // monitor handle, which is not owned (nothing to release).
    let monitor = unsafe { MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = u32::try_from(std::mem::size_of::<MONITORINFOEXW>()).ok()?;
    // SAFETY: `info` is a MONITORINFOEXW whose cbSize says so, which GetMonitorInfoW accepts through a MONITORINFO
    // pointer (the extended structure starts with it) and fills, device name included; `monitor` came from
    // MonitorFromPoint just now.
    let found = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
    if found == 0 {
        return None;
    }
    let device: Vec<u16> = until_nul(&info.szDevice).to_vec();
    let id = String::from_utf16_lossy(&device);
    let profile = profile_path(&device).and_then(|path| read_profile(&path).ok());
    let system_managed = advanced_color(&device).unwrap_or(false);
    Some(Display { id, profile, system_managed })
}

/// The path of the profile Windows associates with display device `device` (its GDI name, `\\.\DISPLAY1`).
fn profile_path(device: &[u16]) -> Option<std::path::PathBuf> {
    let name: Vec<u16> = device.iter().copied().chain([0]).collect();
    // SAFETY: `name` is a NUL-terminated wide string that outlives the call; the port and the mode are optional and
    // null. The device context returned is released with DeleteDC below, on every path.
    let dc = unsafe { CreateDCW(name.as_ptr(), name.as_ptr(), std::ptr::null(), std::ptr::null()) };
    if dc.is_null() {
        return None;
    }
    let mut size: u32 = 0;
    // SAFETY: with a null buffer, GetICMProfileW only writes the size it needs, in UTF-16 units, into `size`.
    unsafe { GetICMProfileW(dc, &raw mut size, std::ptr::null_mut()) };
    let path = (1..=MAX_PATH_UNITS).contains(&size).then(|| {
        let mut buffer = vec![0u16; size as usize];
        let mut length = size;
        // SAFETY: `buffer` holds `length` UTF-16 units, as GetICMProfileW is told, and outlives the call.
        let ok = unsafe { GetICMProfileW(dc, &raw mut length, buffer.as_mut_ptr()) };
        (ok != 0).then(|| std::path::PathBuf::from(String::from_utf16_lossy(until_nul(&buffer))))
    });
    // SAFETY: `dc` came from CreateDCW above and is deleted once.
    unsafe { DeleteDC(dc) };
    path.flatten()
}

/// Whether advanced color is on for display device `device`: its source's path's target says so; `None` when the
/// display configuration can't be read.
fn advanced_color(device: &[u16]) -> Option<bool> {
    let (mut paths, mut modes) = (0u32, 0u32);
    // SAFETY: GetDisplayConfigBufferSizes writes two counts into the two u32s it is given.
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &raw mut paths, &raw mut modes) } != ERROR_SUCCESS {
        return None;
    }
    let mut path_info = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
    let mut mode_info = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
    // SAFETY: the two arrays hold `paths` and `modes` elements, as the counts passed say; QueryDisplayConfig writes at
    // most that many and updates the counts. The topology is only asked for with QDC_DATABASE_CURRENT, so it is null.
    let queried = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &raw mut paths,
            path_info.as_mut_ptr(),
            &raw mut modes,
            mode_info.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    };
    if queried != ERROR_SUCCESS {
        return None;
    }
    path_info.truncate(paths as usize);
    for path in &path_info {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = u32::try_from(std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>()).ok()?;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        // SAFETY: `source` is the structure its header's type and size name, which DisplayConfigGetDeviceInfo fills.
        if unsafe { DisplayConfigGetDeviceInfo(&raw mut source.header) } != 0 || until_nul(&source.viewGdiDeviceName) != device {
            continue;
        }
        let mut color = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default();
        color.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
        color.header.size = u32::try_from(std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>()).ok()?;
        color.header.adapterId = path.targetInfo.adapterId;
        color.header.id = path.targetInfo.id;
        // SAFETY: as above, for the advanced color information of the path's target.
        if unsafe { DisplayConfigGetDeviceInfo(&raw mut color.header) } != 0 {
            return None;
        }
        // SAFETY: the union's two members are the same 32 bits; `value` reads them as one integer. Bit 1 is
        // advancedColorEnabled.
        let bits = unsafe { color.Anonymous.value };
        return Some(bits & 0b10 != 0);
    }
    None
}
