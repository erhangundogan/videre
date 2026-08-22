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
use common::isolated_home;

use rusqlite::Connection;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Enough rows that inlining them is unmistakably over the ceiling, while the
/// fixture still builds in a second or two.
const FILES: usize = 3_000;

/// Roughly one file in three carries a labelled face, which is the shape of a
/// real library and enough for a reintroduced crop bug to be obvious.
const FACES: usize = 1_000;

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

fn seed(dir: &Path) -> Option<PathBuf> {
    // :warning: Before anything resolves a path. The embeddings database lives
    // under the videre home, so seeding it before the home is isolated writes
    // it where the server will not look, and the fixture then silently fails to
    // exercise the payload it exists to measure.
    isolated_home();

    let pics = dir.join("pics");
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

    let db = dir.join("payload.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
         bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
         cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
         is_primary INTEGER DEFAULT 0);
         CREATE TABLE people (name TEXT PRIMARY KEY, full_name TEXT);
         INSERT INTO people (name, full_name) VALUES ('ozgur_demirtas', 'Özgür');",
    )
    .unwrap();
    videre_core::db::ensure_file_hashes_columns(&conn);

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
        tx.execute(
            "INSERT INTO file_hashes
               (path, hash, size_bytes, modified_at, ext, exif_date, gps_lat, gps_lon, width, height)
             VALUES (?1, ?2, ?3, ?4, 'jpg', ?4, ?5, ?6, 16, 16)",
            rusqlite::params![
                path.to_string_lossy(),
                format!("{i:064x}"),
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

    seed_embeddings(&db);
    Some(db)
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
fn seed_embeddings(db: &Path) {
    let model = videre_core::embeddings::DEFAULT_MODEL_ID;
    let path = videre_core::embeddings_db::db_path(db, model).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(&path).unwrap();
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
    fn start(db: &Path) -> Server {
        isolated_home();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = Command::new(env!("CARGO_BIN_EXE_videre"))
            .arg("gallery")
            .arg("--db")
            .arg(db)
            .arg("--port")
            .arg(port.to_string())
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
            .map(|(_, b)| b.len())
            .unwrap_or(0)
    }
}

fn assert_under_ceiling(route: &str) {
    let dir = tempdir().unwrap();
    let Some(db) = seed(dir.path()) else {
        skip_no_fixture();
        return;
    };
    let server = Server::start(&db);
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

/// The one that already passes, and the reason the other two are worth writing:
/// `/people` has fetched its data since it was written, and is the model the
/// other routes are moving toward.
#[test]
fn the_people_page_already_fetches_its_data() {
    let dir = tempdir().unwrap();
    let Some(db) = seed(dir.path()) else {
        skip_no_fixture();
        return;
    };
    let server = Server::start(&db);
    let len = server.get_len("/people");
    assert!(
        len <= CEILING,
        "/people served {len} bytes, over the {CEILING} byte ceiling. It fetches \
         from /api/faces, so this should not be possible without a regression."
    );
}
