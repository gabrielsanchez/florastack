//! The display under a point on macOS (see the module above): the display there (`CGGetDisplaysWithPoint`, in the
//! global display coordinates' points), its color space as `ColorSync` has it (`CGDisplayCopyColorSpace`) and that
//! space's ICC data; and the window's Metal layer tagged with the space the viewer sends (`setColorspace`), so that
//! Core Animation converts from it to the display once, and not at all when it is the display's own.
//!
//! Each `unsafe` block says what it relies on.

#![allow(unsafe_code)]

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{ClassType, msg_send};
use objc2_core_foundation::{CFData, CFRetained, CGPoint};
use objc2_core_graphics::{CGColorSpace, CGDirectDisplayID, CGError, CGGetDisplaysWithPoint, kCGColorSpaceSRGB};
use objc2_quartz_core::{CALayer, CAMetalLayer};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::Display;

unsafe extern "C-unwind" {
    // Declared here rather than taken from objc2-core-graphics, whose wrapper panics when the display has no color
    // space (it returns null for a display that went away): a crash loses work.
    fn CGDisplayCopyColorSpace(display: CGDirectDisplayID) -> Option<NonNull<CGColorSpace>>;
}

pub(super) fn display_at(point: [f64; 2]) -> Option<Display> {
    let mut display: CGDirectDisplayID = 0;
    let mut count: u32 = 0;
    // SAFETY: room for one display id and the count, both owned here, as the arguments say.
    let error = unsafe { CGGetDisplaysWithPoint(CGPoint { x: point[0], y: point[1] }, 1, &raw mut display, &raw mut count) };
    if error != CGError::Success || count == 0 {
        return None;
    }
    // SAFETY: CGDisplayCopyColorSpace takes any display id and returns a retained color space or null, which
    // CFRetained takes ownership of (released when dropped).
    let space = unsafe { CGDisplayCopyColorSpace(display) }.map(|space| unsafe { CFRetained::from_raw(space) });
    let profile = space.and_then(|space| CGColorSpace::icc_data(Some(&space))).map(|data| data.to_vec()).filter(|bytes| !bytes.is_empty());
    Some(Display { id: format!("display {display}"), profile, system_managed: false })
}

/// The window's Metal layer: wgpu's, the view's layer or one of its sublayers (raw-window-metal adds it as one).
fn metal_layer(window: &impl HasWindowHandle) -> Option<Retained<CAMetalLayer>> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return None };
    // SAFETY: the AppKit handle's view is an NSView that lives as long as the window, which outlives this call (the
    // handle borrows it); `layer` is an NSView method returning its (possibly nil) CALayer, retained here.
    let layer: Option<Retained<CALayer>> = unsafe {
        let view: &AnyObject = appkit.ns_view.cast::<AnyObject>().as_ref();
        msg_send![view, layer]
    };
    let layer = layer?;
    if layer.isKindOfClass(CAMetalLayer::class()) {
        return layer.downcast::<CAMetalLayer>().ok();
    }
    // SAFETY: the layer's sublayers, read on the main thread (App::logic), where AppKit changes them.
    let sublayers = unsafe { layer.sublayers() }?;
    sublayers.iter().find(|l| l.isKindOfClass(CAMetalLayer::class())).and_then(|l| l.downcast::<CAMetalLayer>().ok())
}

/// Tags the window's Metal layer with `icc`'s color space, or sRGB without it (or when the profile makes none).
pub(super) fn tag_output(window: &impl HasWindowHandle, icc: Option<&[u8]>) {
    let Some(layer) = metal_layer(window) else {
        tracing::warn!("the window has no Metal layer to tag with the display profile: macOS may convert its colors again");
        return;
    };
    let tagged = icc.and_then(|bytes| {
        let data = CFData::from_bytes(bytes);
        // SAFETY: `data` is a CFData, the type CGColorSpaceCreateWithICCData takes; it returns a retained space or
        // null for data that isn't a usable profile.
        unsafe { CGColorSpace::with_icc_data(Some(&data)) }
    });
    // SAFETY: kCGColorSpaceSRGB is a constant CFString that Core Graphics defines.
    let space = tagged.or_else(|| CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })));
    layer.setColorspace(space.as_deref());
}
