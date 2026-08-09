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


/// Types the embedding pipeline can decode.
pub const EMBEDDABLE_MIMES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/tiff",
    "image/heic",
    "video/quicktime",
    "video/mp4",
];

/// Types carrying EXIF metadata worth extracting.
pub const EXIF_MIMES: &[&str] = &["image/jpeg", "image/tiff", "image/heic"];

/// Types the perceptual hash can be computed for.
pub const PHASH_MIMES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/tiff",
    "video/quicktime",
    "video/mp4",
];

/// Types counted as photos by `library_stats`.
pub const PHOTO_MIMES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/tiff",
    "image/heic",
];

/// Types counted as videos.
pub const VIDEO_MIMES: &[&str] = &["video/quicktime", "video/mp4"];

/// The type a filename extension implies, used only when `mime` is NULL.
fn mime_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext.to_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        // DNG really is TIFF; `is_embeddable` vetoes it separately.
        "tiff" | "tif" | "dng" => "image/tiff",
        "heic" => "image/heic",
        "mov" => "video/quicktime",
        "mp4" | "m4v" => "video/mp4",
        _ => return None,
    })
}

/// The type to route on: the detected mime when known, else derived from the
/// extension.
///
/// The fallback is not compatibility support for older versions; it is how a
/// nullable column behaves before a library has been re-scanned.
pub fn effective_mime(mime: Option<&str>, ext: &str) -> Option<&'static str> {
    if let Some(m) = mime {
        // Normalise to a &'static str from the tables so callers compare
        // against the same values the constants hold.
        if let Some(known) = EMBEDDABLE_MIMES
            .iter()
            .chain(PHOTO_MIMES)
            .chain(VIDEO_MIMES)
            .find(|k| **k == m)
        {
            return Some(known);
        }
    }
    mime_for_ext(ext)
}

/// Whether `videre embed` should attempt this file.
///
/// `ext == "dng"` vetoes regardless of mime. DNG is a TIFF variant, so its
/// magic bytes are TIFF and TIFF is embeddable, but the `image` crate has no
/// DNG decoder: without this veto every DNG is queried as pending and fails
/// to decode on every run, forever. That exact bug was fixed 2026-08-01 by
/// excluding `dng` from the extension list, and routing on mime would revive
/// it.
pub fn is_embeddable(mime: Option<&str>, ext: &str) -> bool {
    if ext.eq_ignore_ascii_case("dng") {
        return false;
    }
    effective_mime(mime, ext).is_some_and(|m| EMBEDDABLE_MIMES.contains(&m))
}

/// Whether this is a video, for the single-frame QuickLook path.
pub fn is_video_mime(mime: &str) -> bool {
    VIDEO_MIMES.contains(&mime)
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

    #[test]
    fn effective_mime_prefers_the_detected_value() {
        assert_eq!(effective_mime(Some("image/jpeg"), "png"), Some("image/jpeg"));
    }

    #[test]
    fn effective_mime_falls_back_to_the_extension_when_null() {
        // mime is NULL until a library is re-scanned; without this fallback an
        // existing library reports zero photos and embeds nothing.
        assert_eq!(effective_mime(None, "jpg"), Some("image/jpeg"));
        assert_eq!(effective_mime(None, "MOV"), Some("video/quicktime"));
        assert_eq!(effective_mime(None, "xyz"), None);
    }

    #[test]
    fn a_misnamed_jpeg_is_embeddable() {
        // The real file this exists for: a JPEG named .png that fails every
        // embed run with "Invalid PNG signature".
        assert!(is_embeddable(Some("image/jpeg"), "png"));
    }

    #[test]
    fn dng_is_never_embeddable_even_though_it_reports_tiff() {
        // Regression guard for the 2026-08-01 fix. DNG is a TIFF variant, so
        // its magic bytes genuinely are TIFF, and tiff IS embeddable. Without
        // this veto every .dng is queried as pending and fails to decode, on
        // every run, forever: the `image` crate has no DNG decoder.
        assert!(!is_embeddable(Some("image/tiff"), "dng"));
        assert!(!is_embeddable(None, "dng"));
    }

    #[test]
    fn a_real_tiff_is_still_embeddable_so_the_veto_stays_narrow() {
        assert!(is_embeddable(Some("image/tiff"), "tiff"));
    }

    #[test]
    fn videos_are_embeddable_and_identified_as_video() {
        assert!(is_embeddable(Some("video/quicktime"), "mov"));
        assert!(is_video_mime("video/quicktime"));
        assert!(is_video_mime("video/mp4"));
        assert!(!is_video_mime("image/jpeg"));
    }

    #[test]
    fn an_unknown_type_is_not_embeddable() {
        assert!(!is_embeddable(None, "xyz"));
    }
}
