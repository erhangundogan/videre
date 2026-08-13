//! Shared vocabulary for saying *which* files a command should work on.
//!
//! Two selections exist, deliberately as separate types:
//!
//! - `RowSelection` for commands that query rows (`search`, `embed`, `faces`,
//!   `classify`, `locations`). It can filter on anything recorded.
//! - `PathSelection` for commands that walk a filesystem (`scan`, `watch`).
//!   It can only filter on what is knowable *before* a file is read.
//!
//! Keeping them separate is the point: adding a predicate to the row side
//! cannot change what `scan` accepts, because `scan` does not take that type.
//!
//! This module holds the primitives both share. The selections themselves and
//! their resolution follow in the same module.

use anyhow::{bail, Result};

/// The coarse kind of a media file.
///
/// `--type image` / `--type video`. A value flag rather than boolean
/// `--image`/`--video` flags, for symmetry with `--ext` and `--mime`, which
/// have no boolean form, and because it extends to further kinds without
/// growing the flag surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

impl MediaKind {
    /// Parse a user-supplied kind, naming every valid value on failure.
    ///
    /// An unknown *kind* is a typo worth reporting, unlike an unknown
    /// extension, which legitimately matches nothing.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "image" => Ok(Self::Image),
            "video" => Ok(Self::Video),
            other => bail!("unknown --type {other:?}; valid values are: image, video"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
        }
    }
}

/// Normalise a user-supplied extension so `.MOV`, `MOV` and `mov` agree.
///
/// Done once at parse time rather than per row: a selection is compared
/// against tens of thousands of files.
pub fn normalise_ext(s: &str) -> String {
    s.trim().trim_start_matches('.').to_lowercase()
}

/// Whether a row's type matches `kind`.
///
/// Resolves through `mime_probe::effective_mime`, which is what every other
/// consumer does: a file whose magic bytes could not be identified stores the
/// sentinel `application/octet-stream`, and `effective_mime` falls back to the
/// extension. A `.jpg` whose header was unreadable is still a photo to its
/// owner, and treating the sentinel as a type of its own would drop those files
/// out of `--type image` for a reason the user cannot see.
pub fn row_matches_kind(kind: MediaKind, mime: Option<&str>, ext: &str) -> bool {
    let Some(m) = crate::mime_probe::effective_mime(mime, &ext.to_lowercase()) else {
        return false;
    };
    match kind {
        MediaKind::Image => crate::mime_probe::PHOTO_MIMES.contains(&m),
        MediaKind::Video => crate::mime_probe::VIDEO_MIMES.contains(&m),
    }
}

/// Whether a *path* matches `kind`, judged by extension alone.
///
/// :warning: This deliberately differs from `row_matches_kind`. A walk decides
/// whether to read a file before it has read it, so the magic bytes are not
/// available and the extension is all there is. On a library whose extensions
/// are wrong, `scan --type video` and `search --type video` will disagree, and
/// that is inherent to filtering before reading rather than a bug to fix.
pub fn path_matches_kind(kind: MediaKind, path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext.is_empty() {
        return false;
    }
    row_matches_kind(kind, None, &ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_names_every_valid_value_on_a_typo() {
        assert_eq!(MediaKind::parse("image").unwrap(), MediaKind::Image);
        assert_eq!(MediaKind::parse("VIDEO").unwrap(), MediaKind::Video);
        assert_eq!(MediaKind::parse("  video  ").unwrap(), MediaKind::Video);
        let e = MediaKind::parse("vidoe").unwrap_err().to_string();
        assert!(e.contains("image") && e.contains("video"), "got: {e}");
    }

    #[test]
    fn extensions_normalise_to_one_spelling() {
        for s in [".MOV", "MOV", "mov", " .mov "] {
            assert_eq!(normalise_ext(s), "mov", "input {s:?}");
        }
    }

    #[test]
    fn an_unidentified_file_is_still_its_extension() {
        // The sentinel means "could not identify", not "a type of its own".
        // Dropping these from --type image would be invisible to the user.
        assert!(row_matches_kind(
            MediaKind::Image,
            Some(crate::mime_probe::UNKNOWN_MIME),
            "jpg"
        ));
        assert!(row_matches_kind(MediaKind::Video, None, "mov"));
    }

    #[test]
    fn mime_beats_a_wrong_extension_for_rows() {
        assert!(row_matches_kind(
            MediaKind::Image,
            Some("image/jpeg"),
            "txt"
        ));
        assert!(!row_matches_kind(
            MediaKind::Video,
            Some("image/jpeg"),
            "mov"
        ));
    }

    #[test]
    fn paths_are_judged_by_extension_only() {
        // The divergence from row matching, asserted on purpose: a walk has not
        // read the file, so this is all it can know.
        assert!(path_matches_kind(MediaKind::Video, Path::new("/a/b.mov")));
        assert!(path_matches_kind(MediaKind::Image, Path::new("/a/b.HEIC")));
        assert!(!path_matches_kind(MediaKind::Image, Path::new("/a/b.mov")));
        assert!(!path_matches_kind(MediaKind::Image, Path::new("/a/noext")));
    }
}
