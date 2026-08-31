//! Byte-level characterization of the renderer at the stable CLI/HTTP boundary.
//!
//! Scaffolding for the report.rs split (RenderSet reshape). The interfaces
//! captured here - `dedupe --html` and the gallery `/`, `/duplicates`, `/date`
//! routes - do not change across the refactor, so identical normalized output
//! before and after proves the reshape preserved behaviour. Deleted once the
//! split lands (see the plan's Task 4).
//!
//! Normalisation strips the two volatile pieces: the generation timestamp and
//! every occurrence of the tempdir path. Set VIDERE_BLESS=1 to (re)write the
//! golden files instead of comparing.

mod common;
use common::isolated_home;

use rusqlite::Connection;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn videre_bin() -> PathBuf {
    isolated_home();
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    p.pop(); // debug/
    p.push("videre");
    p
}

/// A DB with a duplicate group (hdup x2), a singleton (hsing), and dated rows,
/// plus real files on disk so the existence filter passes. Deterministic.
fn fixture(dir: &Path) -> (PathBuf, [PathBuf; 3]) {
    let pics = dir.join("pics");
    std::fs::create_dir(&pics).unwrap();
    let files = [pics.join("a.jpg"), pics.join("b.jpg"), pics.join("c.jpg")];
    for f in &files {
        std::fs::write(f, b"dummy").unwrap();
    }
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (
            path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
            created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
            exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);",
    )
    .unwrap();
    videre_core::db::ensure_file_hashes_columns(&conn);
    for (path, hash, date) in [
        (files[0].to_str().unwrap(), "hdup", "2025-06-03T15:08:23"),
        (files[1].to_str().unwrap(), "hdup", "2025-06-03T15:08:23"),
        (files[2].to_str().unwrap(), "hsing", "2024-01-09T09:00:00"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext, exif_date)
             VALUES (?1, ?2, 100, 'jpg', ?3)",
            rusqlite::params![path, hash, date],
        )
        .unwrap();
    }
    (db, files)
}

/// Replace the two volatile pieces with fixed tokens: the generation timestamp
/// ("YYYY-MM-DD HH:MM UTC", the one " UTC" in the document) and the tempdir path.
fn normalize(html: &str, dir: &Path) -> String {
    let mut out = html.to_string();
    while let Some(sp) = out.find(" UTC") {
        if sp < 16 {
            break;
        }
        let start = sp - 16;
        let w = out.as_bytes();
        if w[start + 4] == b'-' && w[start + 7] == b'-' && w[start + 10] == b' ' {
            out.replace_range(start..sp + 4, "<GENERATED_AT>");
        } else {
            break; // a " UTC" that is not our timestamp: stop, do not loop forever
        }
    }
    out.replace(dir.to_str().unwrap(), "<TMP>")
}

/// Compare against a committed golden, or rewrite it when VIDERE_BLESS=1.
fn assert_golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden")
        .join(name);
    if std::env::var("VIDERE_BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {name}; run once with VIDERE_BLESS=1"));
    assert_eq!(actual, expected, "golden mismatch for {name}");
}

#[test]
fn golden_dedupe_html() {
    let dir = tempdir().unwrap();
    let (db, _files) = fixture(dir.path());
    let out = dir.path().join("out.html");
    let status = Command::new(videre_bin())
        .args([
            "dedupe",
            "--html",
            out.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let html = std::fs::read_to_string(&out).unwrap();
    assert_golden("dedupe.html", &normalize(&html, dir.path()));
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_gallery(db: &Path, port: u16) -> Server {
    let child = Command::new(videre_bin())
        .args([
            "gallery",
            "--db",
            db.to_str().unwrap(),
            "--port",
            &port.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Server(child);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("gallery did not start on port {port}");
}

fn get_body(port: u16, path: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut reader = BufReader::new(s);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }
    let mut body = String::new();
    reader.read_to_string(&mut body).unwrap();
    body
}

#[test]
fn golden_gallery_pages() {
    let dir = tempdir().unwrap();
    let (db, _files) = fixture(dir.path());
    let port = free_port();
    let _server = start_gallery(&db, port);
    for (route, name) in [
        ("/", "gallery_root.html"),
        ("/duplicates", "gallery_duplicates.html"),
        ("/date", "gallery_date.html"),
    ] {
        let body = get_body(port, route);
        assert_golden(name, &normalize(&body, dir.path()));
    }
}
