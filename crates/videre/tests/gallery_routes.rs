//! `videre gallery` serves pages, and until now nothing asked it for one.
//!
//! The command shipped in 0.18.0 with no route coverage. That went unnoticed
//! because `tests/report.rs` was exercising the same rendering code through the
//! command gallery replaced, so the suite looked healthy while the new entry
//! point was untested. Removing `report` in 0.20.0 made the gap visible.
//!
//! These tests drive the real binary over a real socket rather than calling
//! handlers directly, because the thing worth checking is that the server binds,
//! routes, and answers. A handler that returns the right String while the router
//! never reaches it is exactly the failure this file exists to catch.
//!
//! :warning: HTTP is spoken by hand over `TcpStream` on purpose. A one-line GET
//! and a status line need no HTTP client, and adding a dev-dependency to assert
//! `200 OK` would be a poor trade.

mod common;
use common::isolated_home;

use rusqlite::Connection;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// A database with one file, one face, and one named person, so the gallery has
/// something to render on every view rather than only exercising empty states.
fn fixture(dir: &Path) -> PathBuf {
    let db = dir.join("gallery.db");
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
         INSERT INTO file_hashes (path, hash, ext, size_bytes, exif_date)
           VALUES ('/tmp/a.jpg', 'abc123', 'jpg', 1024, '2025-06-03T15:08:23');
         INSERT INTO faces (hash, bbox, embedding, cluster_id, person_label, confirmed)
           VALUES ('abc123', '0,0,50,50', X'0000', 1, 'ozgur_demirtas', 1);
         INSERT INTO people (name, full_name) VALUES ('ozgur_demirtas', 'Özgür');",
    )
    .unwrap();
    videre_core::db::ensure_file_hashes_columns(&conn);
    db
}

/// Ask the OS for a free port, then let it go so the server can take it.
///
/// :warning: There is an unavoidable gap between releasing the port and the
/// child binding it, so `STARTUP` below serialises that window. Without it two
/// tests in this file were handed the same port and one of them connected to
/// the other's server, which failed as a read error rather than as anything
/// legible.
///
/// `--port 0` would remove the race entirely, but `videre gallery` prints the
/// port it was asked for rather than the one it bound, so the test cannot learn
/// where to connect.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Held from picking a port until the child is accepting connections on it.
static STARTUP: Mutex<()> = Mutex::new(());

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
    /// Starts the gallery and waits until the port actually accepts a
    /// connection. Sleeping a fixed interval instead makes a slow machine look
    /// like a broken server.
    fn start(db: &Path) -> Server {
        isolated_home();
        let _serialised = STARTUP.lock().unwrap_or_else(|e| e.into_inner());
        let port = free_port();
        let child = Command::new(env!("CARGO_BIN_EXE_videre"))
            .arg("gallery")
            .arg("--db")
            .arg(db)
            .arg("--port")
            .arg(port.to_string())
            .spawn()
            .expect("failed to spawn videre gallery");
        let server = Server { child, port };

        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("videre gallery did not start listening on port {port}");
    }

    /// Returns (status code, body).
    fn get(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))
            .unwrap_or_else(|e| panic!("connect for {path}: {e}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or_else(|| {
                panic!("no status line for {path}: {}", &text[..text.len().min(80)])
            });
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        (status, body.to_string())
    }
}

#[test]
fn every_live_route_answers() {
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    for path in ["/", "/people", "/date"] {
        let (status, body) = server.get(path);
        assert_eq!(status, 200, "{path} did not return 200");
        assert!(
            body.contains("<html") || body.contains("<!doctype") || body.contains("<div"),
            "{path} returned 200 but no markup"
        );
    }
}

// :warning: The reserved views answer **404 with a page**, which is deliberate
// and easy to mistake for a bug. The status says the view does not exist yet;
// the body says so in words and links back. Registering them is what makes the
// intended shape visible in the router.
//
// The pair of tests below is the point: a reserved route and a typo both return
// 404, so only the body tells them apart. Asserting the status alone would pass
// even if `/map` were deleted from the router entirely.
#[test]
fn a_reserved_route_returns_404_with_an_explanation() {
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    for path in ["/map", "/events", "/smart"] {
        let (status, body) = server.get(path);
        assert_eq!(status, 404, "{path} should report that it is not built yet");
        assert!(
            body.contains("Not built yet"),
            "{path} 404s without saying it is reserved, so it reads as a missing route"
        );
    }
}

#[test]
fn an_unregistered_path_404s_with_no_such_explanation() {
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    let (status, body) = server.get("/definitely-not-a-route");
    assert_eq!(status, 404);
    assert!(
        !body.contains("Not built yet"),
        "an unrouted path must not look like a reserved view"
    );
}

