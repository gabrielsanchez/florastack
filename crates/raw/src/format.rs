//! The camera RAW formats FloraStack recognizes ([`RawFormat`]) and the file extensions they come with.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A camera RAW container format.
///
/// [`RawFormat::is_decoded`] says which ones the built-in decoder reads; within those, a few coding variants are
/// still unsupported (compressed Olympus ORF and Samsung SRW, Nikon's High Efficiency NEF), and they fail with
/// [`crate::RawError::Unsupported`], as the formats it only recognizes do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RawFormat {
    /// Adobe's Digital Negative, written by many cameras and phones and by raw converters.
    Dng,
    Cr2,
    /// Lossless CRX and C-RAW.
    Cr3,
    /// Canon CRW (2000 to 2004 bodies).
    Crw,
    Nef,
    /// Nikon Coolpix.
    Nrw,
    /// Sony ARW, and SR2.
    Arw,
    /// Fujifilm RAF, Bayer and X-Trans.
    Raf,
    Orf,
    /// Panasonic RW2 and RAW, and Leica RWL.
    Rw2,
    Pef,
    Srw,
    /// Epson ERF.
    Erf,
    /// Mamiya MEF.
    Mef,
    /// Hasselblad 3FR and FFF.
    Hasselblad,
    /// Kodak DCS TIFF.
    KodakDcs,
    /// Minolta MRW.
    Mrw,
    /// Sigma X3F (Foveon).
    X3f,
    /// Another TIFF-based raw (IIQ, MOS, KDC, DCR and others).
    OtherTiff,
}

impl RawFormat {
    /// Every format.
    pub const ALL: [RawFormat; 19] = [
        RawFormat::Dng,
        RawFormat::Cr2,
        RawFormat::Cr3,
        RawFormat::Crw,
        RawFormat::Nef,
        RawFormat::Nrw,
        RawFormat::Arw,
        RawFormat::Raf,
        RawFormat::Orf,
        RawFormat::Rw2,
        RawFormat::Pef,
        RawFormat::Srw,
        RawFormat::Erf,
        RawFormat::Mef,
        RawFormat::Hasselblad,
        RawFormat::KodakDcs,
        RawFormat::Mrw,
        RawFormat::X3f,
        RawFormat::OtherTiff,
    ];

