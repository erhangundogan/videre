//! A gallery page must not carry the whole library.
//!
//! :warning: **This is the test that would have caught three shipped faults, all
//! of which passed every test that existed.**
//!
//! 0.18.0 shipped `videre gallery` inlining its entire dataset. On a real
//! 70,601-file library that produced a **149 MB page** which no browser could
//! open: 145 MB of base64 embeddings, plus a face crop decoded from the original
//! for every labelled face, plus every file as JSON. The server built it in
//! 0.74s, so nothing anywhere reported a problem. It simply never appeared.
//!
//! Two of the three were fixed in 0.20.2. The third is the file list.
//!
//! **Why nothing caught them:** every other fixture in this suite holds between
//! one and four files, and `gallery_routes.rs` asserts that a page *renders*. A
//! page that takes an hour still renders. Reproducing the worst of them needed
//! faces, at scale, on slow storage; a synthetic library with the right file
//! count and no faces confirmed the wrong fix entirely.
//!
//! So this asserts a **byte ceiling**, which is the one assertion that catches
//! the whole class rather than the three instances.
//!
//! :warning: **The fixture uses real decodable JPEGs on purpose.** If a
//! regression reintroduced inline face crops, `face_thumb_b64` would silently
//! return `None` for a stub file and the page would stay small, passing this
//! test while being broken. The crops have to be producible for their absence to
//! mean anything.

mod common;
use common::TestLibrary;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

/// Enough rows that inlining them is unmistakably over the ceiling, while the
/// fixture still builds in a second or two.
const FILES: usize = 3_000;

/// Roughly one file in three carries a labelled face, which is the shape of a
/// real library and enough for a reintroduced crop bug to be obvious.
const FACES: usize = 1_000;

/// How many files share a hash with another, forming duplicate groups.
///
/// :warning: **Every fixture in this file used to have unique hashes**, so it
/// had zero duplicate groups and could not see that `var GROUPS=[...]` is
/// inlined rather than fetched. `/` carried every duplicate group in the
/// library, and this test measured that page and called it clean. A library
/// that has been deduped has no groups, which is exactly why the development
/// library did not show it either.
const DUPES: usize = 2_000;

/// 768 f16 dimensions, matching the default model.
const DIM: usize = 768;

/// The ceiling.
///
/// The intended page is a shell plus one screen of rows: about 20 KB of markup
/// and CSS, plus `GPAGE` (200) rows at roughly 400 bytes each, so ~100 KB.
/// 512 KB leaves room for the shell to grow without becoming a tripwire, while
/// staying far below anything that inlines a library.
///
/// For scale, the same page inlined for 3,000 files with embeddings is several
/// megabytes, and for the 70,601-file library it was measured at 28 MB.
const CEILING: usize = 512 * 1024;

