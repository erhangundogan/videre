//! `videre dedupe` by kind and action: `list` (the default), `review`,
//! `trash`, `delete` and `undo`, each but `undo` scoped by `--kind` and
//! `--query`.

mod common;
use common::TestLibrary;
use std::path::Path;

/// A resized copy of a file already in the library: the picture at 80% of
/// its size, re-encoded.
fn resized_copy(lib: &TestLibrary, source: &str, target: &str) {
    let img = videre_core::image_decode::decode_oriented_file(&lib.root.join(source)).unwrap();
    let (w, h) = (img.width() * 4 / 5, img.height() * 4 / 5);
    let small = img.resize_exact(w, h, image::imageops::FilterType::Lanczos3);
    let out = lib.root.join(target);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    small.to_rgb8().save(&out).unwrap();
}

/// A picture of its own (no fixture shares its content key): diagonal bands
/// and a block, so its fingerprint has structure.
fn painted(lib: &TestLibrary, target: &str) {
    let img = image::RgbImage::from_fn(600, 400, |x, y| {
        let band = ((x + 2 * y) / 40 % 2) as u8 * 160;
        let block = if (200..350).contains(&x) && (100..250).contains(&y) {
            90
        } else {
            0
        };
        image::Rgb([band.saturating_add(block), 60, 200 - band / 2])
    });
    let out = lib.root.join(target);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    img.save(&out).unwrap();
}

/// What `embed` would store: the fingerprint of each file's upright pixels.
/// Written directly so the test stays model-free.
fn fingerprint(lib: &TestLibrary) {
    let conn = lib.conn();
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT path, hash FROM file_hashes")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (path, hash) in rows {
        let img = videre_core::image_decode::decode_oriented_file(Path::new(&path)).unwrap();
        let phash = videre_core::image_decode::dhash(&img) as i64;
        conn.execute(
            "UPDATE file_hashes SET phash = ?1 WHERE hash = ?2",
            rusqlite::params![phash, hash],
        )
        .unwrap();
    }
}

