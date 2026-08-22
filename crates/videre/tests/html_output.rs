mod common;
use common::isolated_home;

use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn report_bin() -> std::path::PathBuf {
    isolated_home();
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

/// Fixture: two duplicates (hash hdup), one singular (hsing), one video (hvid).
/// Creates real files on disk so the existence filter in query_all_files passes.
/// Returns (db_path, [path_a, path_b, path_c, path_d]).
fn fixture_db(
    dir: &std::path::Path,
    with_embeddings: bool,
) -> (std::path::PathBuf, [std::path::PathBuf; 4]) {
    let pics = dir.join("pics");
    std::fs::create_dir(&pics).unwrap();
    let files = [
        pics.join("a.jpg"),
        pics.join("b.jpg"),
        pics.join("c.jpg"),
        pics.join("d.mov"),
    ];
    for f in &files {
        std::fs::write(f, b"dummy").unwrap();
    }

    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (
            path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
            created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
            exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER
        );",
    )
    .unwrap();
    videre_core::db::ensure_file_hashes_columns(&conn);
    for (path, hash, ext) in [
        (files[0].to_str().unwrap(), "hdup", "jpg"),
        (files[1].to_str().unwrap(), "hdup", "jpg"),
        (files[2].to_str().unwrap(), "hsing", "jpg"),
        (files[3].to_str().unwrap(), "hvid", "mov"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext) VALUES (?1, ?2, 100, ?3)",
            rusqlite::params![path, hash, ext],
        )
        .unwrap();
    }
    if with_embeddings {
        // Into the per-model database, which is where report reads from now.
        isolated_home();
        videre_core::embeddings_db::attach(
            &conn,
            &db,
            videre_core::embeddings::DEFAULT_MODEL_ID,
            true,
        )
        .unwrap();
        let v1 = videre_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let v2 = videre_core::vectors::to_f16_bytes(&[0.0, 1.0]);
        for (hash, v) in [("hdup", v1), ("hsing", v2)] {
            conn.execute(
                "INSERT INTO emb.embeddings VALUES (?1, ?2, ?3, 'now')",
                rusqlite::params![hash, videre_core::embeddings::DEFAULT_MODEL_ID, v],
            )
            .unwrap();
        }
        videre_core::embeddings_db::detach(&conn).unwrap();
    }
    (db, files)
}

/// `videre dedupe --html`, which is what plain `videre report` used to be.
fn run_dedupe_html(db: &std::path::Path) -> String {
    let out = db.with_extension("dupes.html");
    let status = Command::new(report_bin())
        .arg("dedupe")
        .arg("--db")
        .arg(db)
        .arg("--html")
        .arg(&out)
        .status()
        .expect("failed to run videre dedupe --html");
    assert!(status.success());
    std::fs::read_to_string(&out).unwrap()
}

/// `videre search --html`, the other static page. Renders a flat list rather
/// than duplicate groups.
fn run_search_html(db: &std::path::Path, query: &str) -> String {
    let out = db.with_extension("search.html");
    let status = Command::new(report_bin())
        .arg("search")
        .arg("--ext")
        .arg(query)
        .arg("--db")
        .arg(db)
        .arg("--html")
        .arg(&out)
        .status()
        .expect("failed to run videre search --html");
    assert!(status.success());
    std::fs::read_to_string(&out).unwrap()
}

// :warning: These pages carry no similarity search, and that is deliberate.
// `write_static_page` passes no vectors, because an exported file cannot ask
// the database for anything later. In-page similarity lives in `videre
// gallery`, which is a live server. The assertions below pin that difference so
// nobody "restores" vectors to a static export without deciding to.

#[test]
fn a_static_page_carries_no_vectors_or_gallery_shell() {
    let dir = tempdir().unwrap();
    let (db, _) = fixture_db(dir.path(), true);
    let html = run_dedupe_html(&db);
    assert!(
        !html.contains("var VEC_B64="),
        "static export must not embed vectors"
    );
    assert!(!html.contains("var ALLFILES="));
    assert!(!html.contains("id=\"gallery\""));
    assert!(!html.contains("id=\"results\""));
    // The section strip is server-only for the same reason: `/date` and
    // `/people` do not exist once the file is opened from `file://`, so linking
    // to them would offer three routes and deliver one dead end each.
    assert!(
        !html.contains("class=\"secnav\""),
        "static export must not carry section links to routes that need a server"
    );
}

#[test]
fn dedupe_html_contains_the_duplicate_group() {
    let dir = tempdir().unwrap();
    let (db, files) = fixture_db(dir.path(), false);
    let html = run_dedupe_html(&db);
    // a.jpg and b.jpg share hash hdup, so both belong on the page.
    assert!(html.contains(files[0].to_str().unwrap()), "a.jpg missing");
    assert!(html.contains(files[1].to_str().unwrap()), "b.jpg missing");
}

// :warning: The two renderers disagree about files deleted after the scan, and
// this pins the disagreement rather than hiding it. `query_all_files`, which fed
// the old `report --all` gallery, filtered on `Path::exists()`. `query_groups`,
// which feeds `dedupe --html`, does not, so a row whose file is gone still
// appears until `videre prune` removes it.
//
// Defensible either way: the database is the source of truth until pruned, and
// a duplicate-review page arguably should show a stale row. But nobody chose
// it, so it is recorded as current behaviour, not as intent.
#[test]
fn dedupe_html_still_lists_a_file_deleted_after_the_scan() {
    let dir = tempdir().unwrap();
    let (db, files) = fixture_db(dir.path(), false);
    std::fs::remove_file(&files[1]).unwrap();
    let html = run_dedupe_html(&db);
    assert!(
        html.contains(files[1].to_str().unwrap()),
        "dedupe --html reads the database, so a not-yet-pruned row still shows"
    );
}

#[test]
fn search_html_writes_a_page() {
    let dir = tempdir().unwrap();
    let (db, _) = fixture_db(dir.path(), false);
    // A filter query needs no embeddings, so this works on the plain fixture.
    let html = run_search_html(&db, "jpg");
    assert!(
        html.contains("<html") || html.contains("<!doctype"),
        "not an HTML document"
    );
}

#[test]
fn html_flag_is_documented_on_both_commands() {
    for cmd in ["dedupe", "search"] {
        let out = Command::new(report_bin())
            .arg(cmd)
            .arg("--help")
            .output()
            .expect("failed to run --help");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("--html"),
            "{cmd} --help does not mention --html"
        );
    }
}