fn seed() -> Option<TestLibrary> {
    let lib = TestLibrary::new();
    let pics = lib.context().paths.root.join("pics");
    std::fs::create_dir_all(&pics).unwrap();

    // A real, decodable JPEG. See the warning at the top of this file.
    //
    // :warning: Read at runtime, never `include_bytes!`. `Cargo.toml` excludes
    // `tests/fixtures/*` from the published package while the tests themselves
    // ship, so a compile-time include would stop the packaged crate building at
    // all for anyone running `cargo test` on the crates.io tarball. Every other
    // fixture user in this suite reads at runtime for the same reason.
    let jpeg = match std::fs::read(fixture()) {
        Ok(b) => b,
        Err(_) => return None,
    };

    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO people (name, full_name) VALUES ('ozgur_demirtas', 'Özgür')",
        [],
    )
    .unwrap();

    let tx = conn.unchecked_transaction().unwrap();
    for i in 0..FILES {
        // Spread across directories, as a real library is.
        let sub = pics.join(format!("d{}", i / 500));
        if i % 500 == 0 {
            std::fs::create_dir_all(&sub).unwrap();
        }
        let path = sub.join(format!("f{i}.jpg"));
        std::fs::write(&path, &jpeg).unwrap();

        // Dates spread over years, so `/date` builds a real tree rather than
        // one bucket.
        let year = 2019 + (i % 6);
        let month = 1 + (i % 12);
        let day = 1 + (i % 28);
        // The first DUPES files pair up, so the library has DUPES/2 duplicate
        // groups of two. See the constant.
        let hash = if i < DUPES { i / 2 } else { i };
        tx.execute(
            "INSERT INTO file_hashes
               (path, hash, size_bytes, modified_at, ext, exif_date, gps_lat, gps_lon, width, height)
             VALUES (?1, ?2, ?3, ?4, 'jpg', ?4, ?5, ?6, 16, 16)",
            rusqlite::params![
                path.to_string_lossy(),
                format!("{hash:064x}"),
                jpeg.len() as i64,
                format!("{year}-{month:02}-{day:02}T12:00:00"),
                41.0 + (i as f64) * 1e-5,
                29.0 + (i as f64) * 1e-5,
            ],
        )
        .unwrap();

        if i < FACES {
            tx.execute(
                "INSERT INTO faces (hash, bbox, embedding, cluster_id, person_label, confirmed)
                 VALUES (?1, '2,2,12,12', X'0000', 1, 'ozgur_demirtas', 1)",
                rusqlite::params![format!("{i:064x}")],
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
    drop(conn);

    seed_embeddings(&lib);
    Some(lib)
}

/// Written to fd 2 directly, not via `eprintln!`. libtest captures the print
/// macros for tests that pass, and a skip passes, so an `eprintln!` here would
/// only appear under `--nocapture`. Same reasoning as `skip_without_models`.
fn skip_no_fixture() {
    use std::io::Write;
    let _ = std::io::stderr().write_all(
        b"SKIP: tests/fixtures/tiny.jpg is absent, which is expected when running \
          against the published package, where fixtures are excluded.\n",
    );
}

/// The fixture image, absent from the published package by design.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny.jpg")
}

/// Embeddings live in a per-library, per-model database beside the main one.
/// Seeding them is what makes this fixture able to catch the 145 MB fault.
fn seed_embeddings(lib: &TestLibrary) {
    let model = videre_core::embeddings::DEFAULT_MODEL_ID;
    let path = videre_core::embeddings_db::db_path_in(&lib.context(), model).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embeddings
         (hash TEXT PRIMARY KEY, model_id TEXT NOT NULL, embedding BLOB NOT NULL);",
    )
    .unwrap();
    // f16 zeroes: the values are irrelevant, only the volume matters here.
    let blob = vec![0u8; DIM * 2];
    let tx = conn.unchecked_transaction().unwrap();
    for i in 0..FILES {
        tx.execute(
            "INSERT OR REPLACE INTO embeddings VALUES (?1, ?2, ?3)",
            rusqlite::params![format!("{i:064x}"), model, blob],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl Server {
    fn start(lib: &TestLibrary) -> Server {
        Server::spawn(lib.cmd())
    }

    /// Start a gallery pinned to `lib` but invoked from a different working
    /// directory (`cwd`), via `--library`. Proves the server binds to the
    /// selected library rather than to wherever it was launched.
    fn start_in(lib: &TestLibrary, cwd: &Path) -> Server {
        Server::spawn(lib.from(cwd))
    }

    fn spawn(mut cmd: std::process::Command) -> Server {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = cmd
            .args(["gallery", "--port", &port.to_string()])
            .spawn()
            .expect("failed to spawn videre gallery");
        let server = Server { child, port };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("gallery did not start on port {port}");
    }

    fn get_len(&self, path: &str) -> usize {
        self.get_body(path).len()
    }

    fn get_body(&self, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).unwrap();
        let text = String::from_utf8_lossy(&raw);
        text.split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default()
    }
}

fn assert_under_ceiling(route: &str) {
    let Some(lib) = seed() else {
        skip_no_fixture();
        return;
    };
    let server = Server::start(&lib);
    let len = server.get_len(route);
    assert!(
        len <= CEILING,
        "{route} served {len} bytes for {FILES} files, over the {CEILING} byte ceiling.\n\
         The page is carrying the library instead of fetching it. At this rate a \
         70,601-file library would serve roughly {} MB.",
        len * 70_601 / FILES / 1_000_000
    );
}

#[test]
fn the_all_files_page_does_not_carry_the_library() {
    assert_under_ceiling("/");
}

#[test]
fn the_date_page_does_not_carry_the_library() {
    assert_under_ceiling("/date");
}

/// :warning: **`/duplicates` still inlines its groups**, which is why this is
/// ignored rather than absent. `/` no longer carries them, so the default route
/// is clean; moving groups to a paged endpoint is the remaining half, and this
/// is the assertion that will prove it. Un-ignore it then.
/// :warning: **`/duplicates` still inlines its groups.** Measured at **989 bytes per
/// group**, so 1,000 groups is ~1 MB and 50,000 is ~49 MB. `/` no longer carries
/// them, which is the half that matters most since it is where everyone lands;
/// moving them to a paged endpoint is the other half, and this is the assertion
/// that will prove it. Un-ignore then.
///
/// The fixture seeds enough duplicates to cross the ceiling on purpose. At 300
/// groups it came to 297 KB and passed, which would have recorded the fault as
/// fixed while it was only small.
#[test]
#[ignore = "/duplicates inlines its groups; the paged endpoint is not built yet"]
fn the_duplicates_page_does_not_carry_the_library() {
    assert_under_ceiling("/duplicates");
}

/// The default route must not grow with the number of duplicates.
///
/// Separate from the ceiling test above because it is about a specific array:
/// with `DUPES` groups seeded, an inlined `GROUPS` is unmistakable, while a
/// byte ceiling could in principle be met by a page that still carries a few.
#[test]
fn the_default_page_carries_no_duplicate_groups() {
    let Some(lib) = seed() else {
        skip_no_fixture();
        return;
    };
    let server = Server::start(&lib);
    let body = server.get_body("/");
    let groups = body
        .split_once("var GROUPS=[")
        .map(|(_, rest)| rest.split("];").next().unwrap_or("").trim())
        .unwrap_or("");
    assert!(
        groups.is_empty(),
        "/ inlined {} bytes of duplicate groups; they belong on /duplicates",
        groups.len()
    );
}

/// The one that already passes, and the reason the other two are worth writing:
/// `/people` has fetched its data since it was written, and is the model the
/// other routes are moving toward.
#[test]
fn the_people_page_already_fetches_its_data() {
    let Some(lib) = seed() else {
        skip_no_fixture();
        return;
    };
    let server = Server::start(&lib);
    let len = server.get_len("/people");
    assert!(
        len <= CEILING,
        "/people served {len} bytes, over the {CEILING} byte ceiling. It fetches \
         from /api/faces, so this should not be possible without a regression."
    );
}

/// Two gallery servers, each pinned to its own library but both invoked from a
/// third, empty directory, must never serve each other's rows, and neither may
/// bring the invocation directory into existence as a library.
#[test]
fn two_servers_do_not_cross_library_rows() {
    let a = TestLibrary::new();
    let b = TestLibrary::new();
    let c = TestLibrary::new();
    a.copy_fixture("tiny.jpg", "a-only.jpg");
    b.copy_fixture("sample_with_exif.jpg", "b-only.jpg");
    a.scan();
    b.scan();

    let server_a = Server::start_in(&a, &c.root);
    let server_b = Server::start_in(&b, &c.root);
    let body_a = server_a.get_body("/api/files");
    let body_b = server_b.get_body("/api/files");

    assert!(
        body_a.contains("a-only.jpg"),
        "server A must serve its own file"
    );
    assert!(
        !body_a.contains("b-only.jpg"),
        "server A must not serve B's file"
    );
    assert!(
        body_b.contains("b-only.jpg"),
        "server B must serve its own file"
    );
    assert!(
        !body_b.contains("a-only.jpg"),
        "server B must not serve A's file"
    );
    assert!(
        !c.db().exists(),
        "the invocation directory must not become a library"
    );
}
