//! Helpers shared by the codec tests: synthetic images, and a camera-like JPEG whose EXIF, IPTC and XMP are made here
//! from the specifications (CIPA DC-008, IPTC-IIM 4.2, ISO 16684-1), so that no test depends on a downloaded photo.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use florastack_codecs::{BitDepth, OutputFormat, OutputProfile, OutputSpec};
use florastack_core::{ColorSpace, Image};
use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples};
use lightcraft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t};

/// An 8-bit image whose samples cover every value, different in every channel.
pub fn pattern_u8(w: usize, h: usize, channels: usize) -> Image<u8> {
    Image::from_fn(w, h, channels, ColorSpace::SRGB, |x, y, c| u8::try_from((x * 7 + y * 31 + c * 85) % 256).unwrap()).unwrap()
}

/// A 16-bit image whose samples spread over the whole range.
pub fn pattern_u16(w: usize, h: usize, channels: usize) -> Image<u16> {
    Image::from_fn(w, h, channels, ColorSpace::SRGB, |x, y, c| u16::try_from((x * 2_654_435 + y * 40_503 + c * 21_845) % 65_536).unwrap()).unwrap()
}

/// A linear float image with values outside `[0, 1]` in its color channels (alpha stays in range).
pub fn pattern_f32(w: usize, h: usize, channels: usize) -> Image<f32> {
    let alpha = channels == 2 || channels == 4;
    Image::from_fn(w, h, channels, ColorSpace::LINEAR, |x, y, c| {
        let v = ((x * 13 + y * 7 + c * 3) % 97) as f32 / 31.0 - 0.5;
        if alpha && c + 1 == channels { (v + 0.5) / 4.0 } else { v }
    })
    .unwrap()
}

/// A smooth, textured sRGB picture (the synthetic stacks' scene), for lossy formats.
pub fn picture_u8(w: usize, h: usize) -> Image<u8> {
    let (sharp, _) = florastack_core::testutil::scene::textured(w, h, 7).unwrap();
    sharp.to_encoding::<u8>(florastack_core::Encoding::Srgb).unwrap()
}

/// An output spec in the input's profile, keeping extended range.
pub fn spec(format: OutputFormat, depth: BitDepth) -> OutputSpec {
    OutputSpec { format, depth, profile: OutputProfile::Input, extended_range: true }
}

/// A camera's EXIF block: IFD0 (make, model, orientation, firmware, artist, copyright), an Exif IFD with exposure,
/// aperture, ISO, capture time, a `MakerNote` and a DCF interoperability IFD, and a GPS IFD.
pub fn camera_exif(orientation: u16) -> Vec<u8> {
    let mut interop = IfdBuilder::new();
    interop.set(1, Value::Ascii("R98".into()));
    let mut exif = IfdBuilder::new();
    exif.set(t::EXPOSURE_TIME, Value::Rational(vec![(1, 250)]));
    exif.set(t::F_NUMBER, Value::Rational(vec![(56, 10)]));
    exif.set(t::ISO_SPEED, Value::Short(vec![200]));
    exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii("2026:10:20 09:15:00".into()));
    exif.set(t::FOCAL_LENGTH, Value::Rational(vec![(100, 1)]));
    exif.set(t::LENS_MODEL, Value::Ascii("RF100mm F2.8 L MACRO IS USM".into()));
    exif.set(t::MAKER_NOTE, Value::Undefined(b"vendor data with offsets into the block".to_vec()));
    exif.set(40961, Value::Short(vec![1]));
    exif.set_child(t::INTEROP_IFD, interop);
    let mut gps = IfdBuilder::new();
    gps.set(t::GPS_LATITUDE_REF, Value::Ascii("N".into()));
    gps.set(t::GPS_LATITUDE, Value::Rational(vec![(47, 1), (22, 1), (0, 1)]));
    gps.set(t::GPS_LONGITUDE_REF, Value::Ascii("E".into()));
    gps.set(t::GPS_LONGITUDE, Value::Rational(vec![(8, 1), (32, 1), (0, 1)]));
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(t::MAKE, Value::Ascii("Canon".into()));
    ifd0.set(t::MODEL, Value::Ascii("Canon EOS R5".into()));
    ifd0.set(t::ORIENTATION, Value::Short(vec![orientation]));
    ifd0.set(t::SOFTWARE, Value::Ascii("Firmware Version 1.8.0".into()));
    ifd0.set(t::ARTIST, Value::Ascii("A. Photographer".into()));
    ifd0.set(t::COPYRIGHT, Value::Ascii("CC BY 4.0".into()));
    ifd0.set_child(t::EXIF_IFD, exif);
    ifd0.set_child(t::GPS_IFD, gps);
    let mut ifd1 = IfdBuilder::new();
    ifd1.set(t::COMPRESSION, Value::Short(vec![6]));
    TiffWriter::new(ByteOrder::Big, false).write(&[ifd0, ifd1]).unwrap()
}

