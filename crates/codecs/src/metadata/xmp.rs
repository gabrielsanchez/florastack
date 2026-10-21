//! XMP (ISO 16684-1): the source frame's packet carried over, with FloraStack's own properties merged in by
//! lightcraft-meta's `merge_xmp`, which keeps everything it isn't told to replace byte for byte.
//!
//! FloraStack writes `xmp:CreatorTool` (the XMP twin of the EXIF `Software` tag), the dates when it is given one,
//! the output's orientation and size, and the stack's frame count in its own namespace (`branding.toml`). It removes
//! what no longer describes the output: the source's develop settings (Camera Raw's `crs:` and LightCraft's `lc:`,
//! which a raw converter would apply again), its document identity and the structure of its pixels.

use std::fmt::Write as _;

use florastack_core::branding;

use crate::error::{MetadataKind, Warning};

/// What the output's XMP says about itself.
pub(crate) struct XmpOutput<'a> {
    pub creator_tool: &'a str,
    pub frame_count: Option<u32>,
    /// `xmp:ModifyDate` and `xmp:MetadataDate`, ISO 8601.
    pub modified: Option<&'a str>,
    pub width: u32,
    pub height: u32,
}

/// The XMP packet's own properties that the merged packet owns, whatever the source said.
fn owned() -> Vec<String> {
    let mut owned: Vec<String> = [
        "xmp:CreatorTool",
        "xmp:ModifyDate",
        "xmp:MetadataDate",
        "crs:*",
        "lc:*",
        "tiff:Orientation",
        "tiff:ImageWidth",
        "tiff:ImageLength",
        "tiff:BitsPerSample",
        "tiff:Compression",
        "tiff:PhotometricInterpretation",
        "tiff:SamplesPerPixel",
        "tiff:PlanarConfiguration",
        "tiff:YCbCrSubSampling",
        "exif:PixelXDimension",
        "exif:PixelYDimension",
        "exif:ColorSpace",
        "photoshop:ICCProfile",
        "photoshop:ColorMode",
        "xmpMM:InstanceID",
        "xmpMM:DocumentID",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    owned.push(format!("{}:*", branding::XMP_PREFIX));
    owned
}

/// What goes first when a packet is too large: the document history Photoshop and Lightroom accumulate.
const HISTORY: &[&str] =
    &["photoshop:DocumentAncestors", "xmpMM:History", "xmpMM:Ingredients", "xmpMM:Pantry", "xmpMM:Manifest", "xmpMM:DerivedFrom"];

/// `text` escaped for XML element content and attribute values.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters XML 1.0 forbids are dropped rather than written.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// FloraStack's own packet: the properties it sets, in one `rdf:Description`.
fn fresh(out: &XmpOutput<'_>) -> String {
    let mut props = format!("\n   <xmp:CreatorTool>{}</xmp:CreatorTool>", escape(out.creator_tool));
    // Writing to a String can't fail.
    if let Some(when) = out.modified {
        let when = escape(when);
        let _ = write!(props, "\n   <xmp:ModifyDate>{when}</xmp:ModifyDate>\n   <xmp:MetadataDate>{when}</xmp:MetadataDate>");
    }
    let _ = write!(
        props,
        "\n   <tiff:Orientation>1</tiff:Orientation>\n   <exif:PixelXDimension>{}</exif:PixelXDimension>\n   <exif:PixelYDimension>{}</exif:PixelYDimension>",
        out.width, out.height
    );
    if let Some(count) = out.frame_count {
        let p = branding::XMP_PREFIX;
        let _ = write!(props, "\n   <{p}:FrameCount>{count}</{p}:FrameCount>");
    }
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
         <rdf:Description rdf:about=\"\"\n    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n    xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\"\n    \
         xmlns:exif=\"http://ns.adobe.com/exif/1.0/\"\n    xmlns:{}=\"{}\">{props}\n  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>",
        branding::XMP_PREFIX,
        branding::XMP_NAMESPACE
    )
}

/// The packets larger than this aren't parsed: no camera or editor writes one, and parsing is linear in its size.
const MAX_SOURCE_BYTES: usize = 16 << 20;

/// The output's XMP packet: the source frame's merged with FloraStack's properties, no longer than `budget` bytes if
/// a budget is given (a JPEG APP1 segment holds 65,504 bytes of packet). Warnings say what was removed.
pub(crate) fn for_output(source: Option<&str>, out: &XmpOutput<'_>, budget: Option<usize>, warnings: &mut Vec<Warning>) -> Option<String> {
    let fresh = fresh(out);
    let fits = |packet: &str| budget.is_none_or(|b| packet.len() <= b);
    let source = match source {
        Some(s) if s.len() > MAX_SOURCE_BYTES => {
            warnings.push(Warning::MetadataDropped { kind: MetadataKind::Xmp, reason: "the source frame's packet is larger than 16 MiB" });
            None
        }
        other => other,
    };
    if let Some(source) = source {
        let mut owned = owned();
        let merged = merge(source, &fresh, &owned);
        match merged {
            Some(packet) if fits(&packet) => return Some(packet),
            Some(_) => {
                owned.extend(HISTORY.iter().map(|s| (*s).to_owned()));
                if let Some(packet) = merge(source, &fresh, &owned).filter(|p| fits(p)) {
                    warnings.push(Warning::MetadataTrimmed { kind: MetadataKind::Xmp, removed: "the document history" });
                    return Some(packet);
                }
                warnings.push(Warning::MetadataTrimmed { kind: MetadataKind::Xmp, removed: "all but FloraStack's own properties" });
            }
            None => warnings.push(Warning::UnreadableMetadata(MetadataKind::Xmp)),
        }
    }
    if fits(&fresh) {
        Some(fresh)
    } else {
        warnings.push(Warning::MetadataDropped { kind: MetadataKind::Xmp, reason: "it doesn't fit the format" });
        None
    }
}

