mod common;

use common::TestLibrary;

/// Fixture: two duplicates (hash hdup), one singular (hsing), one video (hvid),
/// as real files under the library root so any existence filter passes, seeded
/// directly with in-root paths. Returns the library plus its four file paths.
fn fixture(with_embeddings: bool) -> (TestLibrary, [std::path::PathBuf; 4]) {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let pics = root.join("pics");
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

    let conn = lib.init_db();
    for (path, hash, ext) in [
        (&files[0], "hdup", "jpg"),
        (&files[1], "hdup", "jpg"),
        (&files[2], "hsing", "jpg"),
        (&files[3], "hvid", "mov"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext) VALUES (?1, ?2, 100, ?3)",
            rusqlite::params![path.to_string_lossy().as_ref(), hash, ext],
        )
        .unwrap();
    }
    if with_embeddings {
        let ctx = lib.context();
        videre_core::embeddings_db::attach_in(
            &conn,
            &ctx,
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
    (lib, files)
}

/// `videre dedupe review`, which is what plain `videre report` used to be.
fn run_dedupe_html(lib: &TestLibrary) -> String {
    let out = lib.context().paths.root.join("dupes.html");
    let status = lib
        .cmd()
        .args(["dedupe", "review"])
        .arg(&out)
        .status()
        .expect("failed to run videre dedupe review");
    assert!(status.success());
    std::fs::read_to_string(&out).unwrap()
}

/// `dedupe review` shows Google Takeout creations only with `--kind creation`.
#[test]
fn dedupe_review_shows_creations_only_with_their_kind() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Photos/IMG_1.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "Photos/IMG_1-edited.jpg");
    lib.scan();
    let html = run_dedupe_html(&lib);
    assert!(
        !html.contains("\"edited\":true"),
        "no creations without --kind creation"
    );

    let out = lib.context().paths.root.join("edited.html");
    let status = lib
        .cmd()
        .args(["dedupe", "review", "--kind", "exact,creation"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    let html = std::fs::read_to_string(&out).unwrap();
    assert!(html.contains("\"edited\":true"), "the pair is shown");
    assert!(html.contains("IMG_1-edited.jpg"));
    // The header counts the pairs it shows, not only exact copies: a Takeout
    // library whose groups are all edits read "Duplicate groups: 0".
    let edit_size = std::fs::metadata(lib.context().paths.root.join("Photos/IMG_1-edited.jpg"))
        .unwrap()
        .len();
    let expected = format!(
        "Google Photos edits:</span> 1 ({})",
        videre_core::disk::human_bytes(edit_size)
    );
    assert!(html.contains(&expected), "{expected} in the header");
}

/// `videre search --html`, the other static page. Renders a flat list rather
/// than duplicate groups.
fn run_search_html(lib: &TestLibrary, query: &str) -> String {
    let out = lib.context().paths.root.join("search.html");
    let status = lib
        .cmd()
        .args(["search", "--ext", query, "--html"])
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
    let (lib, _) = fixture(true);
    let html = run_dedupe_html(&lib);
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
    assert!(
        !html.contains("href=\"/date/2025"),
        "static export must not link to live-only date routes"
    );
}

#[test]
fn dedupe_review_contains_the_duplicate_group() {
    let (lib, files) = fixture(false);
    let html = run_dedupe_html(&lib);
    // a.jpg and b.jpg share hash hdup, so both belong on the page.
    assert!(html.contains(files[0].to_str().unwrap()), "a.jpg missing");
    assert!(html.contains(files[1].to_str().unwrap()), "b.jpg missing");
}

// :warning: The two renderers disagree about files deleted after the scan, and
// this pins the disagreement rather than hiding it. `query_all_files`, which fed
// the old `report --all` gallery, filtered on `Path::exists()`. `query_groups`,
// which fed `dedupe review`, did not, so a row whose file is gone still
// appears until `videre prune` removes it.
#[test]
fn dedupe_review_still_lists_a_file_deleted_after_the_scan() {
    let (lib, files) = fixture(false);
    std::fs::remove_file(&files[1]).unwrap();
    let html = run_dedupe_html(&lib);
    assert!(
        html.contains(files[1].to_str().unwrap()),
        "dedupe review reads the database, so a not-yet-pruned row still shows"
    );
}

#[test]
fn search_html_writes_a_page() {
    let (lib, _) = fixture(false);
    // A filter query needs no embeddings, so this works on the plain fixture.
    let html = run_search_html(&lib, "jpg");
    assert!(
        html.contains("<html") || html.contains("<!doctype"),
        "not an HTML document"
    );
}

#[test]
fn both_static_pages_are_in_help() {
    let lib = TestLibrary::new();
    for (cmd, needle) in [("dedupe", "review"), ("search", "--html")] {
        let out = lib
            .cmd()
            .args([cmd, "--help"])
            .output()
            .expect("failed to run --help");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(needle),
            "{cmd} --help does not mention {needle}"
        );
    }
}