/// An IPTC-IIM stream: object name (2:5), two keywords (2:25), by-line (2:80) and caption (2:120).
pub fn iim() -> Vec<u8> {
    let mut v = Vec::new();
    for (dataset, text) in [(5u8, "Weevil"), (25, "beetle"), (25, "macro"), (80, "A. Photographer"), (120, "A weevil on moss")] {
        v.extend_from_slice(&[0x1c, 2, dataset, 0, u8::try_from(text.len()).unwrap()]);
        v.extend_from_slice(text.as_bytes());
    }
    v
}

/// An editor's XMP packet: a title and keywords, Camera Raw develop settings, and a short history.
pub fn xmp_packet() -> String {
    r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
    xmp:CreatorTool="Canon EOS R5"
    xmp:Rating="4"
    crs:Exposure2012="+0.50"
    xmpMM:DocumentID="xmp.did:0001">
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Weevil on moss</rdf:li></rdf:Alt></dc:title>
   <dc:subject><rdf:Bag><rdf:li>beetle</rdf:li><rdf:li>macro</rdf:li></rdf:Bag></dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#
        .to_owned()
}

/// `jpeg` with an APP13 segment holding `iim` (Photoshop image resource 0x0404), inserted after its first APP
/// segment, as cameras and editors place it.
pub fn with_app13(jpeg: &[u8], iim: &[u8]) -> Vec<u8> {
    let mut resource = b"Photoshop 3.0\0".to_vec();
    resource.extend_from_slice(b"8BIM\x04\x04\0\0");
    resource.extend_from_slice(&u32::try_from(iim.len()).unwrap().to_be_bytes());
    resource.extend_from_slice(iim);
    if iim.len() % 2 == 1 {
        resource.push(0);
    }
    // SOI, then the first segment (APP0 JFIF from LightCraft's encoder).
    let first = 4 + usize::from(u16::from_be_bytes([jpeg[4], jpeg[5]]));
    let mut out = jpeg[..first].to_vec();
    out.extend_from_slice(&[0xff, 0xed]);
    out.extend_from_slice(&u16::try_from(resource.len() + 2).unwrap().to_be_bytes());
    out.extend_from_slice(&resource);
    out.extend_from_slice(&jpeg[first..]);
    out
}

/// A camera JPEG of `image` (RGB) at quality 100: EXIF with `orientation`, the IPTC stream, the XMP packet and an
/// sRGB profile.
pub fn camera_jpeg(image: &Image<u8>, orientation: u16) -> Vec<u8> {
    let exif = camera_exif(orientation);
    let xmp = xmp_packet();
    let icc = lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::Srgb);
    let meta = EncodeMeta { icc: Some(&icc), exif: Some(&exif), xmp: Some(&xmp), ppi: None };
    let input = EncodeImage::new(
        u32::try_from(image.width()).unwrap(),
        u32::try_from(image.height()).unwrap(),
        u8::try_from(image.channels()).unwrap(),
        Samples::U8(image.as_raw()),
    );
    let jpeg = lightcraft_codecs::encode_jpeg(&input, 100, ChromaSubsampling::S444, &meta).unwrap();
    with_app13(&jpeg, &iim())
}

/// The largest difference between two 8-bit images' samples.
pub fn max_diff_u8(a: &Image<u8>, b: &Image<u8>) -> u8 {
    assert!(a.same_shape(b), "{a:?} vs {b:?}");
    a.rows().zip(b.rows()).flat_map(|(r, s)| r.iter().zip(s).map(|(x, y)| x.abs_diff(*y))).max().unwrap_or(0)
}
