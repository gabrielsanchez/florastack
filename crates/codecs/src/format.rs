//! The file formats this crate reads and writes, and their detection from a file's first bytes.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::CodecError;

/// An image file format.
///
/// | Format | Read | Write |
/// |---|---|---|
/// | JPEG | 8-bit gray and RGB | 8-bit, with a quality setting |
/// | PNG | 1 to 16 bits, palette and transparency included | 8 and 16 bits |
/// | TIFF | 1 to 16 bits and 16, 32 or 64-bit float; uncompressed, LZW, Deflate, PackBits and JPEG; strips, tiles, planes | 8, 16 and 32-bit float; uncompressed, LZW and Deflate |
/// | OpenEXR | half, float and integer channels | 32-bit or half float |
/// | WebP | lossy and lossless, the first frame of an animation | no |
/// | BMP | the formats of Windows and OS/2 | no |
/// | DNG | no: the RAW importer reads it | linear 16-bit |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    Jpeg,
    Png,
    Tiff,
    OpenExr,
    WebP,
    Bmp,
    Dng,
}

impl Format {
    /// Every format, the read-only and write-only ones included.
    pub const ALL: [Format; 7] = [Format::Jpeg, Format::Png, Format::Tiff, Format::OpenExr, Format::WebP, Format::Bmp, Format::Dng];

    /// The name people know it by.
    pub fn name(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG",
            Format::Png => "PNG",
            Format::Tiff => "TIFF",
            Format::OpenExr => "OpenEXR",
            Format::WebP => "WebP",
            Format::Bmp => "BMP",
            Format::Dng => "DNG",
        }
    }

    /// The usual file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Jpeg => "jpg",
            Format::Png => "png",
            Format::Tiff => "tif",
            Format::OpenExr => "exr",
            Format::WebP => "webp",
            Format::Bmp => "bmp",
            Format::Dng => "dng",
        }
    }

    /// Whether [`crate::read`] reads this format (DNG is read by the RAW importer).
    pub fn can_read(self) -> bool {
        !matches!(self, Format::Dng)
    }

    /// Whether [`crate::write`] writes this format.
    pub fn can_write(self) -> bool {
        !matches!(self, Format::WebP | Format::Bmp)
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The OpenEXR magic number (OpenEXR file layout, "Magic Number").
const EXR_MAGIC: [u8; 4] = [0x76, 0x2f, 0x31, 0x01];

/// The format of a file from its first bytes (a few KiB; for TIFF, which may be a camera RAW in disguise, the whole
/// file is best). Camera RAW and DNG files, and formats this crate doesn't read, are errors that say what to do.
pub fn sniff(bytes: &[u8]) -> Result<Format, CodecError> {
    use lightcraft_codecs::Format as Lc;
    if bytes.starts_with(&EXR_MAGIC) {
        return Ok(Format::OpenExr);
    }
    match lightcraft_codecs::sniff(bytes) {
        Some(Lc::Jpeg) => Ok(Format::Jpeg),
        Some(Lc::Png) => Ok(Format::Png),
        Some(Lc::Tiff) => Ok(Format::Tiff),
        Some(Lc::WebP) => Ok(Format::WebP),
        Some(Lc::Bmp) => Ok(Format::Bmp),
        Some(Lc::RawTiffLike) => Err(CodecError::RawFile("DNG or camera RAW file")),
        Some(Lc::RawOther) => Err(CodecError::RawFile("camera RAW file")),
        Some(Lc::Gif) => Err(CodecError::UnsupportedFormat("GIF")),
        Some(Lc::Psd) => Err(CodecError::UnsupportedFormat("Photoshop (PSD)")),
        Some(Lc::Jxl) => Err(CodecError::UnsupportedFormat("JPEG XL")),
        Some(Lc::Avif) => Err(CodecError::UnsupportedFormat("AVIF")),
        Some(Lc::Heif) => Err(CodecError::UnsupportedFormat("HEIF/HEIC")),
        None => Err(CodecError::UnknownFormat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_numbers_name_the_format() {
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0, 0, 0]).unwrap(), Format::Jpeg);
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n\0\0\0\x0d").unwrap(), Format::Png);
        assert_eq!(sniff(&[0x76, 0x2f, 0x31, 0x01, 2, 0, 0, 0]).unwrap(), Format::OpenExr);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ").unwrap(), Format::WebP);
        assert!(matches!(sniff(b"GIF89a\0\0"), Err(CodecError::UnsupportedFormat("GIF"))));
        assert!(matches!(sniff(b"FUJIFILMCCD-RAW 0201"), Err(CodecError::RawFile(_))));
        assert!(matches!(sniff(b"hello"), Err(CodecError::UnknownFormat)));
        assert!(matches!(sniff(b""), Err(CodecError::UnknownFormat)));
    }

    #[test]
    fn names_and_capabilities() {
        assert_eq!(Format::OpenExr.to_string(), "OpenEXR");
        assert!(Format::ALL.iter().all(|f| f.can_read() || f.can_write()));
        assert!(!Format::Dng.can_read() && Format::Dng.can_write());
        assert!(Format::WebP.can_read() && !Format::WebP.can_write());
    }
}