    /// The name people know it by.
    pub fn name(self) -> &'static str {
        match self {
            RawFormat::Dng => "DNG",
            RawFormat::Cr2 => "Canon CR2",
            RawFormat::Cr3 => "Canon CR3",
            RawFormat::Crw => "Canon CRW",
            RawFormat::Nef => "Nikon NEF",
            RawFormat::Nrw => "Nikon NRW",
            RawFormat::Arw => "Sony ARW",
            RawFormat::Raf => "Fujifilm RAF",
            RawFormat::Orf => "Olympus ORF",
            RawFormat::Rw2 => "Panasonic RW2",
            RawFormat::Pef => "Pentax PEF",
            RawFormat::Srw => "Samsung SRW",
            RawFormat::Erf => "Epson ERF",
            RawFormat::Mef => "Mamiya MEF",
            RawFormat::Hasselblad => "Hasselblad 3FR",
            RawFormat::KodakDcs => "Kodak DCS",
            RawFormat::Mrw => "Minolta MRW",
            RawFormat::X3f => "Sigma X3F",
            RawFormat::OtherTiff => "camera RAW",
        }
    }

    /// The file extensions of the format, lower case and without the dot (a watch folder picks files by them).
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            RawFormat::Dng => &["dng"],
            RawFormat::Cr2 => &["cr2"],
            RawFormat::Cr3 => &["cr3"],
            RawFormat::Crw => &["crw"],
            RawFormat::Nef => &["nef"],
            RawFormat::Nrw => &["nrw"],
            RawFormat::Arw => &["arw", "sr2", "srf"],
            RawFormat::Raf => &["raf"],
            RawFormat::Orf => &["orf"],
            RawFormat::Rw2 => &["rw2", "rwl", "raw"],
            RawFormat::Pef => &["pef"],
            RawFormat::Srw => &["srw"],
            RawFormat::Erf => &["erf"],
            RawFormat::Mef => &["mef"],
            RawFormat::Hasselblad => &["3fr", "fff"],
            RawFormat::KodakDcs => &["tif"],
            RawFormat::Mrw => &["mrw"],
            RawFormat::X3f => &["x3f"],
            RawFormat::OtherTiff => &["iiq", "mos", "kdc", "dcr"],
        }
    }

    /// Whether the built-in decoder reads this format (some coding variants inside it aside).
    pub fn is_decoded(self) -> bool {
        !matches!(self, RawFormat::Crw | RawFormat::Mrw | RawFormat::X3f | RawFormat::OtherTiff)
    }

    pub(crate) fn from_lightcraft(format: lightcraft_raw::RawFormat) -> RawFormat {
        use lightcraft_raw::RawFormat as Lc;
        match format {
            Lc::Dng => RawFormat::Dng,
            Lc::Cr2 => RawFormat::Cr2,
            Lc::Cr3 => RawFormat::Cr3,
            Lc::Crw => RawFormat::Crw,
            Lc::Nef => RawFormat::Nef,
            Lc::Nrw => RawFormat::Nrw,
            Lc::Arw => RawFormat::Arw,
            Lc::Raf => RawFormat::Raf,
            Lc::Orf => RawFormat::Orf,
            Lc::Rw2 => RawFormat::Rw2,
            Lc::Pef => RawFormat::Pef,
            Lc::Srw => RawFormat::Srw,
            Lc::Erf => RawFormat::Erf,
            Lc::Mef => RawFormat::Mef,
            Lc::CfaTiff => RawFormat::Hasselblad,
            Lc::KodakDcs => RawFormat::KodakDcs,
            Lc::Mrw => RawFormat::Mrw,
            Lc::X3f => RawFormat::X3f,
            Lc::OtherTiff => RawFormat::OtherTiff,
        }
    }
}

impl fmt::Display for RawFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Every RAW file extension, lower case and without the dot.
pub fn extensions() -> impl Iterator<Item = &'static str> {
    RawFormat::ALL.into_iter().flat_map(|f| f.extensions().iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lightcraft_format_has_a_name_and_the_decoded_ones_agree() {
        use lightcraft_raw::RawFormat as Lc;
        let all = [
            Lc::Dng,
            Lc::Cr2,
            Lc::Cr3,
            Lc::Nef,
            Lc::Nrw,
            Lc::Arw,
            Lc::Raf,
            Lc::Orf,
            Lc::Rw2,
            Lc::Pef,
            Lc::Srw,
            Lc::Crw,
            Lc::Mrw,
            Lc::X3f,
            Lc::CfaTiff,
            Lc::KodakDcs,
            Lc::Erf,
            Lc::Mef,
            Lc::OtherTiff,
        ];
        for lc in all {
            let ours = RawFormat::from_lightcraft(lc);
            // LightCraft says Olympus ORF is not supported as a container because only its uncompressed files decode;
            // FloraStack counts it as decoded and reports the compressed ones as unsupported, which they are.
            if ours != RawFormat::Orf {
                assert_eq!(ours.is_decoded(), lc.is_supported(), "{ours}");
            }
            assert!(!ours.name().is_empty() && !ours.extensions().is_empty());
        }
        assert_eq!(RawFormat::ALL.len(), all.len());
        assert!(extensions().any(|e| e == "rwl") && extensions().any(|e| e == "cr3"));
        assert!(extensions().all(|e| e == e.to_ascii_lowercase() && !e.starts_with('.')));
    }

    #[test]
    fn formats_serialize_by_name() {
        assert_eq!(serde_json::to_string(&RawFormat::KodakDcs).unwrap(), "\"kodak-dcs\"");
        assert_eq!(serde_json::from_str::<RawFormat>("\"cr3\"").unwrap(), RawFormat::Cr3);
    }
}
