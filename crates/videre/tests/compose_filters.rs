//! The mark/tag composition filters, now accepted beyond `search`, actually
//! narrow the commands that gained them. Behavioural end-to-end checks against
//! the real binary; no models (a plain hand-seeded library with a mark set
//! directly, so `export` never loads anything).

mod common;
use common::TestLibrary;

/// Seed two distinct files, label one, and confirm `export --jsonl --label`
/// snapshots only the labelled file. This exercises the whole path: the flag
/// parses on `export`, `row_selection` carries the label, and `resolve_in`
/// intersects it.
#[test]
fn export_jsonl_scoped_by_label_writes_only_the_labelled_file() {
    let lib = TestLibrary::new();
    // Distinct files -> distinct hashes, so a mark on one does not select both.
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "b.jpg");
    lib.scan();

    let conn = lib.conn();
    let a_hash: String = conn
        .query_row(
            "SELECT hash FROM file_hashes WHERE path LIKE '%a.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    videre_core::marks::ensure_marks_table(&conn).unwrap();
    videre_core::marks::set(
        &conn,
        std::slice::from_ref(&a_hash),
        &videre_core::marks::change_from_parts(None, None, Some("Green"), None),
    )
    .unwrap();
    drop(conn);

    let out = lib
        .cmd()
        .args(["export", "--jsonl", "--label", "Green"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let jsonl = std::fs::read_to_string(lib.root.join(".videre/hashes.jsonl")).unwrap();
    assert!(
        jsonl.contains("a.jpg"),
        "the labelled file must be in the snapshot"
    );
    assert!(
        !jsonl.contains("b.jpg"),
        "an unlabelled file must be excluded by --label"
    );
}

/// `embed --like` on a library with no likes narrows to nothing rather than
/// erroring: a filter that matches nothing is not an error. Uses the `.dng`
/// non-embeddable guard so no model loads even though embed is invoked.
#[test]
fn embed_like_on_a_library_with_no_likes_processes_nothing_without_a_model() {
    let lib = TestLibrary::new();
    // .dng is scanned but vetoed as non-embeddable, so embed returns before
    // loading a model regardless of the filter.
    std::fs::write(lib.root.join("only.dng"), b"not really a dng").unwrap();
    lib.scan();

    let out = lib
        .cmd()
        .env("HF_HOME", lib.home.join("hf-empty"))
        .args(["embed", "--like", "--silent"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "embed --like must succeed (0 of N), not error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Nothing was embedded, and crucially no weights were fetched.
    assert!(
        !lib.home.join("hf-empty").exists()
            || std::fs::read_dir(lib.home.join("hf-empty"))
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "embed --like with no matches must not download a model"
    );
}

/// `mark --tag t --rating 5` rates only the tagged file: the `--tag` filter is a
/// row-side narrower, while `--rating` remains the setter. Proves mark's new
/// `--tag` filter and its setters coexist and land on the right files.
#[test]
fn mark_rating_scoped_by_tag_rates_only_the_tagged_file() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "b.jpg");
    lib.scan();

    let conn = lib.conn();
    let a_hash: String = conn
        .query_row(
            "SELECT hash FROM file_hashes WHERE path LIKE '%a.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let b_hash: String = conn
        .query_row(
            "SELECT hash FROM file_hashes WHERE path LIKE '%b.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    videre_core::tags::ensure_photo_tags_table(&conn).unwrap();
    videre_core::tags::set_tags(
        &conn,
        std::slice::from_ref(&a_hash),
        &["keeper".to_string()],
    )
    .unwrap();
    drop(conn);

    let out = lib
        .cmd()
        .args(["mark", "--tag", "keeper", "--rating", "5", "--silent"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conn = lib.conn();
    assert_eq!(
        videre_core::marks::get(&conn, &a_hash).unwrap().rating,
        Some(5),
        "the tagged file must be rated"
    );
    assert_eq!(
        videre_core::marks::get(&conn, &b_hash).unwrap().rating,
        None,
        "an untagged file must be untouched by --tag-scoped mark"
    );
}

/// `tag --add x --label Green` tags only the green file: `--add` is the setter,
/// `--label` is a mark-side filter narrowing which files are tagged. Proves
/// tag's setters and the mark filter group coexist.
#[test]
fn tag_add_scoped_by_label_tags_only_the_labelled_file() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "b.jpg");
    lib.scan();

    let conn = lib.conn();
    let a_hash: String = conn
        .query_row(
            "SELECT hash FROM file_hashes WHERE path LIKE '%a.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let b_hash: String = conn
        .query_row(
            "SELECT hash FROM file_hashes WHERE path LIKE '%b.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    videre_core::marks::ensure_marks_table(&conn).unwrap();
    videre_core::marks::set(
        &conn,
        std::slice::from_ref(&a_hash),
        &videre_core::marks::change_from_parts(None, None, Some("Green"), None),
    )
    .unwrap();
    drop(conn);

    let out = lib
        .cmd()
        .args(["tag", "--add", "keeper", "--label", "Green", "--silent"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conn = lib.conn();
    assert_eq!(
        videre_core::tags::tags_for_hash(&conn, &a_hash).unwrap(),
        vec!["keeper".to_string()],
        "the labelled file must be tagged"
    );
    assert!(
        videre_core::tags::tags_for_hash(&conn, &b_hash)
            .unwrap()
            .is_empty(),
        "an unlabelled file must not be tagged by a --label-scoped tag"
    );
}
