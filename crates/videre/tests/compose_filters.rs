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
