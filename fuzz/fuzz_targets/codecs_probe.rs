//! What florastack-codecs reads from a file's headers, without decoding its pixels: the format, the size and
//! layout, and the EXIF, IPTC and XMP metadata. These run on every file added to a project, so they must turn any
//! input into an answer or an error, never a panic or a hang.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = florastack_codecs::sniff(data);
    let _ = florastack_codecs::probe(data);
    let _ = florastack_codecs::read_metadata(data);
});
