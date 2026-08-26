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

/// The (namespace-URI, local-name) pairs videre owns. Any existing element or
/// attribute under an rdf:Description matching one of these is removed before
/// ours is inserted, so a re-export never doubles a property.
/// `lr:hierarchicalSubject` is listed now so a later tags change need not touch
/// this set again.
const OWNED_PROPS: &[(&str, &str)] = &[
    ("http://ns.adobe.com/xap/1.0/", "Rating"),
    ("http://ns.adobe.com/xap/1.0/", "Label"),
    ("http://purl.org/dc/elements/1.1/", "subject"),
    ("http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/", "Location"),
    (
        "http://www.metadataworkinggroup.com/schemas/regions/",
        "Regions",
    ),
    ("http://ns.adobe.com/lightroom/1.0/", "hierarchicalSubject"),
];

const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

fn is_owned(ns: &str, name: &str) -> bool {
    OWNED_PROPS.iter().any(|(u, n)| *u == ns && *n == name)
}

/// Apply non-overlapping `(start, end, replacement)` edits to `s`, highest start
/// first so earlier offsets stay valid.
fn apply_edits(mut s: String, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, end, repl) in edits {
        s.replace_range(start..end, &repl);
    }
    s
}

/// Merge owned properties into an existing sidecar's text. Removes only
/// videre-owned properties (in element OR attribute form, across every
/// rdf:Description), preserves all foreign content and its namespace
/// declarations verbatim, and inserts the freshly built owned block into the
/// first rdf:Description. Best effort: if the text cannot be parsed or has no
/// rdf:Description, fall back to a fresh packet, so a merge never fails an export.
pub fn merge_into(existing: &str, o: &OwnedXmp) -> String {
    // Pass 1: locate and delete owned props (elements and attributes) everywhere.
    let out = {
        let Ok(doc) = roxmltree::Document::parse(existing) else {
            return build_packet(o);
        };
        let descs: Vec<_> = doc
            .descendants()
            .filter(|n| {
                n.tag_name().name() == "Description" && n.tag_name().namespace() == Some(RDF_NS)
            })
            .collect();
        if descs.is_empty() {
            return build_packet(o);
        }
        let mut cuts: Vec<(usize, usize, String)> = Vec::new();
        for desc in &descs {
            for child in desc.children().filter(|n| n.is_element()) {
                if is_owned(
                    child.tag_name().namespace().unwrap_or(""),
                    child.tag_name().name(),
                ) {
                    let r = child.range();
                    cuts.push((r.start, r.end, String::new()));
                }
            }
            for attr in desc.attributes() {
                if is_owned(attr.namespace().unwrap_or(""), attr.name()) {
                    let mut r = attr.range();
                    // Eat one leading space so we do not leave a double gap.
                    if r.start > 0 && existing.as_bytes()[r.start - 1] == b' ' {
                        r.start -= 1;
                    }
                    cuts.push((r.start, r.end, String::new()));
                }
            }
        }
        apply_edits(existing.to_string(), cuts)
    };

    // Pass 2: insert owned block + any missing owned namespaces into the first
    // rdf:Description of the trimmed text. Re-parse so offsets are valid.
    let (props, missing_ns, tag_open_gt, self_closing, close_lt, desc_start) = {
        let Ok(doc) = roxmltree::Document::parse(&out) else {
            return build_packet(o);
        };
        let Some(desc) = doc.descendants().find(|n| {
            n.tag_name().name() == "Description" && n.tag_name().namespace() == Some(RDF_NS)
        }) else {
            return build_packet(o);
        };
        let declared: std::collections::HashSet<&str> =
            desc.namespaces().filter_map(|ns| ns.name()).collect();
        let missing_ns: String = OWNED_NS
            .iter()
            .filter(|(p, _)| !declared.contains(*p))
            .map(|(p, u)| format!(" xmlns:{p}=\"{u}\""))
            .collect();
        let desc_start = desc.range().start;
        let gt = out[desc_start..]
            .find('>')
            .map(|i| desc_start + i)
            .unwrap_or(out.len());
        let self_closing = gt > desc_start && out.as_bytes()[gt - 1] == b'/';
        let desc_end = desc.range().end;
        let close_lt = out[..desc_end].rfind('<').unwrap_or(desc_end);
        (
            owned_properties_xml(o),
            missing_ns,
            gt,
            self_closing,
            close_lt,
            desc_start,
        )
    };
    let _ = desc_start;

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    if self_closing {
        // Replace the trailing `/>` with `{ns}>\n{props}  </rdf:Description>\n`.
        let slash = tag_open_gt - 1;
        edits.push((
            slash,
            tag_open_gt + 1,
            format!("{missing_ns}>\n{props}  </rdf:Description>\n"),
        ));
    } else {
        edits.push((tag_open_gt, tag_open_gt, missing_ns));
        edits.push((close_lt, close_lt, props));
    }
    apply_edits(out, edits)
}

