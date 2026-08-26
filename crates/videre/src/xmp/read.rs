//! Read rating and colour label from XMP. Two sources, checked in order:
//! an adjacent `<file>.xmp` sidecar, then the embedded XMP packet. kamadak-exif
//! reads EXIF, not XMP, so this is separate. Best-effort: any parse failure
//! yields no marks rather than an error, so a malformed packet never fails a scan.

use std::path::Path;

const XMP_NS: &str = "http://ns.adobe.com/xap/1.0/";
const MWG_RS_NS: &str = "http://www.metadataworkinggroup.com/schemas/regions/";
const STAREA_NS: &str = "http://ns.adobe.com/xmp/sType/Area#";
const DC_NS: &str = "http://purl.org/dc/elements/1.1/";

#[derive(Debug, Default, PartialEq)]
pub struct XmpMarks {
    pub rating: Option<i64>,
    pub label: Option<String>,
}

/// One region read from a sidecar: a display name and a center-based normalized
/// area (0..1), whichever encoding the file used.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadRegion {
    pub name: String,
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
}

/// Everything the reader can extract: the marks (rating/label), MWG face regions,
/// and dc:subject keywords. A superset of `XmpMarks` for callers that also want
/// regions and keywords.
#[derive(Debug, Default, PartialEq)]
pub struct XmpData {
    pub rating: Option<i64>,
    pub label: Option<String>,
    pub regions: Vec<ReadRegion>,
    pub keywords: Vec<String>,
}

/// Parse an XMP document string for xmp:Rating and xmp:Label, in either the
/// attribute (`xmp:Rating="4"`) or element (`<xmp:Rating>4</xmp:Rating>`) form.
pub fn parse_xmp(doc: &str) -> XmpMarks {
    let Ok(tree) = roxmltree::Document::parse(doc) else {
        return XmpMarks::default();
    };
    let mut m = XmpMarks::default();
    for node in tree.descendants() {
        // attribute form, usually on rdf:Description
        for attr in node.attributes() {
            if attr.namespace() != Some(XMP_NS) {
                continue;
            }
            match attr.name() {
                "Rating" => m.rating = attr.value().trim().parse().ok().map(|r: i64| r.clamp(0, 5)),
                "Label" => {
                    let v = attr.value().trim();
                    if !v.is_empty() {
                        m.label = Some(v.to_string());
                    }
                }
                _ => {}
            }
        }
        // element form
        if node.tag_name().namespace() == Some(XMP_NS) {
            let text = node.text().map(str::trim).unwrap_or("");
            match node.tag_name().name() {
                "Rating" => m.rating = text.parse().ok().map(|r: i64| r.clamp(0, 5)),
                "Label" if !text.is_empty() => m.label = Some(text.to_string()),
                _ => {}
            }
        }
    }
    m
}

/// Read one `stArea` coordinate from an `mwg-rs:Area` node, accepting both the
/// attribute form (`stArea:x="0.4"`) and the nested element form
/// (`<stArea:x>0.4</stArea:x>`, which exiftool writes).
fn area_coord(area: &roxmltree::Node, name: &str) -> Option<f64> {
    if let Some(v) = area
        .attributes()
        .find(|a| a.namespace() == Some(STAREA_NS) && a.name() == name)
    {
        return v.value().trim().parse().ok();
    }
    area.children()
        .find(|c| c.tag_name().namespace() == Some(STAREA_NS) && c.tag_name().name() == name)
        .and_then(|c| c.text())
        .and_then(|t| t.trim().parse().ok())
}

/// Parse rating, label, MWG face regions and dc:subject keywords. Best-effort: a
/// missing or malformed field yields nothing for that field, never an error.
/// Handles both region encodings (videre's attribute-form `stArea` and exiftool's
/// nested element-form).
pub fn parse_xmp_data(doc: &str) -> XmpData {
    let marks = parse_xmp(doc); // reuse the existing rating/label parser
    let mut d = XmpData {
        rating: marks.rating,
        label: marks.label,
        ..Default::default()
    };
    let Ok(tree) = roxmltree::Document::parse(doc) else {
        return d;
    };
    for node in tree.descendants() {
        let tn = node.tag_name();
        // dc:subject bag -> keywords
        if tn.namespace() == Some(DC_NS) && tn.name() == "subject" {
            for li in node.descendants().filter(|n| n.tag_name().name() == "li") {
                if let Some(t) = li.text() {
                    let t = t.trim();
                    if !t.is_empty() {
                        d.keywords.push(t.to_string());
                    }
                }
            }
        }
        // An rdf:li that carries an mwg-rs:Name and an mwg-rs:Area is a region.
        if tn.name() == "li" {
            let name = node
                .children()
                .find(|c| {
                    c.tag_name().namespace() == Some(MWG_RS_NS) && c.tag_name().name() == "Name"
                })
                .and_then(|c| c.text())
                .map(|s| s.trim().to_string());
            let area = node.children().find(|c| {
                c.tag_name().namespace() == Some(MWG_RS_NS) && c.tag_name().name() == "Area"
            });
            if let (Some(name), Some(area)) = (name, area) {
                if !name.is_empty() {
                    if let (Some(cx), Some(cy), Some(w), Some(h)) = (
                        area_coord(&area, "x"),
                        area_coord(&area, "y"),
                        area_coord(&area, "w"),
                        area_coord(&area, "h"),
                    ) {
                        d.regions.push(ReadRegion { name, cx, cy, w, h });
                    }
                }
            }
        }
    }
    d
}

