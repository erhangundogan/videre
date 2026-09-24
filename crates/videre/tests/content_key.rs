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