/// The sidecar path for a photo: `<file>.<ext>.xmp`, matching what the reader
/// looks for.
pub fn sidecar_path(path: &Path) -> std::path::PathBuf {
    path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.xmp"),
        None => "xmp".to_string(),
    })
}

/// Write (creating or merging) the sidecar for `path` from its owned properties.
/// Reads any existing sidecar and merges so foreign data is preserved. Returns
/// whether a file was written (false when there is nothing owned to write).
pub fn write_sidecar(path: &Path, o: &OwnedXmp) -> std::io::Result<bool> {
    if o.is_empty() {
        return Ok(false);
    }
    let side = sidecar_path(path);
    let doc = match std::fs::read_to_string(&side) {
        Ok(existing) => merge_into(&existing, o),
        Err(_) => build_packet(o),
    };
    std::fs::write(&side, doc)?;
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
    fn merge_preserves_foreign_props_and_replaces_owned_element_form() {
        use crate::xmp::model::OwnedXmp;
        let existing = r#"<?xpacket begin="﻿"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
 xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about=""
   xmlns:xmp="http://ns.adobe.com/xap/1.0/"
   xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
  <xmp:Rating>2</xmp:Rating>
  <crs:Temperature>5200</crs:Temperature>
 </rdf:Description>
</rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;
        let owned = OwnedXmp {
            rating: Some(5),
            keywords: vec!["beach".into()],
            ..Default::default()
        };
        let merged = merge_into(existing, &owned);
        assert!(merged.contains("<crs:Temperature>5200</crs:Temperature>")); // foreign survives
        assert!(!merged.contains("<xmp:Rating>2</xmp:Rating>")); // old rating gone
        assert_eq!(merged.matches("<xmp:Rating>5</xmp:Rating>").count(), 1); // new, once
        assert!(merged.contains("<rdf:li>beach</rdf:li>"));
        assert!(merged.contains(r#"xmlns:dc="http://purl.org/dc/elements/1.1/""#)); // dc declared
        assert!(roxmltree::Document::parse(&merged).is_ok()); // still well-formed
    }

    #[test]
    fn merge_strips_owned_attribute_form_rating() {
        use crate::xmp::model::OwnedXmp;
        // Lightroom writes xmp:Rating as an ATTRIBUTE on rdf:Description.
        let existing = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
 xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/"
   xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   xmp:Rating="2" crs:Contrast="10">
  <crs:Temperature>5200</crs:Temperature>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let owned = OwnedXmp {
            rating: Some(5),
            ..Default::default()
        };
        let merged = merge_into(existing, &owned);
        // Old attribute rating removed; no attribute rating survives.
        assert!(!merged.contains(r#"xmp:Rating="2""#));
        assert!(!merged.contains(r#"xmp:Rating="5""#));
        // New rating present exactly once, as an element.
        assert_eq!(merged.matches("<xmp:Rating>5</xmp:Rating>").count(), 1);
        // Foreign attribute and element both survive.
        assert!(merged.contains(r#"crs:Contrast="10""#));
        assert!(merged.contains("<crs:Temperature>5200</crs:Temperature>"));
        assert!(roxmltree::Document::parse(&merged).is_ok());
    }

    #[test]
    fn merge_preserves_a_real_exiftool_written_sidecar() {
        use crate::xmp::model::{Area, OwnedXmp, Region};
        // A genuine third-party sidecar produced by exiftool (the MWG reference
        // writer): multiple rdf:Description blocks, one per namespace, and nested
        // element-form MWG areas - a structure quite unlike what videre emits.
        let existing = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/xmp/thirdparty-lightroom.xmp"
        ))
        .unwrap();
        let owned = OwnedXmp {
            rating: Some(5),
            keywords: vec!["portrait".into()],
            regions: vec![Region {
                name: "Ayşe".into(),
                area: Area {
                    cx: 0.5,
                    cy: 0.5,
                    w: 0.1,
                    h: 0.1,
                },
            }],
            applied_dims: Some((6000, 4000)),
            ..Default::default()
        };
        let merged = merge_into(&existing, &owned);

        // Foreign Camera Raw develop setting survives untouched.
        assert!(merged.contains("<crs:Contrast>25</crs:Contrast>"));
        // Owned rating replaced: the exiftool value is gone, ours present once.
        assert!(!merged.contains("<xmp:Rating>3</xmp:Rating>"));
        assert_eq!(merged.matches("<xmp:Rating>5</xmp:Rating>").count(), 1);
        // The old face region and keywords (owned) are replaced by ours, not doubled.
        assert!(!merged.contains("<mwg-rs:Name>Gökhan</mwg-rs:Name>"));
        assert!(merged.contains("<mwg-rs:Name>Ayşe</mwg-rs:Name>"));
        assert!(!merged.contains("<rdf:li>holiday</rdf:li>"));
        assert!(merged.contains("<rdf:li>portrait</rdf:li>"));
        // And the merged document is still well-formed XML.
        assert!(roxmltree::Document::parse(&merged).is_ok());
        // Our reader can read the new rating back out of the merged result.
        assert_eq!(crate::xmp::read::parse_xmp(&merged).rating, Some(5));
    }

    #[test]
    fn merge_on_unparseable_falls_back_to_fresh_packet() {
        use crate::xmp::model::OwnedXmp;
        let owned = OwnedXmp {
            rating: Some(3),
            ..Default::default()
        };
        let merged = merge_into("not xml at all", &owned);
        assert!(merged.contains("<xmp:Rating>3</xmp:Rating>"));
        assert!(merged.contains("<x:xmpmeta"));
    }

    #[test]
    fn omits_absent_fields() {
        use crate::xmp::model::OwnedXmp;
        let doc = build_packet(&OwnedXmp {
            rating: Some(2),
            ..Default::default()
        });
        assert!(doc.contains("xmp:Rating"));
        assert!(!doc.contains("xmp:Label"));
        assert!(!doc.contains("Iptc4xmpCore:Location"));
        assert!(!doc.contains("mwg-rs:Regions"));
    }

    #[test]
    fn escapes_a_label() {
        use crate::xmp::model::OwnedXmp;
        let doc = build_packet(&OwnedXmp {
            label: Some(r#"a&b"c"#.into()),
            ..Default::default()
        });
        assert!(doc.contains("a&amp;b&quot;c"));
        // and it survives a round-trip back to the original text
        assert_eq!(
            crate::xmp::read::parse_xmp(&doc).label.as_deref(),
            Some(r#"a&b"c"#)
        );
    }

    #[test]
    fn write_sidecar_merges_when_one_exists() {
        use crate::xmp::model::OwnedXmp;
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("p.jpg");
        std::fs::write(&photo, b"not-a-real-jpeg").unwrap();
        let side = sidecar_path(&photo);
        std::fs::write(
            &side,
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
 xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
  <crs:Temperature>5200</crs:Temperature>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        let owned = OwnedXmp {
            rating: Some(3),
            ..Default::default()
        };
        assert!(write_sidecar(&photo, &owned).unwrap());
        let back = std::fs::read_to_string(&side).unwrap();
        assert!(back.contains("crs:Temperature")); // foreign survived
        assert!(back.contains("<xmp:Rating>3</xmp:Rating>"));
    }

    #[test]
    fn write_sidecar_skips_when_nothing_owned() {
        use crate::xmp::model::OwnedXmp;
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("p.jpg");
        std::fs::write(&photo, b"x").unwrap();
        assert!(!write_sidecar(&photo, &OwnedXmp::default()).unwrap());
        assert!(!sidecar_path(&photo).exists());
    }
}
