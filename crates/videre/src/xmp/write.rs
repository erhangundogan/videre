//! Generate and write XMP sidecars for videre's owned labels: rating, colour
//! label, resolved location, category/tag keywords, and MWG face regions. Pick
//! and like have no XMP standard, so they are never exported.
//!
//! The MWG-Regions shape emitted here was validated against exiftool (the MWG
//! reference parser digiKam/Lightroom/darktable rely on): a center-based
//! normalized `stArea` under `mwg-rs:RegionList`, with `AppliedToDimensions` in
//! pixels. See the XMP portability spec for the confirmed round-trip.

use crate::xmp::model::OwnedXmp;
use std::path::Path;

/// A minimal XMP packet carrying only the fields we own. Attribute form on a
/// single rdf:Description, which every reader (including ours) accepts.
pub fn sidecar_doc(rating: Option<i64>, label: Option<&str>) -> String {
    let mut attrs = String::new();
    if let Some(r) = rating {
        attrs.push_str(&format!(" xmp:Rating=\"{}\"", r.clamp(0, 5)));
    }
    if let Some(l) = label {
        attrs.push_str(&format!(" xmp:Label=\"{}\"", xml_escape(l)));
    }
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         <rdf:Description xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"{attrs}/>\n\
         </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>\n"
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Namespace prefix -> URI for every namespace the owned block can use. Declared
/// on the rdf:Description so every prefix below resolves.
const OWNED_NS: &[(&str, &str)] = &[
    ("xmp", "http://ns.adobe.com/xap/1.0/"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    (
        "Iptc4xmpCore",
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
    ),
    (
        "mwg-rs",
        "http://www.metadataworkinggroup.com/schemas/regions/",
    ),
    ("stArea", "http://ns.adobe.com/xmp/sType/Area#"),
    ("stDim", "http://ns.adobe.com/xap/1.0/sType/Dimensions#"),
];

/// Render the owned properties as XML property elements (no wrapping
/// rdf:Description), for use by both the fresh-packet builder and the merger.
pub fn owned_properties_xml(o: &OwnedXmp) -> String {
    let mut s = String::new();
    if let Some(r) = o.rating {
        s.push_str(&format!("   <xmp:Rating>{}</xmp:Rating>\n", r.clamp(0, 5)));
    }
    if let Some(l) = &o.label {
        s.push_str(&format!("   <xmp:Label>{}</xmp:Label>\n", xml_escape(l)));
    }
    if let Some(loc) = &o.location {
        s.push_str(&format!(
            "   <Iptc4xmpCore:Location>{}</Iptc4xmpCore:Location>\n",
            xml_escape(loc)
        ));
    }
    if !o.keywords.is_empty() {
        s.push_str("   <dc:subject><rdf:Bag>\n");
        for k in &o.keywords {
            s.push_str(&format!("    <rdf:li>{}</rdf:li>\n", xml_escape(k)));
        }
        s.push_str("   </rdf:Bag></dc:subject>\n");
    }
    if !o.regions.is_empty() {
        let (w, h) = o.applied_dims.unwrap_or((0, 0));
        s.push_str("   <mwg-rs:Regions rdf:parseType=\"Resource\">\n");
        s.push_str(&format!(
            "    <mwg-rs:AppliedToDimensions stDim:w=\"{w}\" stDim:h=\"{h}\" stDim:unit=\"pixel\"/>\n"
        ));
        s.push_str("    <mwg-rs:RegionList><rdf:Bag>\n");
        for r in &o.regions {
            s.push_str("     <rdf:li rdf:parseType=\"Resource\">\n");
            s.push_str(&format!(
                "      <mwg-rs:Name>{}</mwg-rs:Name>\n",
                xml_escape(&r.name)
            ));
            s.push_str("      <mwg-rs:Type>Face</mwg-rs:Type>\n");
            s.push_str(&format!(
                "      <mwg-rs:Area stArea:x=\"{:.6}\" stArea:y=\"{:.6}\" stArea:w=\"{:.6}\" stArea:h=\"{:.6}\" stArea:unit=\"normalized\"/>\n",
                r.area.cx, r.area.cy, r.area.w, r.area.h
            ));
            s.push_str("     </rdf:li>\n");
        }
        s.push_str("    </rdf:Bag></mwg-rs:RegionList>\n");
        s.push_str("   </mwg-rs:Regions>\n");
    }
    s
}

/// Build a complete, standalone XMP packet carrying only the owned properties.
/// Used when there is no existing sidecar to merge into.
pub fn build_packet(o: &OwnedXmp) -> String {
    let ns: String = OWNED_NS
        .iter()
        .map(|(p, u)| format!("\n    xmlns:{p}=\"{u}\""))
        .collect();
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         <rdf:Description rdf:about=\"\"{ns}>\n\
         {props}  </rdf:Description>\n\
         </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>\n",
        props = owned_properties_xml(o)
    )
}

/// The sidecar path for a photo: `<file>.<ext>.xmp`, matching what the reader
/// looks for.
pub fn sidecar_path(path: &Path) -> std::path::PathBuf {
    path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.xmp"),
        None => "xmp".to_string(),
    })
}

