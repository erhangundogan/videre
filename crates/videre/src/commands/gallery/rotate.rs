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

/// The display-canvas dimensions of `path` as it decodes *right now*, before any
/// rotation: the raw pixel dimensions with the current EXIF Orientation applied.
/// A transposing orientation (5-8) swaps width and height, so a landscape sensor
/// buffer displayed portrait reports its portrait dimensions here.
///
/// Used to transform stored face geometry when the orientation is bumped: the
/// bbox/landmark coordinates are in this pre-rotation display canvas, so the turn
/// that maps a point to the new canvas needs this canvas's height. Returns `None`
/// if the dimensions cannot be read.
pub fn current_display_dimensions(path: &Path) -> Option<(u32, u32)> {
    let (w, h) = image::image_dimensions(path).ok()?;
    if matches!(current_orientation(path), 5..=8) {
        Some((h, w))
    } else {
        Some((w, h))
    }
}

/// Rotate an `"x,y,w,h"` bbox string (integers, as `videre faces` writes them)
/// 90 degrees clockwise on a display canvas of height `display_h` pixels, giving
/// its position in the canvas after one clockwise turn. A point `(x, y)` maps to
/// `(display_h - y, x)`; the box's width and height swap. Returns `None` if the
/// string is not four integers.
pub fn rotate_bbox_cw(bbox: &str, display_h: i32) -> Option<String> {
    let v: Vec<i32> = bbox
        .split(',')
        .map(|s| s.trim().parse().ok())
        .collect::<Option<_>>()?;
    if v.len() != 4 {
        return None;
    }
    let (x, y, w, h) = (v[0], v[1], v[2], v[3]);
    // Top-left corner (x, y) turns to (display_h - y, x); the opposite corner
    // (x+w, y+h) turns to (display_h - y - h, x + w). The new top-left is the
    // smaller of the two, so x' = display_h - y - h, y' = x, and the sides swap.
    Some(format!("{},{},{},{}", display_h - y - h, x, h, w))
}

/// Rotate a `"x1,y1,...,x5,y5"` landmark string 90 degrees clockwise on a
/// display canvas of height `display_h`, mapping each point `(x, y)` to
/// `(display_h - y, x)`. Returns `None` if the string is not an even, non-empty
/// list of floats. Coordinates are formatted like the detector writes them
/// (default float `Display`).
pub fn rotate_landmark_cw(landmark: &str, display_h: f32) -> Option<String> {
    let v: Vec<f32> = landmark
        .split(',')
        .map(|s| s.trim().parse().ok())
        .collect::<Option<_>>()?;
    if v.is_empty() || !v.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(v.len());
    for pair in v.chunks_exact(2) {
        let (x, y) = (pair[0], pair[1]);
        out.push((display_h - y).to_string());
        out.push(x.to_string());
    }
    Some(out.join(","))
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
    fn bbox_turns_clockwise_and_swaps_sides() {
        // On a 100-tall display canvas, a box at (10, 20) sized 30x40 turns so
        // its new top-left x is 100 - 20 - 40 = 40, y is the old x (10), and the
        // width/height swap to 40x30.
        assert_eq!(
            rotate_bbox_cw("10,20,30,40", 100),
            Some("40,10,40,30".into())
        );
        // Four turns return to the start (canvas height alternates W<->H).
        let b0 = "10,20,30,40";
        let b1 = rotate_bbox_cw(b0, 100).unwrap(); // canvas 80x100 -> 100x80
        let b2 = rotate_bbox_cw(&b1, 80).unwrap(); // canvas 100x80 -> 80x100
        let b3 = rotate_bbox_cw(&b2, 100).unwrap();
        let b4 = rotate_bbox_cw(&b3, 80).unwrap();
        assert_eq!(b4, b0);
        // Malformed input is refused rather than corrupted.
        assert_eq!(rotate_bbox_cw("1,2,3", 100), None);
        assert_eq!(rotate_bbox_cw("a,b,c,d", 100), None);
    }

    #[test]
    fn landmark_turns_each_point_clockwise() {
        // (x, y) -> (H - y, x); two points on a 100-tall canvas.
        assert_eq!(
            rotate_landmark_cw("10,20,30,40", 100.0),
            Some("80,10,60,30".into())
        );
        // An odd or empty list is refused.
        assert_eq!(rotate_landmark_cw("1,2,3", 100.0), None);
        assert_eq!(rotate_landmark_cw("", 100.0), None);
    }

    #[test]
    fn rotate_refuses_a_video() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"not a real mp4").unwrap();
        assert!(rotate_cw_in_place(&path, "mp4").is_err());
    }
}
