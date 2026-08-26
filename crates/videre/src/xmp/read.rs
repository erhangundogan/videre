//! Read rating and colour label from XMP. Two sources, checked in order:
//! an adjacent `<file>.xmp` sidecar, then the embedded XMP packet. kamadak-exif
//! reads EXIF, not XMP, so this is separate. Best-effort: any parse failure
//! yields no marks rather than an error, so a malformed packet never fails a scan.

use std::path::Path;

const XMP_NS: &str = "http://ns.adobe.com/xap/1.0/";

#[derive(Debug, Default, PartialEq)]
pub struct XmpMarks {
    pub rating: Option<i64>,
    pub label: Option<String>,
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