// :warning: `/people` is a shell. The person list is fetched by the page from
// `/api/faces` rather than rendered into the HTML, so asserting on names in
// that document would fail while the view worked perfectly. The data contract
// is what to check.
#[test]
fn the_people_data_carries_identity_and_display_name_separately() {
    // The fixture's display name does not normalise back to its identity:
    // `ozgur_demirtas` is shown as `Özgür`. That divergence is the point, the
    // same shape videre-api's person_surfaces tests use, because a surface that
    // only ever sees agreeing values proves nothing.
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    let (status, body) = server.get("/api/faces");
    assert_eq!(status, 200);
    assert!(
        body.contains("ozgur_demirtas"),
        "the faces API did not carry the person's identity: {body}"
    );
    assert!(
        body.contains("Özgür") || body.contains("\\u00d6"),
        "the faces API did not carry the display name, so a renamed person would show as their identity: {body}"
    );
}

#[test]
fn a_person_page_renders() {
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    let (status, _) = server.get("/person/ozgur_demirtas");
    assert_eq!(status, 200, "a labelled person's page should render");
}

#[test]
fn the_api_the_labeling_ui_depends_on_answers_json() {
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    let (status, body) = server.get("/api/faces");
    assert_eq!(status, 200, "/api/faces did not return 200");
    serde_json::from_str::<serde_json::Value>(&body)
        .unwrap_or_else(|e| panic!("/api/faces returned invalid JSON: {e}\nbody: {body}"));
}

/// :warning: `--port 0` must report the port it BOUND, not the one it was asked
/// for. It used to print `http://127.0.0.1:0`, so a server started that way was
/// unreachable short of `lsof`, and `--browse` opened the same dead address.
///
/// This is also the mechanism that would let `Server::start` above drop its
/// mutex: with a trustworthy announced port there is no free-port race to
/// serialise.
#[test]
fn port_zero_announces_the_port_it_actually_bound() {
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    isolated_home();

    let mut child = Command::new(env!("CARGO_BIN_EXE_videre"))
        .arg("gallery")
        .arg("--db")
        .arg(&db)
        .arg("--port")
        .arg("0")
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn videre gallery --port 0");

    let stderr = BufReader::new(child.stderr.take().unwrap());
    let mut announced = None;
    for line in stderr.lines().map_while(Result::ok) {
        if let Some(rest) = line.split("http://127.0.0.1:").nth(1) {
            announced = rest.trim().parse::<u16>().ok();
            break;
        }
    }
    let port = announced.unwrap_or_else(|| {
        child.kill().ok();
        panic!("gallery never announced an address");
    });

    let connected = TcpStream::connect(("127.0.0.1", port)).is_ok();
    child.kill().ok();
    child.wait().ok();

    assert_ne!(port, 0, "announced port 0, which cannot be connected to");
    assert!(
        connected,
        "announced port {port} but nothing was listening there"
    );
}

// ---- /api/files, the endpoint the gallery will fetch its rows from ----------
//
// :warning: The first version of this endpoint returned an empty page when its
// query failed to prepare, so a malformed `view=date` looked exactly like a
// library with nothing in it. These assert row counts rather than only status,
// because a 200 carrying nothing is the failure that actually happened.

/// Returns (total, number of rows returned).
fn files_page(server: &Server, query: &str) -> (i64, usize) {
    let (status, body) = server.get(&format!("/api/files?{query}"));
    assert_eq!(status, 200, "/api/files?{query} did not return 200");
    let v: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("invalid JSON for {query}: {e}"));
    (
        v["total"].as_i64().expect("total"),
        v["files"].as_array().expect("files").len(),
    )
}

#[test]
fn the_files_endpoint_pages_both_views() {
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    // The fixture holds one file, so both views see it and paging is trivial
    // but real: the arithmetic is what is being pinned, not the volume.
    for view in ["all", "date"] {
        let (total, n) = files_page(&server, &format!("view={view}&limit=10"));
        assert_eq!(total, 1, "view={view} reported the wrong total");
        assert_eq!(n, 1, "view={view} returned the wrong number of rows");
    }
}

#[test]
fn an_offset_past_the_end_is_an_empty_page_not_an_error() {
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    let (total, n) = files_page(&server, "offset=9999&limit=10");
    assert_eq!(total, 1, "total must describe the view, not the page");
    assert_eq!(n, 0, "a page past the end should be empty");
}

#[test]
fn limit_is_capped_so_a_client_cannot_ask_for_the_library() {
    // The whole point of the endpoint is that no single response carries
    // everything, so an unbounded limit would defeat it.
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    let (_, n) = files_page(&server, "limit=100000");
    assert!(n <= 500, "limit was not capped: {n} rows returned");
}

#[test]
fn each_row_carries_its_copy_count() {
    // `copies` is why the client no longer needs the whole array: it used to
    // scan everything to count files per hash, a number the database had.
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    let (status, body) = server.get("/api/files?limit=1");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let copies = v["files"][0]["copies"]
        .as_i64()
        .expect("every row must carry copies");
    assert_eq!(
        copies, 1,
        "the fixture has one file, so one copy of its hash"
    );
}

#[test]
fn an_unknown_view_falls_back_rather_than_failing() {
    // An unknown view is a client bug. Rejecting it would render as an empty
    // gallery with no explanation, which is worse than showing all files.
    let dir = tempdir().unwrap();
    let db = fixture(dir.path());
    let server = Server::start(&db);

    let (total, n) = files_page(&server, "view=nonsense&limit=10");
    assert_eq!(total, 1);
    assert_eq!(n, 1);
}
