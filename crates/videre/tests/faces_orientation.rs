//! Verifies that `videre faces` detects orientation-tagged files on the
//! display canvas: the same fixture with and without EXIF Orientation = 6
//! must produce the same detections, in display-canvas coordinates, and
//! every new row must carry `oriented = 1`.
//!
//! Regression guard for the bug where JPEGs stored rotated (orientation 6,
//! common in exports) were detected and embedded on the raw sensor canvas,
//! which scrambled landmarks and produced embeddings carrying no identity,
//! leaving the owner's faces as unassigned singletons.

mod common;
use common::{face_models_cached, shared_cache_guard, skip_without_models, TestLibrary};

use rusqlite::Connection;
use std::path::Path;

/// A library holding the untagged fixture and its orientation-6 twin. Same
/// pixels by construction (the twin differs only in its EXIF tag), so both
/// files must yield the same detections once decoding is orientation-aware.
fn two_canvas_library() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (name, hash) in [
        ("ai-generated-couple.jpg", "h_plain"),
        ("ai-generated-couple_o6.jpg", "h_o6"),
    ] {
        let path = root.join(name);
        std::fs::copy(base.join(name), &path).unwrap();
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, 'jpg')",
            rusqlite::params![path.to_str().unwrap(), hash],
        )
        .unwrap();
    }
    lib
}

fn face_rows(db: &Path, hash: &str) -> Vec<(String, i64)> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT bbox, COALESCE(oriented, 0) FROM faces WHERE hash = ?1")
        .unwrap();
    let rows = stmt
        .query_map([hash], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

#[test]
fn orientation6_fixture_detects_on_the_display_canvas() {
    if skip_without_models("faces", face_models_cached()) {
        return;
    }
    let _serial = shared_cache_guard();
    let lib = two_canvas_library();
    let db = lib.db();

    let out = lib
        .cmd()
        .args(["faces", "--workers", "1", "--silent"])
        .output()
        .expect("failed to run videre faces");
    assert!(
        out.status.success(),
        "videre faces failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let plain = face_rows(&db, "h_plain");
    let tagged = face_rows(&db, "h_o6");
    assert!(
        !plain.is_empty(),
        "the ai-generated-couple fixture must contain at least one face"
    );
    assert_eq!(
        plain.len(),
        tagged.len(),
        "identical pixels must produce the same detections regardless of the EXIF tag"
    );

    // The o6 file's raw canvas is a 90 CW rotation of the plain file's, so
    // the display canvas is 1543 wide where the raw canvas is 1200. A
    // raw-canvas detection could never produce a bbox reaching past x=1200;
    // the observed boxes do, which is only explicable on the display canvas.
    for (bbox, _) in &tagged {
        let parts: Vec<f32> = bbox.split(',').filter_map(|v| v.parse().ok()).collect();
        assert_eq!(parts.len(), 4, "bbox {bbox:?} must be x,y,w,h");
        let (x, y, w, h) = (parts[0], parts[1], parts[2], parts[3]);
        assert!(
            x + w > 1200.0 || y + h > 1543.0,
            "bbox {bbox:?} fits the raw canvas; expected display-canvas geometry"
        );
        assert!(
            x + w <= 1543.0 && y + h <= 1200.0,
            "bbox {bbox:?} must fit the 1543x1200 display canvas"
        );
    }

    // The same faces, seen through the rotation: map each tagged bbox back
    // into raw-canvas space ([x, x+w) maps to [H-x-w, H-x), w/h swap) and
    // require it to land on a plain bbox. The detector is not bit-exact
    // across rotated inputs, so match by center distance rather than bytes.
    let centers = |b: &str| -> (f32, f32) {
        let p: Vec<f32> = b.split(',').filter_map(|v| v.parse().ok()).collect();
        (p[0] + p[2] / 2.0, p[1] + p[3] / 2.0)
    };
    let plain_centers: Vec<(f32, f32)> = plain.iter().map(|(b, _)| centers(b)).collect();
    for (bbox, _) in &tagged {
        let p: Vec<f32> = bbox.split(',').filter_map(|v| v.parse().ok()).collect();
        let (x, y, w, h) = (p[0], p[1], p[2], p[3]);
        // Display (x', y') comes from raw (x, y) via x' = H-1-y, y' = x, so
        // the inverse maps this box's center to raw (y'+h/2, H-x'-w/2).
        let raw_center = (y + h / 2.0, 1543.0 - x - w / 2.0);
        let best = plain_centers
            .iter()
            .map(|(cx, cy)| ((cx - raw_center.0).powi(2) + (cy - raw_center.1).powi(2)).sqrt())
            .fold(f32::INFINITY, f32::min);
        assert!(
            best < 60.0,
            "tagged bbox {bbox} maps to raw center {raw_center:?}, which is {best}px from the \
             nearest untagged detection; the same face must be found on both canvases"
        );
    }

    for (bbox, oriented) in plain.iter().chain(tagged.iter()) {
        assert_eq!(*oriented, 1, "every new row is display-canvas ({bbox})");
    }
}
