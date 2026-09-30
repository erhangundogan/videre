//! QuickLook, which renders every HEIC videre shows or analyses, agrees with
//! `heif_rotate` about which way a turn goes. The unit tests pin the bytes;
//! this pins what a person then sees.
#![cfg(target_os = "macos")]

use image::{DynamicImage, GenericImageView};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/heic/grid_rot90.heic"
);

fn render(bytes: &[u8], dir: &std::path::Path, name: &str) -> DynamicImage {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    videre_core::heic::decode_via_quicklook(&path, "test", Some(1024)).unwrap()
}

/// Which corner the red marker is in, as (right, bottom).
fn marker_corner(img: &DynamicImage) -> (bool, bool) {
    let (w, h) = img.dimensions();
    let red = |x: u32, y: u32| {
        let p = img.get_pixel(x, y).0;
        p[0] > 200 && p[1] < 80 && p[2] < 80
    };
    let (x_in, y_in) = (w / 40, h / 40);
    let corners = [
        (false, false, x_in, y_in),
        (true, false, w - 1 - x_in, y_in),
        (false, true, x_in, h - 1 - y_in),
        (true, true, w - 1 - x_in, h - 1 - y_in),
    ];
    let hits: Vec<_> = corners
        .iter()
        .filter(|c| red(c.2, c.3))
        .map(|c| (c.0, c.1))
        .collect();
    assert_eq!(hits.len(), 1, "exactly one red corner, got {hits:?}");
    hits[0]
}

#[test]
fn a_clockwise_turn_renders_a_quarter_turn_clockwise() {
    let dir = tempfile::tempdir().unwrap();
    let original = std::fs::read(FIXTURE).unwrap();
    let before = render(&original, dir.path(), "before.heic");
    // Stored landscape with the marker top-left; irot 270 shows it portrait,
    // turned clockwise, so the marker is top-right.
    assert!(before.height() > before.width());
    assert_eq!(marker_corner(&before), (true, false));

    let turned = videre::heif_rotate::rotated(&original, false).unwrap();
    let after = render(&turned, dir.path(), "after.heic");
    // One more clockwise quarter: landscape again, marker bottom-right.
    assert!(after.width() > after.height());
    assert_eq!(marker_corner(&after), (true, true));

    let back = videre::heif_rotate::rotated(&original, true).unwrap();
    let ccw = render(&back, dir.path(), "ccw.heic");
    // Counter-clockwise from the original: the stored orientation, marker
    // back top-left.
    assert!(ccw.width() > ccw.height());
    assert_eq!(marker_corner(&ccw), (false, false));
}
