//! Decoding an image file with florastack-codecs, as a frame is read: JPEG, PNG, TIFF, OpenEXR, WebP and BMP, with
//! their color profiles and orientation. A damaged file must be an error, never a panic, a hang or an allocation out
//! of proportion to the file.
#![no_main]

use florastack_codecs::{OrientationPolicy, ReadOptions};
use libfuzzer_sys::fuzz_target;

/// The largest image decoded, in pixels: a header that claims more is refused before anything is allocated, as the
/// app refuses one beyond its own, larger limit. Small, so that each input decodes quickly.
const MAX_PIXELS: u64 = 1 << 22;

fuzz_target!(|data: &[u8]| {
    let options = ReadOptions { orientation: OrientationPolicy::Apply, max_pixels: MAX_PIXELS };
    let _ = florastack_codecs::read(data, &options);
});
