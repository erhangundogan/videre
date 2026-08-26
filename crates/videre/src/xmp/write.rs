//! Generate and write XMP sidecars for the portable marks (rating and colour
//! label). Pick and like have no XMP standard, so they are never exported.

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