/// Two pictures, each with a resized copy in another folder, plus an exact
/// copy and a Google creation beside its original.
fn library() -> TestLibrary {
    let lib = TestLibrary::new();
    lib.copy_fixture("ai-generated-couple.jpg", "asıl/çiçek.jpg");
    resized_copy(&lib, "asıl/çiçek.jpg", "whatsapp/çiçek-wa.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "kıyı/deniz.jpg");
    resized_copy(&lib, "kıyı/deniz.jpg", "whatsapp/deniz-wa.jpg");
    lib.copy_fixture("tiny.jpg", "tatil/kum.jpg");
    lib.copy_fixture("tiny.jpg", "yedek/kum.jpg");
    painted(&lib, "takeout/IMG_0001.jpg");
    resized_copy(&lib, "takeout/IMG_0001.jpg", "takeout/IMG_0001-EFFECTS.jpg");
    lib.scan();
    fingerprint(&lib);
    lib
}

fn run(lib: &TestLibrary, args: &[&str]) -> std::process::Output {
    lib.cmd().arg("dedupe").args(args).output().unwrap()
}

fn ok(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn exists(lib: &TestLibrary, rel: &str) -> bool {
    lib.root.join(rel).exists()
}

#[test]
fn bare_dedupe_lists_the_exact_copies() {
    let lib = library();
    let out = ok(&run(&lib, &["--silent"]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "{out}");
    assert!(lines[0].ends_with("kum.jpg"), "{out}");
    assert_eq!(ok(&run(&lib, &["list", "--silent"])), out);
}

#[test]
fn trash_resized_moves_the_smaller_copies_and_undo_restores_them() {
    let lib = library();
    let listed = ok(&run(&lib, &["--kind", "resized", "--silent"]));
    assert!(
        listed.contains("çiçek-wa.jpg") && listed.contains("deniz-wa.jpg"),
        "{listed}"
    );
    assert!(
        !listed.contains("EFFECTS"),
        "a creation is not a resized copy: {listed}"
    );

    ok(&run(
        &lib,
        &["trash", "--kind", "resized", "--yes", "--silent"],
    ));
    assert!(!exists(&lib, "whatsapp/çiçek-wa.jpg"));
    assert!(!exists(&lib, "whatsapp/deniz-wa.jpg"));
    assert!(exists(&lib, "asıl/çiçek.jpg") && exists(&lib, "kıyı/deniz.jpg"));
    assert!(
        exists(&lib, "yedek/kum.jpg"),
        "exact copies are another kind"
    );

    ok(&run(&lib, &["undo", "--yes", "--silent"]));
    assert!(exists(&lib, "whatsapp/çiçek-wa.jpg"));
    assert!(exists(&lib, "whatsapp/deniz-wa.jpg"));
}

#[test]
fn trash_creation_keeps_the_original() {
    let lib = library();
    ok(&run(
        &lib,
        &["trash", "--kind", "creation", "--yes", "--silent"],
    ));
    assert!(!exists(&lib, "takeout/IMG_0001-EFFECTS.jpg"));
    assert!(exists(&lib, "takeout/IMG_0001.jpg"));
}

#[test]
fn similar_is_review_only() {
    let lib = library();
    let out = run(&lib, &["trash", "--kind", "similar", "--yes"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("review-only"), "{err}");
    assert!(err.contains("videre dedupe review --kind similar"), "{err}");
}

#[test]
fn undo_takes_no_kind_or_query() {
    let lib = TestLibrary::new();
    for extra in [["--kind", "exact"], ["--query", "tag:deniz"]] {
        let out = lib
            .cmd()
            .args(["dedupe", "undo"])
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{extra:?}");
    }
}

#[test]
fn the_old_flags_are_gone() {
    let lib = TestLibrary::new();
    for flag in [
        "--trash",
        "--delete",
        "--undo",
        "--html",
        "--edited",
        "--similar",
    ] {
        let out = lib.cmd().args(["dedupe", flag]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{flag}");
    }
}

#[test]
fn a_query_selects_whole_groups() {
    let lib = library();
    // Only the keeper carries the tag; its copy still goes, the other
    // picture's copy stays.
    let tag = lib
        .cmd()
        .args(["tag", "--path"])
        .arg(lib.root.join("asıl"))
        .args(["--add", "plaj", "--silent"])
        .output()
        .unwrap();
    ok(&tag);
    ok(&run(
        &lib,
        &[
            "trash", "--kind", "resized", "--query", "tag:plaj", "--yes", "--silent",
        ],
    ));
    assert!(!exists(&lib, "whatsapp/çiçek-wa.jpg"));
    assert!(exists(&lib, "whatsapp/deniz-wa.jpg"));
}

#[test]
fn list_json_is_one_document_by_kind() {
    let lib = library();
    let out = ok(&run(&lib, &["--kind", "exact,resized,creation", "--json"]));
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["schema_version"], 2);
    assert_eq!(
        doc["kinds"],
        serde_json::json!(["exact", "resized", "creation"])
    );
    let groups = doc["groups"].as_array().unwrap();
    let kinds: Vec<&str> = groups.iter().map(|g| g["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["exact", "resized", "resized", "creation"]);
    for g in groups {
        assert!(
            g["keep"]["path"].is_string() && g["remove"].is_array(),
            "{g}"
        );
    }

    let similar = ok(&run(&lib, &["--kind", "similar", "--json"]));
    let doc: serde_json::Value = serde_json::from_str(&similar).unwrap();
    for g in doc["groups"].as_array().unwrap() {
        assert!(g.get("keep").is_none() && g["files"].is_array(), "{g}");
    }
}

#[test]
fn trash_json_reports_what_moved() {
    let lib = library();
    let out = ok(&run(&lib, &["trash", "--kind", "exact", "--yes", "--json"]));
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["removed"].as_array().unwrap().len(), 1, "{doc}");
    assert_eq!(doc["dry_run"], false);
}

#[test]
fn review_writes_the_chosen_kinds() {
    let lib = library();
    let page = lib.root.join("inceleme.html");
    let out = lib
        .cmd()
        .args(["dedupe", "review"])
        .arg(&page)
        .args(["--kind", "exact,creation", "--silent"])
        .output()
        .unwrap();
    ok(&out);
    let html = std::fs::read_to_string(&page).unwrap();
    assert!(html.contains("kum.jpg"), "exact group");
    assert!(html.contains("IMG_0001-EFFECTS.jpg"), "creation group");
    assert!(!html.contains("çiçek-wa.jpg"), "resized was not chosen");
}