/// Write the sidecar for `path` if there is anything portable to write. Returns
/// whether a file was written.
pub fn write_sidecar(
    path: &Path,
    rating: Option<i64>,
    label: Option<&str>,
) -> std::io::Result<bool> {
    if rating.is_none() && label.is_none() {
        return Ok(false);
    }
    std::fs::write(sidecar_path(path), sidecar_doc(rating, label))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_full_packet_that_reads_back() {
        use crate::xmp::model::{Area, OwnedXmp, Region};
        let owned = OwnedXmp {
            rating: Some(4),
            label: Some("Red".into()),
            location: Some("Kadıköy".into()),
            keywords: vec!["photo".into(), "beach".into()],
            regions: vec![Region {
                name: "Ayşe".into(),
                area: Area {
                    cx: 0.35,
                    cy: 0.35,
                    w: 0.2,
                    h: 0.3,
                },
            }],
            applied_dims: Some((4000, 3000)),
        };
        let doc = build_packet(&owned);
        // Round-trips through our own reader for the marks it already understands.
        let m = crate::xmp::read::parse_xmp(&doc);
        assert_eq!(m.rating, Some(4));
        assert_eq!(m.label.as_deref(), Some("Red"));
        // And the structural facts a third-party reader needs are present, in the
        // exact shape validated against exiftool (the MWG reference parser).
        assert!(doc.contains("mwg-rs:Regions"));
        assert!(doc.contains("<mwg-rs:Name>Ayşe</mwg-rs:Name>"));
        assert!(doc.contains(r#"stArea:unit="normalized""#));
        assert!(doc.contains(r#"stDim:w="4000""#));
        assert!(doc.contains("<Iptc4xmpCore:Location>Kadıköy</Iptc4xmpCore:Location>"));
        assert!(doc.contains("<rdf:li>photo</rdf:li>"));
    }

    /// The sample OwnedXmp behind the committed golden sidecar. Kept in one place
    /// so the golden test and any manual exiftool re-validation use identical data.
    fn golden_sample() -> crate::xmp::model::OwnedXmp {
        use crate::xmp::model::{Area, OwnedXmp, Region};
        OwnedXmp {
            rating: Some(4),
            label: Some("Red".into()),
            location: Some("Kadıköy".into()),
            keywords: vec!["photo".into(), "beach".into()],
            regions: vec![Region {
                name: "Ayşe".into(),
                area: Area {
                    cx: 0.35,
                    cy: 0.35,
                    w: 0.2,
                    h: 0.3,
                },
            }],
            applied_dims: Some((4000, 3000)),
        }
    }

    /// Golden test: `build_packet` output must equal the committed reference
    /// sidecar, whose MWG shape was validated against exiftool. Set
    /// `VIDERE_UPDATE_GOLDEN=1` to regenerate it after an intentional change, then
    /// re-run exiftool on it (see tests/fixtures/xmp/README) before committing.
    #[test]
    fn build_packet_matches_exiftool_validated_golden() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/xmp/mwg-reference.xmp"
        );
        let doc = build_packet(&golden_sample());
        if std::env::var("VIDERE_UPDATE_GOLDEN").is_ok() {
            std::fs::write(path, &doc).unwrap();
        }
        let want = std::fs::read_to_string(path).expect("golden fixture missing");
        assert_eq!(doc, want, "build_packet drifted from the validated golden");
    }

    #[test]
    fn sidecar_roundtrips_through_read() {
        let doc = sidecar_doc(Some(4), Some("Red"));
        let m = crate::xmp::read::parse_xmp(&doc);
        assert_eq!(m.rating, Some(4));
        assert_eq!(m.label.as_deref(), Some("Red"));
    }

    #[test]
    fn omits_absent_fields() {
        let doc = sidecar_doc(Some(2), None);
        assert!(doc.contains("xmp:Rating"));
        assert!(!doc.contains("xmp:Label"));
    }

    #[test]
    fn escapes_a_label() {
        let doc = sidecar_doc(None, Some(r#"a&b"c"#));
        assert!(doc.contains("a&amp;b&quot;c"));
        // and it survives a round-trip back to the original text
        assert_eq!(
            crate::xmp::read::parse_xmp(&doc).label.as_deref(),
            Some(r#"a&b"c"#)
        );
    }
}
