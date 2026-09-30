//! `--query` on the commands that work over rows: the query language as
//! filters, narrowing exactly as the flags do. `search` takes the same
//! language as its positional argument (tests/search.rs).

mod common;
use common::TestLibrary;

/// Three rows: `deniz.jpg` (h1) tagged deniz, `plaj.jpg` (h2) tagged plaj,
/// `ev.jpg` (h3) untagged.
fn library() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    for (name, hash) in [("deniz.jpg", "h1"), ("plaj.jpg", "h2"), ("ev.jpg", "h3")] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, modified_at, ext, mime, exif_date)
             VALUES (?1, ?2, 100, '2023-07-01T10:00:00', 'jpg', 'image/jpeg',
                     '2023-07-01T10:00:00')",
            rusqlite::params![root.join(name).to_string_lossy().as_ref(), hash],
        )
        .unwrap();
    }
    videre_core::tags::ensure_photo_tags_table(&conn).unwrap();
    conn.execute_batch("INSERT INTO photo_tags VALUES ('h1', 'deniz'), ('h2', 'plaj');")
        .unwrap();
    lib
}

fn run(lib: &TestLibrary, args: &[&str]) -> std::process::Output {
    lib.cmd().args(args).output().unwrap()
}

fn ok(lib: &TestLibrary, args: &[&str]) -> String {
    let out = run(lib, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn tagged(lib: &TestLibrary, tag: &str) -> Vec<String> {
    let conn = lib.conn();
    let mut stmt = conn
        .prepare("SELECT hash FROM photo_tags WHERE tag = ?1 ORDER BY hash")
        .unwrap();
    stmt.query_map([tag], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn tag_takes_a_query_with_or_and_not() {
    let lib = library();
    let said = ok(
        &lib,
        &["tag", "--query", "tag:deniz OR tag:plaj", "--add", "yaz"],
    );
    assert!(said.contains("2 of 3"), "{said}");
    assert_eq!(tagged(&lib, "yaz"), ["h1", "h2"]);
    ok(&lib, &["tag", "--query", "NOT tag:deniz", "--add", "kış"]);
    assert_eq!(tagged(&lib, "kış"), ["h2", "h3"]);
}

#[test]
fn a_query_narrows_together_with_the_flags() {
    let lib = library();
    ok(
        &lib,
        &[
            "tag",
            "--query",
            "tag:deniz OR tag:plaj",
            "--tag",
            "plaj",
            "--add",
            "ikisi",
        ],
    );
    assert_eq!(tagged(&lib, "ikisi"), ["h2"]);
}

#[test]
fn mark_takes_a_query() {
    let lib = library();
    ok(&lib, &["mark", "--query", "tag:deniz", "--rating", "5"]);
    let rated: Vec<String> = {
        let conn = lib.conn();
        let mut stmt = conn
            .prepare("SELECT hash FROM marks WHERE rating = 5 ORDER BY hash")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(rated, ["h1"]);
}

#[test]
fn the_jsonl_export_takes_a_query() {
    let lib = library();
    ok(&lib, &["export", "--jsonl", "--query", "-tag:deniz"]);
    let jsonl = std::fs::read_to_string(lib.context().paths.jsonl).unwrap();
    assert_eq!(jsonl.lines().count(), 2, "{jsonl}");
    assert!(!jsonl.contains("deniz.jpg"), "{jsonl}");
}

#[test]
fn embed_narrows_by_a_query_before_any_model_work() {
    let lib = library();
    // Nothing matches, so the run ends at "0 of" without loading a model.
    let said = ok(&lib, &["embed", "--query", "tag:yok"]);
    assert!(said.contains("0 of"), "{said}");
}

#[test]
fn words_to_search_for_are_refused_outside_search() {
    let lib = library();
    for command in ["tag", "mark", "embed", "classify", "faces", "export"] {
        let out = run(&lib, &[command, "--query", "gün batımı"]);
        assert!(!out.status.success(), "{command} accepted text");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("videre search"), "{command}: {err}");
    }
}

/// embed and faces offer no --person or --category, because both come from
/// their own results; a query cannot select by them either.
#[test]
fn embed_and_faces_refuse_what_they_produce() {
    let lib = library();
    for command in ["embed", "faces"] {
        for query in ["person:özgür", "tag:deniz OR category:document"] {
            let out = run(&lib, &[command, "--query", query]);
            assert!(!out.status.success(), "{command} accepted {query}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains("own results"), "{command} {query}: {err}");
        }
    }
    // classify can: people are not its output. (This library has no
    // embeddings, so it stops there, but not on the query.)
    let out = run(&lib, &["classify", "--query", "person:özgür"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("own results"), "{err}");
}

#[test]
fn a_bad_query_is_refused_before_anything_changes() {
    let lib = library();
    let out = run(&lib, &["tag", "--query", "kişi:özgür", "--add", "x"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown key"));
    assert!(tagged(&lib, "x").is_empty());
}
