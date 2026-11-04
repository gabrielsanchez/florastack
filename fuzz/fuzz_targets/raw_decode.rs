//! Reading a camera RAW file with florastack-raw, the adapter over LightCraft's decoders: the format sniffed, the
//! headers read, the sensor data decoded and, for a file that decodes, the develop step resolved on it and run as for
//! a preview. A damaged file must be an error, never a panic, a hang or an allocation out of proportion to the file.
#![no_main]

use florastack_raw::{DevelopSettings, Quality, RawDevelop};
use libfuzzer_sys::fuzz_target;

/// The largest sensor decoded, in pixels: a header that claims more is refused before anything is allocated.
const MAX_PIXELS: u64 = 1 << 24;

fuzz_target!(|data: &[u8]| {
    let _ = florastack_raw::probe(data);
    let _ = florastack_raw::info(data);
    let Ok(frame) = florastack_raw::decode(data, MAX_PIXELS) else { return };
    let Ok(develop) = RawDevelop::new(&DevelopSettings::default(), &frame) else { return };
    let _ = develop.develop(&frame, Quality::Preview);
});
