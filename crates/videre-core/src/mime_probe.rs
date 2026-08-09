//! Identify a file's real type from its leading bytes.
//!
//! `file_hashes.ext` is the filename extension and nothing more, yet it drives
//! which decoder each file reaches. A misnamed file is routed by its name: a
//! real library contains a JPEG called `.png` that fails every `videre embed`
//! run with "Invalid PNG signature".
//!
//! Detection reads no I/O of its own. `hasher::hash_file_inner` already fills
//! a 64KB buffer to compute BLAKE3, and the signature lives in the first 12
//! bytes of it.
//!
//! See docs/superpowers/specs/2026-08-09-mime-detection-design.md.

/// Top-level boxes that identify a classic QuickTime file, which predates
/// ISO-BMFF's `ftyp` and may begin with any of these.
const QUICKTIME_BOXES: [&[u8; 4]; 7] =
    [b"ftyp", b"wide", b"mdat", b"moov", b"free", b"skip", b"pnot"];

/// The IANA type identified by `head`'s leading bytes, or None if
/// unrecognised. Needs at least 12 bytes; shorter input is always None.
///
/// Returns `&'static str` rather than an enum so the value stored in the
/// database and the value matched against are the same thing, with no mapping
/// table to drift.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.len() < 12 {
        return None;
    }
    if head.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if head.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if head.starts_with(b"II*\x00") || head.starts_with(b"MM\x00*") {
        return Some("image/tiff");
    }
    // RIFF alone is also WAV and AVI, so the WEBP tag is required.
    if head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    let box_type = &head[4..8];
    if box_type == b"ftyp" {
        let brand = &head[8..12];
        // HEIC and MP4 share the ftyp box; only the brand separates them.
        if matches!(brand, b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1") {
            return Some("image/heic");
        }
        if &brand[..2] == b"qt" {
            return Some("video/quicktime");
        }
        return Some("video/mp4");
    }
    if QUICKTIME_BOXES.iter().any(|b| box_type == *b) {
        return Some("video/quicktime");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ISO-BMFF header: 4 size bytes, then the box type, then a brand.
    fn iso(box_type: &[u8; 4], brand: &[u8; 4]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 0x20];
        v.extend_from_slice(box_type);
        v.extend_from_slice(brand);
        v
    }

    #[test]
    fn jpeg_is_detected() {
        assert_eq!(
            sniff(b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01"),
            Some("image/jpeg")
        );
    }

    #[test]
    fn png_is_detected() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0d"), Some("image/png"));
    }

    #[test]
    fn gif_both_versions_are_detected() {
        assert_eq!(sniff(b"GIF87a\x00\x00\x00\x00\x00\x00"), Some("image/gif"));
        assert_eq!(sniff(b"GIF89a\x00\x00\x00\x00\x00\x00"), Some("image/gif"));
    }

    #[test]
    fn bmp_is_detected() {
        assert_eq!(
            sniff(b"BM\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"),
            Some("image/bmp")
        );
    }

    #[test]
    fn tiff_both_endiannesses_are_detected() {
        assert_eq!(
            sniff(b"II*\x00\x08\x00\x00\x00\x00\x00\x00\x00"),
            Some("image/tiff")
        );
        assert_eq!(
            sniff(b"MM\x00*\x00\x00\x00\x08\x00\x00\x00\x00"),
            Some("image/tiff")
        );
    }

    #[test]
    fn webp_needs_both_riff_and_webp() {
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WEBP"), Some("image/webp"));
        // RIFF alone is also WAV and AVI; it must not claim those.
        assert_eq!(sniff(b"RIFF\x00\x00\x00\x00WAVE"), None);
    }

    #[test]
    fn heic_brands_are_images_not_video() {
        // HEIC and MP4 both carry `ftyp`; only the brand separates them.
        for brand in [b"heic", b"heix", b"hevc", b"hevx", b"mif1", b"msf1"] {
            assert_eq!(sniff(&iso(b"ftyp", brand)), Some("image/heic"), "{brand:?}");
        }
    }

    #[test]
    fn mp4_and_quicktime_brands_are_distinguished() {
        assert_eq!(sniff(&iso(b"ftyp", b"isom")), Some("video/mp4"));
        assert_eq!(sniff(&iso(b"ftyp", b"mp42")), Some("video/mp4"));
        assert_eq!(sniff(&iso(b"ftyp", b"qt  ")), Some("video/quicktime"));
    }

    #[test]
    fn classic_quicktime_without_ftyp_is_detected() {
        // 2.5% of a real library: 75 of 3,000 sampled .mov files begin with a
        // `wide` box then `mdat`, with no ftyp at all. `file(1)` calls them
        // plain data. A naive ftyp-only check leaves them NULL.
        for b in [b"wide", b"mdat", b"moov", b"free", b"skip", b"pnot"] {
            let mut v = vec![0, 0, 0, 8];
            v.extend_from_slice(b);
            v.extend_from_slice(&[0u8; 8]);
            assert_eq!(sniff(&v), Some("video/quicktime"), "{b:?}");
        }
    }

    #[test]
    fn short_input_is_none() {
        assert_eq!(sniff(b""), None);
        assert_eq!(
            sniff(b"\xff\xd8\xff"),
            None,
            "under 12 bytes is never enough"
        );
    }

    #[test]
    fn unrecognised_bytes_are_none() {
        assert_eq!(sniff(b"not a real file header at all"), None);
    }
}