fn merge(source: &str, fresh: &str, owned: &[String]) -> Option<String> {
    let owned: Vec<&str> = owned.iter().map(String::as_str).collect();
    let rules = lightcraft_meta::MergeRules { owned: &owned, owned_if_present: &[] };
    std::panic::catch_unwind(|| lightcraft_meta::merge_xmp(source, fresh, rules)).ok()?.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/" xmp:CreatorTool="Camera 1.0" crs:Exposure2012="+0.50" xmpMM:DocumentID="xmp.did:1"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">Weevil &amp; moss</rdf:li></rdf:Alt></dc:title><dc:subject><rdf:Bag><rdf:li>beetle</rdf:li><rdf:li>macro</rdf:li></rdf:Bag></dc:subject></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;

    fn output() -> XmpOutput<'static> {
        XmpOutput { creator_tool: "FloraStack 0.1.0", frame_count: Some(57), modified: Some("2026-10-21T12:00:00"), width: 4000, height: 3000 }
    }

    fn properties(packet: &str) -> std::collections::BTreeMap<String, lightcraft_meta::XmpValue> {
        lightcraft_meta::parse_xmp(packet).unwrap().values
    }

    #[test]
    fn the_source_packet_keeps_its_own_properties_and_gains_florastacks() {
        let mut w = Vec::new();
        let packet = for_output(Some(SOURCE), &output(), None, &mut w).unwrap();
        assert_eq!(w, []);
        let p = properties(&packet);
        let text = |k: &str| p.get(k).and_then(lightcraft_meta::XmpValue::text).map(str::to_owned);
        assert_eq!(text("xmp:CreatorTool").as_deref(), Some("FloraStack 0.1.0"));
        assert_eq!(text(&format!("{}:FrameCount", branding::XMP_PREFIX)).as_deref(), Some("57"));
        assert_eq!(text("tiff:Orientation").as_deref(), Some("1"));
        assert_eq!(text("exif:PixelXDimension").as_deref(), Some("4000"));
        assert!(p.contains_key("dc:title") && p.contains_key("dc:subject"), "{p:?}");
        assert!(!p.keys().any(|k| k.starts_with("crs:")), "develop settings go");
        assert!(!p.contains_key("xmpMM:DocumentID"));
        assert!(packet.contains("Weevil &amp; moss"), "the source's bytes stay as they were");
    }

    #[test]
    fn without_a_source_the_fresh_packet_parses() {
        let mut w = Vec::new();
        let packet = for_output(None, &XmpOutput { frame_count: None, modified: None, ..output() }, None, &mut w).unwrap();
        let p = properties(&packet);
        assert!(p.contains_key("xmp:CreatorTool") && !p.keys().any(|k| k.ends_with(":FrameCount")), "{p:?}");
        let odd = XmpOutput { creator_tool: "A <B> & \"C\"\u{1}", ..output() };
        let packet = for_output(None, &odd, None, &mut w).unwrap();
        assert_eq!(properties(&packet).get("xmp:CreatorTool").and_then(lightcraft_meta::XmpValue::text), Some("A <B> & \"C\""));
        assert_eq!(w, []);
    }

    #[test]
    fn a_packet_too_large_for_its_budget_is_trimmed_in_order() {
        let mut history = String::new();
        for i in 0..3000 {
            write!(history, "<rdf:li stEvt:action=\"saved\" stEvt:instanceID=\"xmp.iid:{i:08}\"/>").unwrap();
        }
        let big = SOURCE.replace(
            "</rdf:Description>",
            &format!(
                "<xmpMM:History xmlns:stEvt=\"http://ns.adobe.com/xap/1.0/sType/ResourceEvent#\"><rdf:Seq>{history}</rdf:Seq></xmpMM:History></rdf:Description>"
            ),
        );
        let mut w = Vec::new();
        let packet = for_output(Some(&big), &output(), Some(65_504), &mut w).unwrap();
        assert!(packet.len() <= 65_504);
        assert!(properties(&packet).contains_key("dc:title"), "only the history went");
        assert_eq!(w, [Warning::MetadataTrimmed { kind: MetadataKind::Xmp, removed: "the document history" }]);
        // Room for FloraStack's own packet only.
        let mut w = Vec::new();
        let packet = for_output(Some(&big), &output(), Some(fresh(&output()).len() + 10), &mut w).unwrap();
        assert!(!properties(&packet).contains_key("dc:title"));
        assert_eq!(w, [Warning::MetadataTrimmed { kind: MetadataKind::Xmp, removed: "all but FloraStack's own properties" }]);
        let mut w = Vec::new();
        assert!(for_output(Some(SOURCE), &output(), Some(10), &mut w).is_none());
        assert!(matches!(w.last(), Some(Warning::MetadataDropped { .. })));
    }

    #[test]
    fn an_unreadable_source_packet_is_replaced_with_a_warning() {
        let mut w = Vec::new();
        let packet = for_output(Some("<x:xmpmeta><rdf:RDF"), &output(), None, &mut w).unwrap();
        assert!(properties(&packet).contains_key("xmp:CreatorTool"));
        assert_eq!(w, [Warning::UnreadableMetadata(MetadataKind::Xmp)]);
    }
}
