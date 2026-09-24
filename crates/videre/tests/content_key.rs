//! Real files from real writers: a metadata-only edit keeps the content key.

use std::path::Path;
use videre::content_key::{keys, Format, Keys};

fn keys_of(name: &str, format: Format) -> Keys {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let mut file = std::fs::File::open(&path).unwrap();
    let len = file.metadata().unwrap().len();
    keys(&mut file, len, Some(format)).unwrap()
}

#[test]
fn metadata_edits_by_real_tools_keep_the_content_key() {
    for (original, edited, format) in [
        ("tiny.jpg", "content_key/tiny_dated.jpg", Format::Jpeg),
        (
            "content_key/tiny.png",
            "content_key/tiny_dated.png",
            Format::Png,
        ),
        (
            "content_key/tiny.webp",
            "content_key/tiny_dated.webp",
            Format::Webp,
        ),
        (
            "content_key/tiny.gif",
            "content_key/tiny_dated.gif",
            Format::Gif,
        ),
        (
            "content_key/tiny.tiff",
            "content_key/tiny_dated.tiff",
            Format::Tiff,
        ),
        (
            "content_key/tiny.heic",
            "content_key/tiny_dated.heic",
            Format::Heic,
        ),
        (
            "testsrc_1s.mp4",
            "content_key/testsrc_dated.mp4",
            Format::Movie,
        ),
        (
            "audio_only.mov",
            "content_key/audio_only_dated.mov",
            Format::Movie,
        ),
    ] {
        let (a, b) = (keys_of(original, format), keys_of(edited, format));
        assert!(a.meta.is_some(), "{original} was parsed, not a fallback");
        assert!(b.meta.is_some(), "{edited} was parsed, not a fallback");
        assert_eq!(
            a.content, b.content,
            "{original}: same content after a metadata edit"
        );
        assert_ne!(a.meta, b.meta, "{original}: the metadata edit is recorded");
    }
}

#[test]
fn different_images_have_different_keys() {
    assert_ne!(
        keys_of("tiny.jpg", Format::Jpeg).content,
        keys_of("sample_with_exif.jpg", Format::Jpeg).content
    );
    assert_ne!(
        keys_of("testsrc_1s.mp4", Format::Movie).content,
        keys_of("red_1s.mp4", Format::Movie).content
    );
}

mod common;
use common::TestLibrary;

/// tiny.jpg with a COM segment inserted after SOI: the same image with
/// different metadata.
fn commented(comment: &[u8]) -> Vec<u8> {
    let original =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg"))
            .unwrap();
    let mut v = original[..2].to_vec();
    v.extend([0xFF, 0xFE]);
    v.extend(((comment.len() + 2) as u16).to_be_bytes());
    v.extend(comment);
    v.extend(&original[2..]);
    v
}

fn row(lib: &TestLibrary, name: &str) -> (String, Option<String>) {
    let path = lib.context().paths.root.join(name);
    lib.conn()
        .query_row(
            "SELECT hash, meta_hash FROM file_hashes WHERE path = ?1",
            [path.to_string_lossy().as_ref()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

#[test]
fn a_rescan_after_a_metadata_edit_keeps_the_key() {
    let lib = TestLibrary::new();
    std::fs::write(lib.root.join("çağla.jpg"), commented(b"Arsiv 2011")).unwrap();
    lib.scan();
    let (key, meta) = row(&lib, "çağla.jpg");
    assert!(meta.is_some());

    std::fs::write(lib.root.join("çağla.jpg"), commented(b"Arsiv 2024, edited")).unwrap();
    lib.scan();
    let (key_after, meta_after) = row(&lib, "çağla.jpg");
    assert_eq!(key_after, key, "the photo keeps its identity");
    assert_ne!(meta_after, meta, "the metadata change is recorded");
}

#[test]
fn dedupe_groups_the_same_image_with_different_metadata() {
    let lib = TestLibrary::new();
    std::fs::write(lib.root.join("özgür.jpg"), commented(b"one")).unwrap();
    std::fs::write(lib.root.join("şükrü.jpg"), commented(b"two, longer")).unwrap();
    lib.scan();
    let out = lib.cmd().args(["dedupe", "--json"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let groups = json["duplicate_groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{json}");
    let names: Vec<&str> = std::iter::once(&groups[0]["keep"])
        .chain(groups[0]["remove"].as_array().unwrap())
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(names.iter().any(|p| p.ends_with("özgür.jpg")), "{json}");
    assert!(names.iter().any(|p| p.ends_with("şükrü.jpg")), "{json}");
}

#[test]
fn a_library_built_with_the_old_file_identity_is_refused_untouched() {
    let lib = TestLibrary::new();
    let conn = lib.init_db();
    conn.pragma_update(None, "user_version", 2).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(conn);
    let before = std::fs::read(lib.db()).unwrap();
    let out = lib.cmd().arg("stats").output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("created by an older videre"), "{stderr}");
    assert!(stderr.contains("videre scan"), "{stderr}");
    assert_eq!(
        std::fs::read(lib.db()).unwrap(),
        before,
        "nothing was changed"
    );
}
