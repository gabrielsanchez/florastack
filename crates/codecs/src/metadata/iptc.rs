//! IPTC-IIM (IPTC Information Interchange Model 4.2): the source frame's record stream, carried byte for byte. TIFF
//! and DNG files hold it in tag 33723; JPEG files in an APP13 segment, as the Photoshop image resource 0x0404 (Adobe
//! Photoshop File Formats Specification, "Image Resource Blocks"). PNG and OpenEXR have no place for it: their XMP
//! carries the same fields (IPTC Photo Metadata Standard).

/// The APP13 payload for an IIM stream: "Photoshop 3.0", then one `8BIM` resource 0x0404 with an empty name. `None`
/// if it doesn't fit a JPEG segment.
pub(crate) fn jpeg_app13(iim: &[u8]) -> Option<Vec<u8>> {
    let size = u32::try_from(iim.len()).ok()?;
    let mut payload = Vec::with_capacity(iim.len() + 28);
    payload.extend_from_slice(b"Photoshop 3.0\0");
    payload.extend_from_slice(b"8BIM");
    payload.extend_from_slice(&0x0404u16.to_be_bytes());
    // An empty Pascal string, padded to an even length.
    payload.extend_from_slice(&[0, 0]);
    payload.extend_from_slice(&size.to_be_bytes());
    payload.extend_from_slice(iim);
    if iim.len() % 2 == 1 {
        payload.push(0);
    }
    // The segment length field (2 bytes) counts itself.
    u16::try_from(payload.len() + 2).is_ok().then_some(payload)
}

/// `jpeg` with an `APPn` segment (`marker` 0xE0 to 0xEF) holding `payload` inserted after the APP segments that
/// follow its start-of-image marker, or `None` if `jpeg` doesn't start like a JPEG file or the payload is too long.
pub(crate) fn insert_app_segment(jpeg: &[u8], marker: u8, payload: &[u8]) -> Option<Vec<u8>> {
    let length = u16::try_from(payload.len().checked_add(2)?).ok()?;
    if jpeg.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut at = 2usize;
    // Skip the APPn segments (0xFFE0 to 0xFFEF), each a marker and a big-endian length that counts itself.
    while let Some(&[0xff, m]) = jpeg.get(at..at + 2) {
        if !(0xe0..=0xef).contains(&m) {
            break;
        }
        let len = jpeg.get(at + 2..at + 4).map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))?;
        at = at.checked_add(2)?.checked_add(len)?;
        if at > jpeg.len() {
            return None;
        }
    }
    let mut out = Vec::with_capacity(jpeg.len() + payload.len() + 4);
    out.extend_from_slice(jpeg.get(..at)?);
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(jpeg.get(at..)?);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IIM datasets 2:5 (object name) and 2:25 (keyword), as cameras and editors write them.
    fn iim() -> Vec<u8> {
        let mut v = vec![0x1c, 2, 5, 0, 6];
        v.extend_from_slice(b"Weevil");
        v.extend_from_slice(&[0x1c, 2, 25, 0, 5]);
        v.extend_from_slice(b"macro");
        v
    }

    #[test]
    fn the_segment_is_found_where_lightcraft_meta_looks() {
        let app13 = jpeg_app13(&iim()).unwrap();
        // SOI, an APP0 (JFIF), then the start of the frame.
        let jpeg = [&[0xff, 0xd8, 0xff, 0xe0, 0, 4, b'J', b'F'][..], &[0xff, 0xdb, 0, 2, 0xff, 0xd9]].concat();
        let with = insert_app_segment(&jpeg, 0xed, &app13).unwrap();
        assert_eq!(&with[..8], &jpeg[..8], "after the APP0 segment");
        assert_eq!(&with[8..10], &[0xff, 0xed]);
        assert_eq!(lightcraft_meta::jpeg_segments(&with).iptc.as_deref(), Some(&iim()[..]));
        assert_eq!(&with[with.len() - 6..], &jpeg[8..], "the rest unchanged");
        // An odd-length stream is padded inside the resource, not in the data.
        let odd = jpeg_app13(b"\x1c\x02\x05\x00\x01X").unwrap();
        assert_eq!(odd.len() % 2, 0);
    }

    #[test]
    fn bad_input_is_refused_without_panicking() {
        assert!(insert_app_segment(b"GIF89a", 0xed, b"x").is_none());
        assert!(insert_app_segment(&[0xff, 0xd8, 0xff, 0xe1, 0xff], 0xed, b"x").is_none(), "truncated length");
        assert!(insert_app_segment(&[0xff, 0xd8, 0xff, 0xe1, 0x40, 0], 0xed, b"x").is_none(), "segment past the end");
        assert!(insert_app_segment(&[0xff, 0xd8], 0xed, &vec![0; 70_000]).is_none(), "payload too long");
        assert!(jpeg_app13(&vec![0; 70_000]).is_none());
        assert_eq!(insert_app_segment(&[0xff, 0xd8], 0xed, b"ab").unwrap(), [0xff, 0xd8, 0xff, 0xed, 0, 4, b'a', b'b']);
    }
}
