//! Rotating a photo 90 degrees clockwise by editing its stored EXIF
//! Orientation tag, rather than re-encoding pixels: the gallery lightbox's
//! rotate button turns a sideways scan upright without a quality loss, and the
//! change is reversible by rotating back.
//!
//! Only formats that carry an EXIF Orientation tag are supported (JPEG and the
//! PNG/TIFF/WebP family); videos and everything else are refused so the button
//! is never offered where it cannot work.

use anyhow::Context;
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use std::path::Path;

/// The formats whose orientation this rotates. The gallery hides the button for
/// anything else, and the endpoint refuses it, so the two stay in agreement.
pub fn supports_exif_orientation(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp"
    )
}

/// The EXIF Orientation value after one 90-degrees-clockwise turn of the
/// displayed image. The rotation group cycles 1 -> 6 -> 3 -> 8 -> 1; the
/// mirrored group cycles 2 -> 5 -> 4 -> 7 -> 2. Any out-of-range value is
/// treated as 1 (normal), so a missing or corrupt tag becomes a clean 6.
pub fn next_orientation_cw(current: u16) -> u16 {
    match current {
        1 => 6,
        6 => 3,
        3 => 8,
        8 => 1,
        2 => 5,
        5 => 4,
        4 => 7,
        7 => 2,
        _ => 6,
    }
}

/// Read the current Orientation tag, defaulting to 1 when the file carries
/// none (or none can be parsed).
fn current_orientation(path: &Path) -> u16 {
    let Ok(metadata) = Metadata::new_from_path(path) else {
        return 1;
    };
    for tag in metadata.get_tag(&ExifTag::Orientation(Vec::new())) {
        if let ExifTag::Orientation(values) = tag {
            if let Some(&value) = values.first() {
                return value;
            }
        }
    }
    1
}

/// Rotate the file 90 degrees clockwise by writing the next Orientation value
/// into its EXIF, in place. Returns the new orientation. Refuses a format that
/// does not carry EXIF orientation.
pub fn rotate_cw_in_place(path: &Path, ext: &str) -> anyhow::Result<u16> {
    if !supports_exif_orientation(ext) {
        anyhow::bail!("rotation is not supported for .{ext} files");
    }
    let next = next_orientation_cw(current_orientation(path));
    let mut metadata = Metadata::new_from_path(path)
        .with_context(|| format!("reading EXIF from {}", path.display()))?;
    metadata.set_tag(ExifTag::Orientation(vec![next]));
    metadata
        .write_to_file(path)
        .with_context(|| format!("writing EXIF orientation to {}", path.display()))?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cw_cycles_through_both_orientation_groups() {
        // Rotation group returns to start after four turns.
        assert_eq!(next_orientation_cw(1), 6);
        assert_eq!(next_orientation_cw(6), 3);
        assert_eq!(next_orientation_cw(3), 8);
        assert_eq!(next_orientation_cw(8), 1);
        // Mirrored group likewise.
        assert_eq!(next_orientation_cw(2), 5);
        assert_eq!(next_orientation_cw(5), 4);
        assert_eq!(next_orientation_cw(4), 7);
        assert_eq!(next_orientation_cw(7), 2);
        // A missing or bogus value is treated as normal, so one turn gives 6.
        assert_eq!(next_orientation_cw(0), 6);
        assert_eq!(next_orientation_cw(99), 6);
    }

    #[test]
    fn only_exif_bearing_formats_are_supported() {
        for ext in ["jpg", "JPG", "jpeg", "png", "tif", "tiff", "webp"] {
            assert!(supports_exif_orientation(ext), "{ext} should be supported");
        }
        for ext in ["mp4", "mov", "heic", "dng", "gif", "bmp", ""] {
            assert!(!supports_exif_orientation(ext), "{ext} should be refused");
        }
    }

    #[test]
    fn rotate_writes_and_reads_back_a_bumped_orientation() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tiny.jpg");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        std::fs::copy(src, &path).unwrap();

        // Whatever the fixture's starting orientation, a turn advances it by the
        // CW table and the written value reads back.
        let before = current_orientation(&path);
        let after = rotate_cw_in_place(&path, "jpg").unwrap();
        assert_eq!(after, next_orientation_cw(before));
        assert_eq!(current_orientation(&path), after);
        // A second turn advances again from the freshly written value.
        let after2 = rotate_cw_in_place(&path, "jpg").unwrap();
        assert_eq!(after2, next_orientation_cw(after));
        assert_eq!(current_orientation(&path), after2);
    }

    #[test]
    fn rotate_refuses_a_video() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"not a real mp4").unwrap();
        assert!(rotate_cw_in_place(&path, "mp4").is_err());
    }
}