/// Read marks for a photo: sidecar first, then the embedded packet. Never errors.
pub fn read_marks(path: &Path) -> XmpMarks {
    let sidecar = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.xmp"),
        None => "xmp".to_string(),
    });
    if let Ok(doc) = std::fs::read_to_string(&sidecar) {
        let m = parse_xmp(&doc);
        if m != XmpMarks::default() {
            return m;
        }
    }
    if let Some(doc) = embedded_packet(path) {
        return parse_xmp(&doc);
    }
    XmpMarks::default()
}

/// Read the full XMP data for a photo: sidecar first, then the embedded packet,
/// mirroring `read_marks`. Never errors; a missing or malformed source yields an
/// empty `XmpData`.
pub fn read_data(path: &Path) -> XmpData {
    let sidecar = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.xmp"),
        None => "xmp".to_string(),
    });
    if let Ok(doc) = std::fs::read_to_string(&sidecar) {
        let d = parse_xmp_data(&doc);
        if d != XmpData::default() {
            return d;
        }
    }
    if let Some(doc) = embedded_packet(path) {
        return parse_xmp_data(&doc);
    }
    XmpData::default()
}

/// Extract the embedded XMP packet from a file's bytes, best-effort: find the
/// `<x:xmpmeta ...> ... </x:xmpmeta>` span. Works across JPEG/HEIC/PNG because
/// the packet is stored as UTF-8 text regardless of container.
fn embedded_packet(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let start = text.find("<x:xmpmeta")?;
    let end = text[start..].find("</x:xmpmeta>")? + start + "</x:xmpmeta>".len();
    Some(text[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_region_from_the_real_exiftool_fixture() {
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/xmp/thirdparty-lightroom.xmp"
        ))
        .unwrap();
        let d = parse_xmp_data(&doc);
        // exiftool wrote nested element-form stArea and a single named region.
        assert_eq!(d.regions.len(), 1);
        assert_eq!(d.regions[0].name, "Gökhan");
        assert!((d.regions[0].cx - 0.40).abs() < 1e-6);
        assert!((d.regions[0].cy - 0.30).abs() < 1e-6);
        assert!((d.regions[0].w - 0.15).abs() < 1e-6);
        assert!((d.regions[0].h - 0.20).abs() < 1e-6);
        // keywords parse too (their destination is a later change).
        assert!(d.keywords.contains(&"holiday".to_string()));
        assert!(d.keywords.contains(&"beach".to_string()));
        // and the marks still read.
        assert_eq!(d.rating, Some(3));
    }

    #[test]
    fn reads_a_region_from_attribute_form() {
        // videre's own writer emits attribute-form stArea.
        let doc = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
 xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about=""
   xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
   xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
  <mwg-rs:Regions rdf:parseType="Resource"><mwg-rs:RegionList><rdf:Bag>
   <rdf:li rdf:parseType="Resource">
    <mwg-rs:Name>Ayşe</mwg-rs:Name>
    <mwg-rs:Type>Face</mwg-rs:Type>
    <mwg-rs:Area stArea:x="0.5" stArea:y="0.4" stArea:w="0.2" stArea:h="0.25" stArea:unit="normalized"/>
   </rdf:li>
  </rdf:Bag></mwg-rs:RegionList></mwg-rs:Regions>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let d = parse_xmp_data(doc);
        assert_eq!(d.regions.len(), 1);
        assert_eq!(d.regions[0].name, "Ayşe");
        assert!((d.regions[0].cx - 0.5).abs() < 1e-6);
        assert!((d.regions[0].h - 0.25).abs() < 1e-6);
    }

    #[test]
    fn garbage_yields_no_regions_not_an_error() {
        assert_eq!(parse_xmp_data("not xml"), XmpData::default());
    }

    #[test]
    fn read_data_reads_a_region_from_an_adjacent_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("IMG.jpg");
        std::fs::write(&photo, b"not-a-real-jpeg").unwrap();
        std::fs::write(
            dir.path().join("IMG.jpg.xmp"),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
 xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about=""
   xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
   xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
  <mwg-rs:Regions rdf:parseType="Resource"><mwg-rs:RegionList><rdf:Bag>
   <rdf:li rdf:parseType="Resource"><mwg-rs:Name>Ayşe</mwg-rs:Name>
    <mwg-rs:Area stArea:x="0.5" stArea:y="0.5" stArea:w="0.2" stArea:h="0.2"/>
   </rdf:li>
  </rdf:Bag></mwg-rs:RegionList></mwg-rs:Regions>
 </rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        let d = read_data(&photo);
        assert_eq!(d.regions.len(), 1);
        assert_eq!(d.regions[0].name, "Ayşe");
    }

    const SIDECAR: &str = r#"<?xpacket begin="?"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
 xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
 xmlns:xmp="http://ns.adobe.com/xap/1.0/">
 <rdf:Description xmp:Rating="4" xmp:Label="Red"/>
</rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;

    #[test]
    fn parses_attribute_form() {
        let m = parse_xmp(SIDECAR);
        assert_eq!(
            m,
            XmpMarks {
                rating: Some(4),
                label: Some("Red".into())
            }
        );
    }

    #[test]
    fn parses_element_form() {
        let x = r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><xmp:Rating>5</xmp:Rating></rdf:Description>"#;
        assert_eq!(parse_xmp(x).rating, Some(5));
    }

    #[test]
    fn rating_is_clamped() {
        let x = r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="9"/>"#;
        assert_eq!(parse_xmp(x).rating, Some(5));
    }

    #[test]
    fn garbage_is_no_marks_not_an_error() {
        assert_eq!(parse_xmp("not xml at all"), XmpMarks::default());
    }
}
