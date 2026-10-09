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
use common::TestLibrary;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// A database with one file, one face, and one named person, so the gallery has
/// something to render on every view rather than only exercising empty states.
fn fixture() -> TestLibrary {
    let lib = TestLibrary::new();
    let path = lib.context().paths.root.join("a.jpg");
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext, size_bytes, exif_date)
           VALUES (?1, 'abc123', 'jpg', 1024, '2025-06-03T15:08:23')",
        [path.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO people (name, full_name) VALUES ('ozgur_demirtas', 'Özgür');
         INSERT INTO faces (hash, bbox, embedding, cluster_id, person_label, confirmed)
           VALUES ('abc123', '0,0,50,50', X'0000', 1, 'ozgur_demirtas', 1);",
    )
    .unwrap();
    lib
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

/// A GET on `port`: (status code, body), or `None` when nothing answers with
/// a status line.
fn request(port: u16, path: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split_whitespace().nth(1)?.parse().ok()?;
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    Some((status, body.to_string()))
}

/// Held from picking a port until the child is accepting connections on it.
static STARTUP: Mutex<()> = Mutex::new(());

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        common::stop_gallery(&mut self.child, self.port);
    }
}

impl Server {
    /// Starts the gallery and waits until the port actually accepts a
    /// connection. Sleeping a fixed interval instead makes a slow machine look
    /// like a broken server.
    fn start(lib: &TestLibrary) -> Server {
        Server::start_with_hf_home(lib, None)
    }

    /// `hf_home` points the child's model cache somewhere fresh, so a download
    /// that should not happen lands where a test can see it instead of being
    /// absorbed by the developer's warm cache.
    fn start_with_hf_home(lib: &TestLibrary, hf_home: Option<&Path>) -> Server {
        // Route tests check the server alone; the watch it would start beside
        // it has tests of its own (tests/gallery_watch.rs).
        lib.no_gallery_watch();
        let _serialised = STARTUP.lock().unwrap_or_else(|e| e.into_inner());
        // Our own library's settings path, which only our server reports.
        let ours = lib.context().paths.root.display().to_string();
        // :warning: `STARTUP` serialises this file's servers only. Another test
        // binary, running in parallel, can be handed the same free port in the
        // gap before our child binds it: a connection then reaches its server,
        // ours exits for want of the port, and our first request is refused
        // once the other one stops (seen on CI). So a server counts as started
        // only when it serves our library, and a lost port is retried.
        for _ in 0..5 {
            let port = free_port();
            let mut cmd = lib.cmd();
            cmd.arg("gallery").arg("--port").arg(port.to_string());
            if let Some(hf) = hf_home {
                cmd.env("HF_HOME", hf);
            }
            let mut child = cmd.spawn().expect("failed to spawn videre gallery");
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if child.try_wait().unwrap().is_some() {
                    break; // the port was taken; try another
                }
                if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    let serves_ours = request(port, "/api/settings")
                        .is_some_and(|(_, body)| body.contains(&ours));
                    if serves_ours && child.try_wait().unwrap().is_none() {
                        return Server { child, port };
                    }
                    break; // someone else's server on our port
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // Never through `stop_gallery`: its quit request would go to
            // whatever server holds the port, which may not be ours.
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("videre gallery did not start serving this library");
    }

    /// Returns (status code, body).
    fn get(&self, path: &str) -> (u16, String) {
        request(self.port, path).unwrap_or_else(|| panic!("no answer for {path}"))
    }

    /// Sends a request with a body and returns (status, body). One connection
    /// per request, like `get`.
    fn send(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))
            .unwrap_or_else(|e| panic!("connect for {method} {path}: {e}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or_else(|| panic!("no status line for {method} {path}"));
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        (status, body.to_string())
    }

    /// A GET with an optional `Range` header. Returns (status, response head,
    /// raw body), for the file-serving routes whose Range handling matters.
    fn get_range(&self, path: &str, range: Option<&str>) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))
            .unwrap_or_else(|e| panic!("connect for {path}: {e}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let range = range.map(|r| format!("Range: {r}\r\n")).unwrap_or_default();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{range}Connection: close\r\n\r\n"
        )
        .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let sep = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or_else(|| panic!("no header end for {path}"));
        let head = String::from_utf8_lossy(&raw[..sep]).to_lowercase();
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        (status, head, raw[sep + 4..].to_vec())
    }

    /// Like `get`, but returns the raw body bytes and the Content-Type, for
    /// binary responses (a poster JPEG) that `get`'s lossy-UTF-8 body mangles.
    // Only the macOS-gated poster test needs raw bytes; unused off macOS.
    #[cfg(target_os = "macos")]
    fn get_bytes(&self, path: &str) -> (u16, Option<String>, Vec<u8>) {
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
        // Split headers from body on the first CRLFCRLF, byte-wise.
        let sep = raw.windows(4).position(|w| w == b"\r\n\r\n");
        let (head, body) = match sep {
            Some(i) => (&raw[..i], raw[i + 4..].to_vec()),
            None => (&raw[..], Vec::new()),
        };
        let head_text = String::from_utf8_lossy(head);
        let status = head_text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let content_type = head_text.lines().find_map(|l| {
            l.split_once(':').and_then(|(k, v)| {
                (k.trim().eq_ignore_ascii_case("content-type")).then(|| v.trim().to_string())
            })
        });
        (status, content_type, body)
    }
}

#[test]
fn a_heic_at_the_failure_threshold_is_refused_without_reconverting() {
    // A HEIC already known undecodable (at the two-strike threshold) must be
    // refused before the expensive, possibly-hanging QuickLook conversion, so it
    // stops re-paying the timeout on every tile request. Proven two ways at once:
    // the request returns 404 promptly (a failed conversion instead waits the
    // full ~20s qlmanage timeout, which would trip the socket read timeout in
    // `get`), and no new strike is recorded (an un-gated path would run the
    // conversion, fail, and record a third).
    //
    // The record/clear side is not exercised here on purpose: making the
    // conversion fail means hanging QuickLook, the very cost this gate exists to
    // avoid. Recording uses the same `decode_failures` calls the embed/faces
    // tests cover.
    let lib = TestLibrary::new();
    let heic_path = lib.context().paths.root.join("shot.heic");
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, size_bytes)
               VALUES (?1, 'heic1', 'heic', 32)",
            [heic_path.to_string_lossy().as_ref()],
        )
        .unwrap();

    let server = Server::start(&lib);
    // Seed the strikes AFTER start: startup clears STAGE_THUMBNAIL (the per-run
    // reset, see gallery_start_clears_thumbnail_decode_failures), so seeding
    // before start would be wiped. The server's own connection sees these
    // committed rows through WAL.
    {
        let conn = lib.conn();
        for _ in 0..videre_core::decode_failures::FAILURE_THRESHOLD {
            videre_core::decode_failures::record(
                &conn,
                "heic1",
                videre_core::decode_failures::STAGE_THUMBNAIL,
                "seeded",
            )
            .unwrap();
        }
    }

    let (status, _) = server.get("/api/files/heic1/raw?size=240");
    assert_eq!(status, 404, "a threshold-failed heic thumbnail is refused");

    let count = videre_core::decode_failures::fail_count(
        &lib.conn(),
        "heic1",
        videre_core::decode_failures::STAGE_THUMBNAIL,
    )
    .unwrap();
    assert_eq!(
        count,
        videre_core::decode_failures::FAILURE_THRESHOLD,
        "the gate returned before the conversion, so no new strike was recorded"
    );
}

#[test]
fn gallery_start_clears_thumbnail_decode_failures() {
    // STAGE_THUMBNAIL is the one skip with no --reprocess hatch, so a new gallery
    // run clears it: a HEIC skipped after two transient failures last run gets
    // another chance rather than being a permanently broken tile.
    let lib = TestLibrary::new();
    let heic_path = lib.context().paths.root.join("shot.heic");
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext, size_bytes)
           VALUES (?1, 'heic1', 'heic', 32)",
        [heic_path.to_string_lossy().as_ref()],
    )
    .unwrap();
    videre_core::decode_failures::ensure_table(&conn).unwrap();
    for _ in 0..videre_core::decode_failures::FAILURE_THRESHOLD {
        videre_core::decode_failures::record(
            &conn,
            "heic1",
            videre_core::decode_failures::STAGE_THUMBNAIL,
            "previous run",
        )
        .unwrap();
    }
    drop(conn);

    // Startup runs the clear before the port accepts, so by the time start
    // returns the records are gone.
    let _server = Server::start(&lib);
    let count = videre_core::decode_failures::fail_count(
        &lib.conn(),
        "heic1",
        videre_core::decode_failures::STAGE_THUMBNAIL,
    )
    .unwrap();
    assert_eq!(
        count, 0,
        "a new gallery run retries previously-skipped thumbnails"
    );
}

#[test]
fn a_video_at_the_thumbnail_failure_threshold_is_refused() {
    // A video whose poster conversion keeps failing is refused before QuickLook,
    // the same two-strike gate as HEIC. Cross-platform: the gate returns before
    // any conversion, so nothing runs and no strike is added.
    let lib = TestLibrary::new();
    let mov_path = lib.context().paths.root.join("clip.mov");
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, size_bytes) VALUES (?1, 'vid1', 'mov', 32)",
            [mov_path.to_string_lossy().as_ref()],
        )
        .unwrap();

    let server = Server::start(&lib);
    // Seed after start (startup clears STAGE_THUMBNAIL); the server sees these
    // committed rows through WAL.
    {
        let conn = lib.conn();
        for _ in 0..videre_core::decode_failures::FAILURE_THRESHOLD {
            videre_core::decode_failures::record(
                &conn,
                "vid1",
                videre_core::decode_failures::STAGE_THUMBNAIL,
                "seeded",
            )
            .unwrap();
        }
    }

    let (status, _) = server.get("/api/files/vid1/raw?size=240");
    assert_eq!(status, 404, "a threshold-failed video poster is refused");
    let count = videre_core::decode_failures::fail_count(
        &lib.conn(),
        "vid1",
        videre_core::decode_failures::STAGE_THUMBNAIL,
    )
    .unwrap();
    assert_eq!(
        count,
        videre_core::decode_failures::FAILURE_THRESHOLD,
        "the gate returned before conversion, so no new strike was recorded"
    );
}

/// Asserts a `bytes=0-15` answer: 206, the right Content-Range, and exactly
/// those bytes of `file`.
fn assert_first_sixteen_bytes(server: &Server, route: &str, file: &Path) {
    let whole = std::fs::read(file).unwrap();
    let (status, head, body) = server.get_range(route, Some("bytes=0-15"));
    assert_eq!(status, 206, "{route}: {head}");
    assert!(
        head.contains(&format!("content-range: bytes 0-15/{}", whole.len())),
        "{route}: {head}"
    );
    assert_eq!(body, whole[..16], "{route}");

    let (status, head, body) = server.get_range(route, None);
    assert_eq!(status, 200, "{route}: {head}");
    assert!(head.contains("accept-ranges: bytes"), "{route}: {head}");
    assert_eq!(body.len(), whole.len(), "{route}");

    let (status, head, _) = server.get_range(route, Some(&format!("bytes={}-", whole.len())));
    assert_eq!(status, 416, "{route}: a range past the end: {head}");
}

#[test]
fn a_raw_video_is_served_by_byte_range() {
    // Browsers read video metadata with Range requests; a full body for each
    // ties up the connection pool (see the raw route).
    let lib = TestLibrary::new();
    let dst = lib
        .copy_fixture("red_1s.mp4", "Kadıköy/vapur.mp4")
        .canonicalize()
        .unwrap();
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, size_bytes) VALUES (?1, 'vid1', 'mp4', 1000)",
            [dst.to_string_lossy().as_ref()],
        )
        .unwrap();
    let server = Server::start(&lib);
    assert_first_sixteen_bytes(&server, "/api/files/vid1/raw", &dst);
}

#[test]
fn the_basemap_archive_is_served_by_byte_range() {
    // MapLibre's pmtiles protocol reads the archive only through Range requests.
    let lib = TestLibrary::new();
    drop(lib.init_db());
    let archive = lib
        .context()
        .paths
        .state
        .join("basemap")
        .join("basemap.pmtiles");
    std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basemap-tiny.pmtiles"),
        &archive,
    )
    .unwrap();
    let server = Server::start(&lib);
    assert_first_sixteen_bytes(&server, "/tiles/basemap.pmtiles", &archive);
}

#[test]
#[cfg(target_os = "macos")]
fn a_sized_video_request_returns_an_oriented_poster_jpeg() {
    // The sized endpoint returns a QuickLook poster image, not the raw video.
    // (The old behavior read the whole file into memory and returned it with the
    // video mime type - the latent bug this fix also closes.) macOS-gated:
    // poster extraction is QuickLook.
    let lib = TestLibrary::new();
    // Match the canonical root stored by LibraryContext on macOS, where
    // tempfile may return /var while the filesystem resolves to /private/var.
    let dst = lib
        .copy_fixture("red_1s.mp4", "clip.mp4")
        .canonicalize()
        .unwrap();
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, size_bytes) VALUES (?1, 'vid1', 'mp4', 1000)",
            [dst.to_string_lossy().as_ref()],
        )
        .unwrap();

    let server = Server::start(&lib);
    let (status, content_type, body) = server.get_bytes("/api/files/vid1/raw?size=240");
    assert_eq!(status, 200, "the poster is served");
    assert_eq!(
        content_type.as_deref(),
        Some("image/jpeg"),
        "served as a jpeg poster, not the raw video"
    );
    assert!(
        body.starts_with(&[0xFF, 0xD8]),
        "the body is a JPEG (starts with the SOI marker)"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn a_conversion_waits_for_a_machine_wide_quicklook_slot() {
    // Another videre process holding every QuickLook slot (here: this test,
    // through its own open files) must hold the gallery's conversion back
    // until one is released, however many permits the gallery has itself.
    use fs2::FileExt;
    let lib = TestLibrary::new();
    let dst = lib
        .copy_fixture("red_1s.mp4", "clip.mp4")
        .canonicalize()
        .unwrap();
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, size_bytes) VALUES (?1, 'vid1', 'mp4', 1000)",
            [dst.to_string_lossy().as_ref()],
        )
        .unwrap();
    let slots = lib.home.join(".cache/videre/locks/quicklook");
    std::fs::create_dir_all(&slots).unwrap();
    let held: Vec<std::fs::File> = (0..6)
        .map(|i| {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(slots.join(format!("slot-{i}.lock")))
                .unwrap();
            file.try_lock_exclusive().unwrap();
            file
        })
        .collect();

    let server = Server::start(&lib);
    std::thread::scope(|scope| {
        let request = scope.spawn(|| server.get_bytes("/api/files/vid1/raw?size=240"));
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            !request.is_finished(),
            "the poster was rendered while every slot was held"
        );
        drop(held);
        let (status, _, body) = request.join().unwrap();
        assert_eq!(status, 200, "the poster is served once a slot is free");
        assert!(body.starts_with(&[0xFF, 0xD8]));
    });
}

#[test]
fn the_empty_duplicates_page_points_at_the_command_that_finds_similar_photos() {
    // Near-duplicates are never shown on this page: `videre embed` computes
    // their fingerprints and `videre dedupe --kind similar` lists them. The hint
    // must say so, not send anyone to the removed `scan --similar`.
    let lib = fixture();
    let server = Server::start(&lib);
    let (status, body) = server.get("/duplicates");
    assert_eq!(status, 200);
    assert!(body.contains("No duplicates"), "{body}");
    assert!(
        body.contains("<code>videre dedupe --kind similar</code>"),
        "{body}"
    );
    assert!(!body.contains("appear here"), "{body}");
    assert!(!body.contains("scan --similar"), "{body}");
}

#[test]
fn every_live_route_answers() {
    let lib = fixture();
    let server = Server::start(&lib);

    for path in ["/", "/people", "/date"] {
        let (status, body) = server.get(path);
        assert_eq!(status, 200, "{path} did not return 200");
        assert!(
            body.contains("<html") || body.contains("<!doctype") || body.contains("<div"),
            "{path} returned 200 but no markup"
        );
    }
}

#[test]
fn no_embeddings_tells_the_client_to_hide_the_similar_button() {
    // fixture() seeds a file and a face but never embeds, so the active model
    // has no vectors and the in-page Similar search cannot work. The page must
    // say so, so the client can omit the Similar button instead of offering one
    // that fails with a generic "Search failed".
    let lib = fixture();
    let server = Server::start(&lib);
    let (status, body) = server.get("/");
    assert_eq!(status, 200);
    assert!(
        body.contains("var HAS_EMBEDDINGS=false"),
        "the all-files page must emit HAS_EMBEDDINGS=false when the library has no embeddings:\n{}",
        &body[..body.len().min(400)]
    );
    assert!(
        !body.contains("var HAS_EMBEDDINGS=true"),
        "no embeddings exist, so the flag must not be true"
    );
}

#[test]
fn the_gallery_page_includes_the_vendored_justified_layout() {
    let lib = fixture();
    let server = Server::start(&lib);
    let (status, body) = server.get("/");
    assert_eq!(status, 200);
    assert!(
        body.contains("justified-layout v4.1.0"),
        "the vendored justified-layout sentinel is missing from the page"
    );
    assert!(
        body.contains("justifiedLayout"),
        "the justifiedLayout global is not present in the page"
    );
}

#[test]
fn the_files_page_has_the_list_tile_toggle() {
    let lib = fixture();
    let server = Server::start(&lib);
    let (status, body) = server.get("/");
    assert_eq!(status, 200);
    assert!(
        body.contains("class=\"view-mode-select\""),
        "the Files page is missing the List/Tile toggle"
    );
}

#[test]
fn the_date_page_has_the_list_tile_toggle() {
    let lib = fixture();
    let server = Server::start(&lib);
    let (status, body) = server.get("/date");
    assert_eq!(status, 200);
    assert!(
        body.contains("class=\"view-mode-select\""),
        "the Date page is missing the List/Tile toggle"
    );
}

#[test]
fn date_prefix_routes_render_with_initial_state() {
    let lib = fixture();
    let server = Server::start(&lib);

    for (path, prefix) in [
        ("/date/2025", "\"value\":\"2025\""),
        ("/date/2025/06", "\"value\":\"2025-06\""),
        ("/date/2025/06/03", "\"value\":\"2025-06-03\""),
    ] {
        let (status, body) = server.get(path);
        assert_eq!(status, 200, "{path}");
        assert!(body.contains("var GDATE="), "{path} does not emit GDATE");
        assert!(
            body.contains("\"kind\":\"prefix\""),
            "{path} is not a prefix page"
        );
        assert!(body.contains(prefix), "{path} has the wrong initial prefix");
    }
}

#[test]
fn date_range_routes_render_with_normalized_initial_state() {
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/date?from=2016-05&to=2017");
    assert_eq!(status, 200);
    assert!(body.contains("\"kind\":\"range\""));
    assert!(body.contains("\"from\":\"2016-05-01\""));
    assert!(body.contains("\"to\":\"2017-01-01\""));

    let (status, body) = server.get("/date?to=2018");
    assert_eq!(status, 200);
    assert!(body.contains("\"from\":null"));
    assert!(body.contains("\"to\":\"2018-01-01\""));
}

#[test]
fn invalid_date_page_routes_return_the_right_status() {
    let lib = fixture();
    let server = Server::start(&lib);

    for path in [
        "/date/abcd",
        "/date/2025/6",
        "/date/2025/13",
        "/date/2025/02/31",
    ] {
        let (status, _) = server.get(path);
        assert_eq!(status, 404, "{path}");
    }

    for path in [
        "/date?from=banana",
        "/date?from=2025-13",
        "/date?from=2017&to=2016",
        "/date?from=2025-05-10&to=2025-05-10",
    ] {
        let (status, _) = server.get(path);
        assert_eq!(status, 400, "{path}");
    }
}

#[test]
fn live_date_pages_link_the_drill_down_routes() {
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/date");
    assert_eq!(status, 200);
    // The card links to the drill-down route, keeping any query.
    assert!(body.contains("href=\"'+escA(withQuery(dateHref(prefix)))"));
    assert!(body.contains("return '/date/'+String(prefix)"));

    let (status, body) = server.get("/date/2025");
    assert_eq!(status, 200);
    assert!(body.contains("\"value\":\"2025\""));
    assert!(body.contains("level='month'"));

    let (status, body) = server.get("/date/2025/06");
    assert_eq!(status, 200);
    assert!(body.contains("\"value\":\"2025-06\""));
    assert!(body.contains("level='day'"));
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
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/smart");
    assert_eq!(status, 404, "/smart should report that it is not built yet");
    assert!(
        body.contains("Not built yet"),
        "/smart 404s without saying it is reserved, so it reads as a missing route"
    );
}

#[test]
fn map_route_renders_with_nav_marked_and_grid_rows() {
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/map");
    assert_eq!(status, 200);
    assert!(body.contains("id=\"map-plot\""));
    assert!(body.contains("id=\"gallery\""));
    assert!(body.contains("<a href=\"/map\" class=\"on\">"));
}

#[test]
fn map_location_route_renders_the_map_with_a_location_bootstrap() {
    let lib = fixture();
    let conn = lib.conn();
    videre_core::location_cluster::ensure_location_clusters_table(&conn).unwrap();
    conn.execute(
        "INSERT INTO location_clusters
            (centroid_lat, centroid_lon, name, photo_count, radius_km, created_at)
         VALUES (52.52, 13.405, 'Berlin', 1, 20.0, CURRENT_TIMESTAMP)",
        [],
    )
    .unwrap();
    drop(conn);
    let server = Server::start(&lib);

    let (status, body) = server.get("/map/location/berlin");
    assert_eq!(status, 200);
    assert!(body.contains("id=\"map-plot\""));
    assert!(body.contains("var GLOC={\"kind\":\"location\",\"name\":\"berlin\",\"radius\":20.0};"));
    assert!(body.contains("<a href=\"/map\" class=\"on\">"));
}

#[test]
fn map_route_on_a_library_that_never_ran_locations_shows_the_empty_state() {
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/map");
    assert_eq!(status, 200);
    assert!(
        body.contains("Run <code>videre locations</code>"),
        "the empty state must name the command"
    );
}

#[test]
fn an_unregistered_path_404s_with_no_such_explanation() {
    let lib = fixture();
    let server = Server::start(&lib);

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
    let lib = fixture();
    let server = Server::start(&lib);

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
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, _) = server.get("/people/person/ozgur_demirtas");
    assert_eq!(status, 200, "a labelled person's page should render");
}

// :warning: **These routes had no test at all**, which is why they sat at the
// root for so long and why their back link pointed at a page that stopped being
// the labeling UI when `gallery` gained routes.
//
// The pair below is the point: the same two pages must live under `/people` on a
// gallery server and at the root on a labeling-only one, because that server has
// no `/people`. Testing only the configuration you are looking at is what makes
// the other one break silently.

#[test]
fn the_labeling_sub_pages_live_under_people_on_a_gallery() {
    let lib = fixture();
    let server = Server::start(&lib);

    for path in ["/people/cluster/1", "/people/person/ozgur_demirtas"] {
        let (status, _) = server.get(path);
        assert_eq!(status, 200, "{path} should render under the gallery");
    }
    for path in ["/cluster/1", "/person/ozgur_demirtas"] {
        let (status, _) = server.get(path);
        assert_eq!(
            status, 404,
            "{path} should not exist on a gallery; these pages belong under /people"
        );
    }
}

#[test]
fn the_back_link_returns_to_the_labeling_ui_not_the_file_list() {
    let lib = fixture();
    let server = Server::start(&lib);

    for path in ["/people/cluster/1", "/people/person/ozgur_demirtas"] {
        let (_, body) = server.get(path);
        assert!(
            body.contains("href=\"/people\""),
            "{path} does not link back to /people, so `back` leaves labeling for \
             the file list"
        );
        assert!(
            !body.contains("Back to labeling"),
            "{path} still says \"Back to labeling\"; the nav calls that section People"
        );
    }
}

#[test]
fn processing_reports_outstanding_work_only_while_watch_runs() {
    let lib = fixture();
    // A second photo nothing has detected faces on yet.
    let path = lib.context().paths.root.join("çiçek.jpg");
    lib.conn()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, size_bytes) VALUES (?1, 'def456', 'jpg', 10)",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/processing");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json, serde_json::json!({"watch": false, "stages": []}));

    // A running watch holds the library's `watch` lock and records the stages
    // it runs. One that does not detect faces leaves them out of the note.
    let _watch = videre_core::library_locks::try_command(&lib.context(), "watch").unwrap();
    videre_core::library_state::set_string(
        &lib.conn(),
        videre_core::status_report::WATCH_STAGES,
        "scan,location",
    )
    .unwrap();
    let (_, body) = server.get("/api/processing");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["stages"], serde_json::json!([]), "{body}");

    videre_core::library_state::set_string(
        &lib.conn(),
        videre_core::status_report::WATCH_STAGES,
        "scan,faces,location,embed",
    )
    .unwrap();
    let (_, body) = server.get("/api/processing");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["watch"], true, "{body}");
    assert_eq!(
        json["stages"],
        serde_json::json!([{"stage": "faces", "outstanding": 1}]),
        "embed is left out while its model is not downloaded: {body}"
    );
}

#[test]
fn the_api_the_labeling_ui_depends_on_answers_json() {
    let lib = fixture();
    let server = Server::start(&lib);

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
    let lib = fixture();
    lib.no_gallery_watch();

    let mut child = lib
        .cmd()
        .args(["gallery", "--port", "0"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn videre gallery --port 0");

    let stderr = BufReader::new(child.stderr.take().unwrap());
    let mut announced = None;
    for line in stderr.lines().map_while(Result::ok) {
        if let Some(rest) = line.split("http://127.0.0.1:").nth(1) {
            // Only the digits: a gallery resuming at a saved page prints its
            // path after the port.
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            announced = digits.parse::<u16>().ok();
            break;
        }
    }
    let port = announced.unwrap_or_else(|| {
        child.kill().ok();
        panic!("gallery never announced an address");
    });

    let connected = TcpStream::connect(("127.0.0.1", port)).is_ok();
    common::stop_gallery(&mut child, port);

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
    let lib = fixture();
    let server = Server::start(&lib);

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
    let lib = fixture();
    let server = Server::start(&lib);

    let (total, n) = files_page(&server, "offset=9999&limit=10");
    assert_eq!(total, 1, "total must describe the view, not the page");
    assert_eq!(n, 0, "a page past the end should be empty");
}

#[test]
fn limit_is_capped_so_a_client_cannot_ask_for_the_library() {
    // The whole point of the endpoint is that no single response carries
    // everything, so an unbounded limit would defeat it.
    let lib = fixture();
    let server = Server::start(&lib);

    let (_, n) = files_page(&server, "limit=100000");
    assert!(n <= 500, "limit was not capped: {n} rows returned");
}

#[test]
fn each_row_carries_its_copy_count() {
    // `copies` is why the client no longer needs the whole array: it used to
    // scan everything to count files per hash, a number the database had.
    let lib = fixture();
    let server = Server::start(&lib);

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
    let lib = fixture();
    let server = Server::start(&lib);

    let (total, n) = files_page(&server, "view=nonsense&limit=10");
    assert_eq!(total, 1);
    assert_eq!(n, 1);
}

// ---- /api/dates, the tree a page of rows cannot build ----------------------

#[test]
fn the_date_tree_comes_from_the_whole_library() {
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/dates?level=year");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let buckets = v["buckets"].as_array().expect("buckets");
    assert_eq!(buckets.len(), 1, "the fixture has one file, so one year");
    assert_eq!(buckets[0]["key"], "2025");
    assert_eq!(buckets[0]["count"], 1);
    // Each card shows a thumbnail, so a bucket without a representative row
    // would render an empty tile.
    assert!(
        buckets[0]["sample"]["path"].is_string(),
        "a bucket must carry a sample row for its preview"
    );
}

#[test]
fn drilling_down_narrows_the_tree() {
    let lib = fixture();
    let server = Server::start(&lib);

    for (q, key) in [
        ("level=month&parent=2025", "2025-06"),
        ("level=day&parent=2025-06", "2025-06-03"),
    ] {
        let (status, body) = server.get(&format!("/api/dates?{q}"));
        assert_eq!(status, 200, "{q}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["buckets"][0]["key"], key, "{q} returned the wrong bucket");
    }
}

// :warning: The count and the rows must describe the same set. Counting a
// subquery that exposed only `hash` once returned 0 while the page returned
// rows, because the date expression had no columns to read and the failure was
// swallowed by a default.
#[test]
fn a_dated_page_agrees_with_its_own_total() {
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/files?view=date&date=2025-06-03&limit=100");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let n = v["files"].as_array().unwrap().len() as i64;
    let total = v["total"].as_i64().unwrap();
    assert_eq!(n, 1, "the fixture has one file on that day");
    assert_eq!(
        total, n,
        "total ({total}) disagrees with the rows returned ({n})"
    );
}

#[test]
fn a_date_that_matches_nothing_is_empty_rather_than_everything() {
    // A filter that fails to apply would return the whole library, which reads
    // as a working day view containing every photo ever taken.
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/files?view=date&date=1999-01-01&limit=100");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["files"].as_array().unwrap().len(), 0);
    assert_eq!(v["total"], 0);
}

#[test]
fn the_files_endpoint_filters_date_ranges() {
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/files?view=date&from=2025-06-01&to=2025-07-01&limit=100");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["total"], 1);
    assert_eq!(v["files"].as_array().unwrap().len(), 1);

    let (status, body) = server.get("/api/files?view=date&from=2025-07-01&to=2025-08-01&limit=100");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["total"], 0);
    assert_eq!(v["files"].as_array().unwrap().len(), 0);
}

#[test]
fn invalid_files_endpoint_date_ranges_are_bad_requests() {
    let lib = fixture();
    let server = Server::start(&lib);

    for path in [
        "/api/files?view=date&from=banana",
        "/api/files?view=date&from=2025-13-01",
        "/api/files?view=date&from=2025-07-01&to=2025-06-01",
    ] {
        let (status, _) = server.get(path);
        assert_eq!(status, 400, "{path}");
    }
}

// :warning: A view nobody can reach is the same class of fault as a view that
// never renders: it works, nothing reports a problem, and no one sees it.
//
// `/date` shipped reachable only by typing the URL. Every route test in this
// file passed, because each one asks for a path directly and none of them asked
// how a person would get there. These two do.
#[test]
fn every_gallery_view_links_to_the_others() {
    let lib = fixture();
    let server = Server::start(&lib);

    for path in ["/", "/duplicates", "/date", "/events", "/people", "/map"] {
        let (status, body) = server.get(path);
        assert_eq!(status, 200, "{path} did not render");
        for target in [
            "href=\"/\"",
            "href=\"/duplicates\"",
            "href=\"/date\"",
            "href=\"/events\"",
            "href=\"/people\"",
            "href=\"/map\"",
        ] {
            assert!(
                body.contains(target),
                "{path} has no {target} link, so that view is reachable only by \
                 typing its URL"
            );
        }
    }
}

/// The current section is marked, or the strip cannot say where you are.
///
/// This is what the typed `Section` protects: a mistyped string would still
/// render three links and simply highlight none of them, which looks like a
/// styling glitch rather than a bug.
#[test]
fn the_current_section_is_marked_on_each_view() {
    let lib = fixture();
    let server = Server::start(&lib);

    for (path, expected) in [
        ("/", "<a href=\"/\" class=\"on\">"),
        ("/duplicates", "<a href=\"/duplicates\" class=\"on\">"),
        ("/date", "<a href=\"/date\" class=\"on\">"),
        ("/events", "<a href=\"/events\" class=\"on\">"),
        ("/people", "<a href=\"/people\" class=\"on\">"),
        ("/map", "<a href=\"/map\" class=\"on\">"),
    ] {
        let (_, body) = server.get(path);
        assert!(
            body.contains(expected),
            "{path} does not mark itself as the current section"
        );
        assert_eq!(
            body.matches("class=\"on\"").count(),
            1,
            "{path} marks more than one section as current"
        );
    }
}

/// Embeddings for the search tests: three orthogonal-ish vectors so the ranking
/// has a knowable answer rather than an arbitrary one.
fn seed_search_embeddings(lib: &TestLibrary, hashes: &[(&str, [f32; 4])]) {
    let model = videre_core::embeddings::DEFAULT_MODEL_ID;
    let path = videre_core::embeddings_db::db_path_in(&lib.context(), model).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embeddings
         (hash TEXT PRIMARY KEY, model_id TEXT NOT NULL, embedding BLOB NOT NULL);",
    )
    .unwrap();
    for (hash, v) in hashes {
        // The same conversion the app uses, rather than a `half` dev-dependency
        // that could drift from it.
        let blob = videre_core::vectors::to_f16_bytes(v);
        conn.execute(
            "INSERT OR REPLACE INTO embeddings VALUES (?1, ?2, ?3)",
            rusqlite::params![hash, model, blob],
        )
        .unwrap();
    }
}

/// A library of four files with known embeddings, for ranking assertions.
fn search_fixture() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let p = |name: &str| root.join(name).to_string_lossy().into_owned();
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext, size_bytes) VALUES
           (?1, 'aaaa', 'jpg', 10),
           (?2, 'bbbb', 'jpg', 10),
           (?3, 'cccc', 'jpg', 10),
           (?4, 'dddd', 'jpg', 10)",
        rusqlite::params![p("a.jpg"), p("b.jpg"), p("c.jpg"), p("d.jpg")],
    )
    .unwrap();
    drop(conn);
    seed_search_embeddings(
        &lib,
        &[
            ("aaaa", [1.0, 0.0, 0.0, 0.0]),
            ("bbbb", [0.9, 0.1, 0.0, 0.0]),
            ("cccc", [0.5, 0.5, 0.0, 0.0]),
            ("dddd", [0.0, 1.0, 0.0, 0.0]),
        ],
    );
    lib
}

// :warning: These use `?like=`, never `?q=`. A text query needs the model, and
// a test that downloads 778MB of SigLIP is the exact failure `CLAUDE.md` records
// from `classify`. Ranking against a stored vector needs no model at all, which
// is precisely why the stored path exists.

#[test]
fn similarity_ranks_by_closeness_to_the_example() {
    let lib = search_fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/search?like=aaaa&limit=10");
    assert_eq!(status, 200);
    let hashes: Vec<&str> = body
        .split("\"hash\":\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    assert_eq!(
        hashes,
        vec!["bbbb", "cccc", "dddd"],
        "expected nearest-first ordering, got {body}"
    );
}

#[test]
fn similar_drops_neighbours_below_the_library_s_floor() {
    let lib = search_fixture();
    // Scores against aaaa (the fixture's vectors are not normalized, so
    // these are its dot products): bbbb 0.9, cccc 0.5, dddd 0.
    std::fs::write(
        lib.root.join(".videre/config.toml"),
        "similar_min_score = 0.4\n",
    )
    .unwrap();
    let server = Server::start(&lib);
    let (status, body) = server.get("/api/search?like=aaaa&limit=10");
    assert_eq!(status, 200, "{body}");
    let hashes: Vec<&str> = body
        .split("\"hash\":\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    assert_eq!(hashes, vec!["bbbb", "cccc"], "{body}");
}

/// The example is not one of its own neighbours.
///
/// The in-page version skipped its own index; this must too, or the first and
/// best result slot is spent showing the picture you already clicked.
#[test]
fn the_example_is_absent_from_its_own_results() {
    let lib = search_fixture();
    let server = Server::start(&lib);

    let (_, body) = server.get("/api/search?like=aaaa&limit=10");
    assert!(
        !body.contains("\"aaaa\""),
        "the example ranked itself, at ~1.0, wasting the top slot: {body}"
    );
}

/// The Search page shows more by asking for the next page of one ranking, so
/// pages must continue each other: no gap, no repeat, and never the example.
#[test]
fn search_pages_continue_one_ranking() {
    let lib = search_fixture();
    let server = Server::start(&lib);
    let hashes = |body: &str| -> Vec<String> {
        body.split("\"hash\":\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap().to_string())
            .collect()
    };
    let mut paged = Vec::new();
    let mut more = Vec::new();
    for offset in 0..4 {
        let (status, body) = server.get(&format!("/api/search?like=aaaa&limit=1&offset={offset}"));
        assert_eq!(status, 200, "{body}");
        paged.extend(hashes(&body));
        more.push(body.contains("\"more\":true"));
    }
    assert_eq!(paged, vec!["bbbb", "cccc", "dddd"]);
    // A next page exists after the first and second, not after the last.
    assert_eq!(more, vec![true, true, false, false]);

    let (_, body) = server.get("/api/search?like=aaaa&limit=2&offset=1");
    assert_eq!(hashes(&body), vec!["cccc", "dddd"], "{body}");
}

#[test]
fn a_search_needs_exactly_one_ranker() {
    let lib = search_fixture();
    let server = Server::start(&lib);

    for path in ["/api/search", "/api/search?q=cat&like=aaaa"] {
        let (status, _) = server.get(path);
        assert_eq!(status, 400, "{path} should be rejected as ambiguous");
    }
}

/// An unembedded example is an error, not an empty result.
///
/// Same rule as the rest of this work: a plausible zero is worse than a failure,
/// because "no similar images" reads as an answer.
#[test]
fn an_example_with_no_embedding_is_an_error() {
    let lib = search_fixture();
    let server = Server::start(&lib);

    let (status, _) = server.get("/api/search?like=nosuchhash");
    assert_eq!(status, 500);
}

/// :warning: **A gallery whose library nobody searches must never load a model.**
///
/// The search endpoint holds a lazily-loaded embedder, and "lazily" is the whole
/// contract. `CLAUDE.md` records what the alternative costs: a model loaded
/// before there was work to do downloaded 778MB from inside a unit test, and on
/// CI those weights then woke a skipped test and took one job from 3 minutes to
/// nearly 40.
///
/// Browsing must therefore leave a cold cache untouched. This drives every route
/// a person hits without searching and asserts nothing was fetched.
#[test]
fn browsing_the_gallery_touches_no_model_cache() {
    let lib = search_fixture();
    let hf_home = tempdir().unwrap();
    let server = Server::start_with_hf_home(&lib, Some(hf_home.path()));

    for path in [
        "/",
        "/date",
        "/events",
        "/people",
        "/api/files?view=all&limit=10",
        "/api/events",
        "/api/dates?level=year",
        // The ranked path too: an example already embedded needs no model, which
        // is the reason the stored-vector path exists rather than re-embedding.
        "/api/search?like=aaaa",
    ] {
        let (status, _) = server.get(path);
        assert_eq!(status, 200, "{path} did not answer");
    }
    drop(server);

    let mut found = Vec::new();
    let mut stack = vec![hf_home.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                found.push(p);
            }
        }
    }
    assert!(
        found.is_empty(),
        "the gallery downloaded model weights without anyone running a text \
         search: {found:?}"
    );
}

/// Face learning is off unless `gallery.json` turns it on.
fn enable_learning(lib: &TestLibrary) {
    std::fs::write(
        lib.root.join(".videre/gallery.json"),
        r#"{"faces":{"learning":true}}"#,
    )
    .unwrap();
}

#[test]
fn face_learning_resources_serve_from_a_fresh_library() {
    let lib = fixture();
    enable_learning(&lib);
    let server = Server::start(&lib);

    assert!(
        videre_core::db::table_exists(&lib.conn(), "face_learning_questions").unwrap(),
        "gallery startup must prepare question storage before the worker runs"
    );

    let (status, body) = server.get("/api/face-learning/questions");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body.trim(), "[]", "{body}");

    let (status, body) = server.get("/api/face-learning/status");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"generation\":0"), "{body}");
    assert!(body.contains("\"pending_questions\":0"), "{body}");

    let (status, body) = server.get("/api/face-learning/events");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body.trim(), "[]", "{body}");

    let (status, body) = server.get("/api/face-learning/events/424242");
    assert_eq!(status, 404, "{body}");

    let (status, body) = server.send(
        "POST",
        "/api/face-learning/questions/424242/answer",
        "{\"answer\":\"yes\"}",
    );
    assert_eq!(status, 404, "{body}");

    let (status, body) = server.send(
        "POST",
        "/api/face-learning/questions/424242/answer",
        "{\"answer\":\"maybe\"}",
    );
    assert_eq!(status, 400, "{body}");
}

#[test]
fn teaching_mutations_return_acknowledgements_and_advance_the_generation() {
    let lib = fixture();
    {
        // One unassigned face with a real two-dimensional f16 embedding, so
        // the teaching path can extract membership features for its event.
        let conn = lib.init_db();
        let mut embedding = Vec::new();
        embedding.extend_from_slice(&half::f16::from_f32(1.0).to_le_bytes());
        embedding.extend_from_slice(&half::f16::from_f32(0.0).to_le_bytes());
        // The fixture face ships a placeholder zero embedding; the teaching
        // path extracts real membership features, so both faces need valid,
        // matching-dimension vectors.
        conn.execute(
            "UPDATE faces SET embedding = ?1, cluster_id = NULL",
            rusqlite::params![embedding],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
             VALUES ('abc123', '0,0,50,50', ?1, NULL, 0)",
            rusqlite::params![embedding],
        )
        .unwrap();
    }
    enable_learning(&lib);
    let server = Server::start(&lib);

    let (status, body) = server.send(
        "PUT",
        "/api/people/ozgur_demirtas/faces",
        "{\"face_ids\":[2]}",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"generation\":1"), "{body}");
    assert!(body.contains("\"event_ids\":["), "{body}");
    assert!(!body.contains("embedding"), "{body}");

    // The background worker may have trained (and failed on tiny evidence)
    // by the time we look; the generation is the contract here.
    let (status, body) = server.get("/api/face-learning/status");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"generation\":1"), "{body}");

    let (status, body) = server.get("/api/face-learning/events");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"action\":\"assign_face\""), "{body}");
    // Provenance and scalar features only: no raw embedding payload.
    assert!(!body.contains("\"embedding\":"), "{body}");
    assert!(body.contains("\"target_identity\":"), "{body}");

    let (status, body) = server.get("/api/face-learning/events/1");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"faces\":"), "{body}");
}

#[test]
fn labeling_pages_carry_the_learning_hooks() {
    let lib = fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/people");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("data-learning-status"), "{body}");
    assert!(body.contains("data-learning-question"), "{body}");
    assert!(body.contains("learning-toast"), "{body}");

    let (status, body) = server.get("/people/person/ozgur_demirtas");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("data-learning-history"), "{body}");
}

#[test]
fn face_learning_boundary_only_user_mutations_change_faces() {
    let lib = fixture();
    {
        // Two unassigned singletons with real embeddings, so the teaching
        // action can extract features and the worker can train or fail.
        let conn = lib.init_db();
        let mut embedding = Vec::new();
        embedding.extend_from_slice(&half::f16::from_f32(1.0).to_le_bytes());
        embedding.extend_from_slice(&half::f16::from_f32(0.0).to_le_bytes());
        for id in [2, 3] {
            conn.execute(
                "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed)
                 VALUES (?1, 'abc123', '0,0,50,50', ?2, NULL, 0)",
                rusqlite::params![id, embedding],
            )
            .unwrap();
        }
        conn.execute(
            "UPDATE faces SET embedding = ?1, cluster_id = NULL",
            rusqlite::params![embedding],
        )
        .unwrap();
    }
    enable_learning(&lib);
    let server = Server::start(&lib);

    let face_rows =
        |conn: &rusqlite::Connection| -> Vec<(i64, Option<i64>, Option<String>, i64, i64)> {
            let mut statement = conn
                .prepare(
                    "SELECT id, cluster_id, person_label, confirmed, is_primary
                      FROM faces ORDER BY id",
                )
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
    let before = {
        let conn = lib.init_db();
        face_rows(&conn)
    };

    // The user mutation: assign one singleton. Exactly that face changes.
    let (status, body) = server.send(
        "PUT",
        "/api/people/ozgur_demirtas/faces",
        "{\"face_ids\":[2]}",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"generation\":1"), "{body}");

    // Wait until the background worker settles (it may succeed, fail, or wait
    // for more feedback on this tiny evidence; either way it must not touch
    // faces). A worker that
    // never settles fails the test instead of letting it pass unobserved.
    // Training starts after a 10 s quiet window.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, status_body) = server.get("/api/face-learning/status");
        let settled = status_body.contains("\"status\":\"current\"")
            || status_body.contains("\"status\":\"failed\"")
            || status_body.contains("\"status\":\"waiting\"");
        if settled {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the background worker never settled: {status_body}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    {
        let conn = lib.init_db();
        let after = face_rows(&conn);
        assert_eq!(after.len(), before.len(), "no face rows added or deleted");
        for (was, is) in before.iter().zip(&after) {
            assert_eq!(was.0, is.0);
            if was.0 == 2 {
                // Only the explicitly named face may change.
                assert_eq!(is.2.as_deref(), Some("ozgur_demirtas"));
                assert_eq!(is.3, 1);
                assert_eq!(is.1, None, "assigning detaches the machine cluster");
            } else {
                assert_eq!(was, is, "no other face row may move");
            }
        }
    }
}

/// Learning is off by default: naming works the same and records nothing,
/// and the learning resources say so.
#[test]
fn face_learning_is_off_by_default() {
    let lib = fixture();
    {
        let conn = lib.init_db();
        let mut embedding = Vec::new();
        embedding.extend_from_slice(&half::f16::from_f32(1.0).to_le_bytes());
        embedding.extend_from_slice(&half::f16::from_f32(0.0).to_le_bytes());
        conn.execute(
            "UPDATE faces SET embedding = ?1, cluster_id = NULL",
            rusqlite::params![embedding],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
             VALUES ('abc123', '0,0,50,50', ?1, NULL, 0)",
            rusqlite::params![embedding],
        )
        .unwrap();
    }
    let server = Server::start(&lib);

    let (status, body) = server.send(
        "PUT",
        "/api/people/ozgur_demirtas/faces",
        "{\"face_ids\":[2]}",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"event_ids\":[]"), "{body}");
    assert!(body.contains("\"message_key\":\"learning_off\""), "{body}");
    let named: Option<String> = lib
        .conn()
        .query_row("SELECT person_label FROM faces WHERE id = 2", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        named.as_deref(),
        Some("ozgur_demirtas"),
        "the name is stored"
    );
    let events: i64 = lib
        .conn()
        .query_row("SELECT count(*) FROM face_learning_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(events, 0, "nothing is recorded while learning is off");

    let (status, body) = server.get("/api/face-learning/status");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"enabled\":false"), "{body}");
    assert!(body.contains("\"generation\":0"), "{body}");

    let (status, body) = server.get("/api/face-learning/questions");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body.trim(), "[]", "{body}");

    let (status, body) = server.send(
        "POST",
        "/api/face-learning/questions/1/answer",
        "{\"answer\":\"yes\"}",
    );
    assert_eq!(status, 503, "{body}");

    let (status, body) = server.get("/api/faces/cluster-params");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"learning\":null"), "{body}");
}

/// A 512-dim f16 embedding near `center`, as the faces table stores one.
/// Deterministic noise, so the run is repeatable.
fn face_embedding(center: usize, seed: u64) -> Vec<u8> {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut values = [0f32; 512];
    for value in values.iter_mut() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *value = ((state >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.2;
    }
    values[center] += 1.0;
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    values
        .iter()
        .flat_map(|v| half::f16::from_f32(v / norm).to_le_bytes())
        .collect()
}

/// The freeze this change fixed: a training run on a person with many faces
/// spent seconds building its snapshot while holding the connection every
/// request shares, so the whole gallery stopped answering. Learning on, one
/// person large enough that the build takes seconds, a cluster named: every
/// request keeps answering quickly while the run is in progress.
#[test]
fn a_training_run_never_holds_up_requests() {
    const PERSON_FACES: u64 = 400;
    let lib = fixture();
    {
        let conn = lib.init_db();
        conn.execute(
            "UPDATE faces SET embedding = ?1, cluster_id = NULL",
            rusqlite::params![face_embedding(0, 0)],
        )
        .unwrap();
        conn.execute_batch("BEGIN").unwrap();
        for seed in 1..PERSON_FACES {
            conn.execute(
                "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed, det_score, blur)
                 VALUES ('abc123', '0,0,50,50', ?1, 'ozgur_demirtas', 1, 0.9, 600.0)",
                rusqlite::params![face_embedding(0, seed)],
            )
            .unwrap();
        }
        // A new person's cluster, named below.
        for seed in 0..3 {
            conn.execute(
                "INSERT INTO faces (hash, bbox, embedding, cluster_id, confirmed, det_score, blur)
                 VALUES ('abc123', '0,0,50,50', ?1, 77, 0, 0.9, 600.0)",
                rusqlite::params![face_embedding(1, 10_000 + seed)],
            )
            .unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();
    }
    let cluster: Vec<i64> = {
        let conn = lib.conn();
        let mut statement = conn
            .prepare("SELECT id FROM faces WHERE cluster_id = 77 ORDER BY id")
            .unwrap();
        let ids = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<i64>, _>>()
            .unwrap();
        ids
    };
    enable_learning(&lib);
    let server = Server::start(&lib);

    let (status, body) = server.send(
        "POST",
        "/api/people",
        &format!("{{\"name\":\"Şebnem\",\"face_ids\":{cluster:?}}}"),
    );
    assert_eq!(status, 200, "{body}");

    let mut saw_training = false;
    let mut slowest = Duration::ZERO;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        for path in ["/api/faces", "/api/face-learning/status"] {
            let began = Instant::now();
            let (status, body) = server.get(path);
            let took = began.elapsed();
            assert_eq!(status, 200, "{path}: {body}");
            slowest = slowest.max(took);
            assert!(
                took < Duration::from_millis(500),
                "{path} took {took:?} while learning ran"
            );
            if path.ends_with("status") {
                if body.contains("\"status\":\"training\"") {
                    saw_training = true;
                } else if saw_training && !body.contains("\"status\":\"stale\"") {
                    // Settled after a run we watched.
                    eprintln!("slowest response during training: {slowest:?}");
                    return;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "training never ran or never settled (saw training: {saw_training})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The duplicates page shows a Google Takeout edit beside its original, the
/// original first (it is the one kept).
#[test]
fn duplicates_page_shows_google_photos_edit_pairs() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Photos from 2015/IMG_1.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "Photos from 2015/IMG_1-edited.jpg");
    lib.scan();
    let server = Server::start(&lib);
    let (status, body) = server.get("/duplicates");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"kind\":\"creation\""), "{body}");
    let original = body.find("IMG_1.jpg").expect("original listed");
    let edit = body.find("IMG_1-edited.jpg").expect("edit listed");
    assert!(original < edit, "the kept original comes first");
}

/// An exact pair, a Google creation beside its original, and two near
/// fingerprints at one size (similar, review only).
fn kinds_library() -> TestLibrary {
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "tatil/deniz.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "yedek/deniz.jpg");
    lib.copy_fixture("ai-generated-couple.jpg", "Photos/IMG_1.jpg");
    lib.copy_fixture("tiny.jpg", "Photos/IMG_1-edited.jpg");
    lib.copy_fixture("corrupt.jpg", "seri/IMG_1816.jpg");
    std::fs::write(lib.root.join("seri/IMG_1817.jpg"), b"another burst frame").unwrap();
    lib.scan();
    let conn = lib.conn();
    for (name, phash) in [
        ("IMG_1816.jpg", 0x0F0F_3C3C_5A5A_6969_i64),
        ("IMG_1817.jpg", 0x0F0F_3C3C_5A5A_6968),
    ] {
        conn.execute(
            "UPDATE file_hashes SET phash = ?1 WHERE path LIKE '%' || ?2",
            rusqlite::params![phash, name],
        )
        .unwrap();
    }
    lib
}

fn save_kinds(lib: &TestLibrary, kinds: &[&str]) {
    std::fs::write(
        lib.root.join(".videre/gallery.json"),
        serde_json::json!({ "routes": { "duplicates": { "kinds": kinds } } }).to_string(),
    )
    .unwrap();
}

#[test]
fn duplicates_page_shows_the_saved_kinds() {
    let lib = kinds_library();
    let server = Server::start(&lib);
    let (status, body) = server.get("/duplicates");
    assert_eq!(status, 200);
    assert!(body.contains("\"kind\":\"exact\""), "{body}");
    assert!(body.contains("\"kind\":\"creation\""));
    assert!(
        !body.contains("\"kind\":\"similar\""),
        "similar is off by default"
    );
    assert!(body.contains("Exact copies:</span> 1"), "{body}");
    assert!(body.contains("Google Photos creations:</span> 1"));
    assert!(body.contains("var DUP_KINDS=[\"exact\",\"resized\",\"creation\"];"));

    save_kinds(&lib, &["similar"]);
    let (_, body) = server.get("/duplicates");
    assert!(body.contains("\"kind\":\"similar\""), "{body}");
    assert!(!body.contains("\"kind\":\"exact\""));
    assert!(body.contains("Similar (review only):</span> 1"));
}

#[test]
fn duplicates_page_says_how_many_resized_candidates_are_unchecked() {
    let lib = TestLibrary::new();
    let conn = lib.init_db();
    let root = lib.context().paths.root;
    for (name, hash, w, phash) in [
        ("çiçek.jpg", "hb", 1600, 0x0F0F_3C3C_5A5A_6969_i64),
        ("çiçek-wa.jpg", "hs", 1280, 0x0F0F_3C3C_5A5A_6968),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext, mime, width, height, phash)
             VALUES (?1, ?2, 100, 'jpg', 'image/jpeg', ?3, ?3 * 3 / 4, ?4)",
            rusqlite::params![root.join(name).to_string_lossy().as_ref(), hash, w, phash],
        )
        .unwrap();
    }
    drop(conn);
    let server = Server::start(&lib);
    let (_, body) = server.get("/duplicates");
    assert!(
        body.contains("2 possible resized copies are not checked yet")
            && body.contains("<code>videre dedupe --kind resized</code>"),
        "{body}"
    );
}

#[test]
fn trashing_copies_from_the_page_keeps_keepers_and_undo_restores() {
    let lib = kinds_library();
    let server = Server::start(&lib);
    let (status, body) = server.send("POST", "/api/duplicates/trash", r#"{"kinds":["similar"]}"#);
    assert_eq!(status, 400, "similar is review-only: {body}");

    let keeper = lib.root.join("Photos/IMG_1.jpg");
    // The page sends the keeper's path as the library stores it.
    let stored: String = lib
        .conn()
        .query_row(
            "SELECT path FROM file_hashes WHERE path LIKE '%/Photos/IMG_1.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let (status, body) = server.send(
        "POST",
        "/api/duplicates/trash",
        &serde_json::json!({ "kinds": ["exact", "creation"], "keepers": [stored] }).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["removed"], 1, "only the chosen group: {body}");
    assert!(keeper.exists());
    assert!(!lib.root.join("Photos/IMG_1-edited.jpg").exists());
    assert!(lib.root.join("tatil/deniz.jpg").exists() && lib.root.join("yedek/deniz.jpg").exists());

    let (status, body) = server.send("POST", "/api/duplicates/trash", r#"{"kinds":["exact"]}"#);
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["removed"], 1, "{body}");
    let left = [
        lib.root.join("tatil/deniz.jpg"),
        lib.root.join("yedek/deniz.jpg"),
    ]
    .iter()
    .filter(|p| p.exists())
    .count();
    assert_eq!(left, 1, "the keeper stays");
    drop(server);

    for _ in 0..2 {
        let out = lib
            .cmd()
            .args(["dedupe", "undo", "--yes", "--silent"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(lib.root.join("Photos/IMG_1-edited.jpg").exists());
    assert!(lib.root.join("tatil/deniz.jpg").exists() && lib.root.join("yedek/deniz.jpg").exists());
}

// ---- people and faces over HTTP -------------------------------------------

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON ({e}): {body}"))
}

/// The fixture's person with two named faces (1 and 4), plus an unassigned
/// cluster 7 of two faces. Named faces carry no cluster id, as labeling
/// leaves them.
fn people_fixture() -> TestLibrary {
    let lib = fixture();
    let conn = lib.init_db();
    conn.execute_batch(
        "UPDATE faces SET cluster_id = NULL WHERE id = 1;
         INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed)
           VALUES (2, 'abc123', '0,0,50,50', X'0000', 7, NULL, 0),
                  (3, 'abc123', '60,0,50,50', X'0000', 7, NULL, 0),
                  (4, 'abc123', '0,60,50,50', X'0000', NULL, 'ozgur_demirtas', 1);",
    )
    .unwrap();
    lib
}

#[test]
fn singles_and_a_persons_faces_page_over_http() {
    let lib = people_fixture();
    lib.conn()
        .execute_batch(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed)
               VALUES (10, 'abc123', '0,0,9,9', X'0000', NULL, NULL, 0),
                      (11, 'abc123', '0,0,9,9', X'0000', NULL, NULL, 0),
                      (12, 'abc123', '0,0,9,9', X'0000', NULL, NULL, 0),
                      (13, 'abc123', '0,0,9,9', X'0000', NULL, NULL, 0),
                      (14, 'abc123', '0,0,9,9', X'0000', NULL, NULL, 0);",
        )
        .unwrap();
    let server = Server::start(&lib);
    let ids = |v: &serde_json::Value| -> Vec<i64> {
        v["singletons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["face_id"].as_i64().unwrap())
            .collect()
    };

    let all = json(&server.get("/api/faces").1);
    assert_eq!(ids(&all), vec![10, 11, 12, 13, 14]);
    assert_eq!(
        (all["singles_total"].as_i64(), all["singles_next"].as_i64()),
        (Some(5), None)
    );

    let first = json(&server.get("/api/faces?singles_limit=2").1);
    assert_eq!(ids(&first), vec![10, 11]);
    assert_eq!(first["singles_next"], 11);
    let second = json(&server.get("/api/faces?singles_after=11&singles_limit=2").1);
    assert_eq!(ids(&second), vec![12, 13]);

    // None at all refreshes people and clusters without touching the singles.
    let none = json(&server.get("/api/faces?singles_limit=0").1);
    assert!(ids(&none).is_empty());
    assert_eq!(none["singles_total"], 5);
    assert_eq!(none["people"].as_array().unwrap().len(), 1);
    assert_eq!(none["clusters"].as_array().unwrap().len(), 1);
    // Over the cap is clamped, not refused.
    assert_eq!(
        ids(&json(&server.get("/api/faces?singles_limit=99999").1)).len(),
        5
    );

    let (status, body) = server.get("/api/people/ozgur_demirtas?limit=1");
    assert_eq!(status, 200, "{body}");
    let one = json(&body);
    assert_eq!(one["faces"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(one["face_total"], 2, "{body}");
    let next = one["next"].as_i64().expect("a next page");
    let rest = json(
        &server
            .get(&format!("/api/people/ozgur_demirtas?after={next}&limit=1"))
            .1,
    );
    assert_eq!(rest["faces"].as_array().unwrap().len(), 1);
    assert!(rest["next"].is_null());
    assert_ne!(rest["faces"][0]["face_id"], one["faces"][0]["face_id"]);
}

#[test]
fn a_person_is_read_renamed_and_given_a_primary_face_over_http() {
    let lib = people_fixture();
    let server = Server::start(&lib);

    // Any spelling of the name reaches the same person.
    let (status, body) = server.get("/api/people/%C3%96zg%C3%BCr%20Demirta%C5%9F");
    assert_eq!(status, 200, "{body}");
    let person = json(&body);
    assert_eq!(person["label"], "ozgur_demirtas", "{body}");
    assert_eq!(person["full_name"], "Özgür", "{body}");
    assert_eq!(person["faces"].as_array().unwrap().len(), 2, "{body}");

    let (status, body) = server.send(
        "PATCH",
        "/api/people/ozgur_demirtas",
        r#"{"full_name":"Özi"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let (_, body) = server.get("/api/people/ozgur_demirtas");
    assert_eq!(json(&body)["full_name"], "Özi", "{body}");

    // The primary face is listed first and flagged.
    let (status, body) = server.send(
        "PATCH",
        "/api/faces/4",
        r#"{"person_label":"ozgur_demirtas"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let (_, body) = server.get("/api/people/ozgur_demirtas");
    let faces = json(&body)["faces"].clone();
    assert_eq!(faces[0]["face_id"], 4, "{body}");
    assert_eq!(faces[0]["is_primary"], true, "{body}");
    assert_eq!(faces[1]["is_primary"], false, "{body}");

    // A person search resolves the display name as typed, whatever its case,
    // to the identity behind it; "Özi" does not normalize to that identity,
    // so only the display-name match can find it. A partial name is no match.
    let (status, body) = server.get("/api/people?name=%C3%B6zi");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("a.jpg"), "{body}");
    let (_, body) = server.get("/api/people?name=ozgur");
    assert_eq!(json(&body), serde_json::json!([]), "{body}");
}

#[test]
fn faces_leave_people_and_clusters_dissolve_over_http() {
    let lib = people_fixture();
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/clusters/7");
    assert_eq!(status, 200, "{body}");
    let ids: Vec<i64> = json(&body)["faces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["face_id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [2, 3], "{body}");

    let (status, body) = server.send("DELETE", "/api/clusters/7", "");
    assert_eq!(status, 200, "{body}");
    let (_, body) = server.get("/api/clusters/7");
    assert_eq!(json(&body)["faces"], serde_json::json!([]), "{body}");
    // Dissolving an empty cluster is a missing one, not a silent success.
    let (status, _) = server.send("DELETE", "/api/clusters/7", "");
    assert_eq!(status, 404);

    let (status, body) = server.send("DELETE", "/api/faces/1", "");
    assert_eq!(status, 200, "{body}");
    let (_, body) = server.get("/api/people/ozgur_demirtas");
    let faces = json(&body)["faces"].clone();
    assert_eq!(faces.as_array().unwrap().len(), 1, "{body}");
    assert_eq!(faces[0]["face_id"], 4, "{body}");

    let (status, body) = server.send("DELETE", "/api/people/ozgur_demirtas", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["message_key"], "person_removed", "{body}");
    let (_, body) = server.get("/api/people/ozgur_demirtas");
    assert_eq!(json(&body)["faces"], serde_json::json!([]), "{body}");

    // A second delete finds nobody and says so.
    let (status, body) = server.send("DELETE", "/api/people/ozgur_demirtas", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        json(&body)["message_key"],
        "person_deleted_without_learning",
        "{body}"
    );
}

#[test]
fn marks_are_set_and_cleared_over_http() {
    let lib = fixture();
    let server = Server::start(&lib);
    let marks = |lib: &TestLibrary| -> (Option<i64>, Option<String>, i64) {
        lib.conn()
            .query_row(
                "SELECT rating, label, liked FROM marks WHERE hash = 'abc123'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };

    let (status, body) = server.send(
        "PATCH",
        "/api/files/abc123",
        r#"{"rating":4,"label":"green","liked":true}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(marks(&lib), (Some(4), Some("green".into()), 1));

    // Only the fields present change; rating 0 and label "none" clear.
    let (status, body) = server.send(
        "PATCH",
        "/api/files/abc123",
        r#"{"rating":0,"label":"none"}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(marks(&lib), (None, None, 1));

    // An empty change is accepted and writes nothing.
    let (status, body) = server.send("PATCH", "/api/files/abc123", "{}");
    assert_eq!(status, 200, "{body}");
    assert_eq!(marks(&lib), (None, None, 1));
}

/// Two HEICs in a scanned library: `portre.heic`, laid out like an iPhone
/// portrait (a grid sharing one `irot` with its thumbnail), and `irotsuz.heic`,
/// written without any `irot`. Returns the library and both content keys.
fn heic_library() -> (TestLibrary, String, String) {
    let lib = TestLibrary::new();
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    std::fs::copy(
        format!("{fixtures}/heic/grid_rot90.heic"),
        lib.root.join("portre.heic"),
    )
    .unwrap();
    let mut plain = std::fs::read(format!("{fixtures}/content_key/tiny.heic")).unwrap();
    let at = plain.windows(4).position(|w| w == b"irot").unwrap();
    plain[at..at + 4].copy_from_slice(b"frot");
    std::fs::write(lib.root.join("irotsuz.heic"), plain).unwrap();
    lib.scan();
    let conn = lib.conn();
    let hash_of = |name: &str| -> String {
        conn.query_row(
            "SELECT hash FROM file_hashes WHERE path LIKE '%' || ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap()
    };
    let (portrait, plain) = (hash_of("portre.heic"), hash_of("irotsuz.heic"));
    drop(conn);
    (lib, portrait, plain)
}

#[test]
fn a_heic_rotates_by_irot_and_its_faces_turn_with_it() {
    let (lib, portrait, plain) = heic_library();
    // The grid displays 1536x2048; a face near its top-left corner.
    lib.conn()
        .execute(
            "INSERT INTO faces (hash, bbox, embedding) VALUES (?1, '100,200,300,400', X'0000')",
            [&portrait],
        )
        .unwrap();
    let before = std::fs::read(lib.root.join("portre.heic")).unwrap();
    let plain_before = std::fs::read(lib.root.join("irotsuz.heic")).unwrap();
    let server = Server::start(&lib);

    let (status, body) = server.send("POST", &format!("/api/files/{portrait}/rotate"), "");
    assert_eq!(status, 200, "{body}");
    // irot 270 turned clockwise is 180, which EXIF calls 3.
    assert_eq!(body, r#"{"orientation":3}"#);
    let after = std::fs::read(lib.root.join("portre.heic")).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(videre::heif_rotate::read(&after).unwrap().angle, 2);
    // Clockwise on a 2048-high canvas: x' = 2048 - y - h, y' = x, sides swap.
    let bbox: String = lib
        .conn()
        .query_row("SELECT bbox FROM faces WHERE hash = ?1", [&portrait], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(bbox, "1448,100,400,300");

    let (status, _) = server.send("POST", &format!("/api/files/{plain}/rotate"), "");
    assert_eq!(status, 415);
    assert_eq!(
        std::fs::read(lib.root.join("irotsuz.heic")).unwrap(),
        plain_before
    );

    // A rescan finds the same photo: the content key survived the turn.
    drop(server);
    lib.scan();
    let keys: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM file_hashes WHERE hash = ?1",
            [&portrait],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(keys, 1);
}

/// A rotation keeps the content key but changes the pixels, so the stored
/// fingerprint and pixel signature describe a picture that no longer exists:
/// both go, and the next `embed` fingerprints the upright pixels again.
#[test]
fn a_rotation_forgets_the_fingerprint_and_the_pixel_signature() {
    let lib = TestLibrary::new();
    lib.copy_fixture("ai-generated-couple.jpg", "çiçek.jpg");
    lib.scan();
    let conn = lib.conn();
    let hash: String = conn
        .query_row("SELECT hash FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    conn.execute("UPDATE file_hashes SET phash = 42 WHERE hash = ?1", [&hash])
        .unwrap();
    videre_core::pixel_signatures::put(
        &conn,
        &hash,
        &videre_core::image_decode::PixelSignature {
            luma: vec![0; 16],
            width: 1,
            height: 1,
        },
    )
    .unwrap();
    drop(conn);
    let server = Server::start(&lib);

    let (status, body) = server.send("POST", &format!("/api/files/{hash}/rotate"), "");
    assert_eq!(status, 200, "{body}");
    drop(server);

    let conn = lib.conn();
    let phash: Option<i64> = conn
        .query_row(
            "SELECT phash FROM file_hashes WHERE hash = ?1",
            [&hash],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(phash, None);
    assert!(
        videre_core::pixel_signatures::get_many(&conn, std::slice::from_ref(&hash))
            .unwrap()
            .is_empty()
    );
}

/// A rotated HEIC must never be served from a conversion cached before the
/// turn: its content key is unchanged, so every `<hash>_` file would still
/// match. The user never clears a cache by hand.
#[cfg(target_os = "macos")]
#[test]
fn a_rotated_heic_is_rendered_again_not_served_from_cache() {
    let (lib, portrait, _) = heic_library();
    let thumbs = lib.context().cache.thumbnails;
    let server = Server::start(&lib);
    let dims = |bytes: &[u8]| {
        let img = image::load_from_memory(bytes).unwrap();
        (img.width(), img.height())
    };

    let (status, _, first) = server.get_bytes(&format!("/api/files/{portrait}/raw?size=240"));
    assert_eq!(status, 200);
    let (w, h) = dims(&first);
    assert!(h > w, "displays portrait before the turn: {w}x{h}");
    // What `watch --heic` would have left, plus the other names the sweep owns.
    std::fs::create_dir_all(&thumbs).unwrap();
    for name in ["240.jpg", "original.jpg", "face-1.jpg"] {
        std::fs::write(thumbs.join(format!("{portrait}_{name}")), &first).unwrap();
    }

    let (status, body) = server.send("POST", &format!("/api/files/{portrait}/rotate"), "");
    assert_eq!(status, 200, "{body}");
    let left: Vec<_> = std::fs::read_dir(&thumbs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&format!("{portrait}_")))
        .collect();
    assert!(left.is_empty(), "stale conversions left: {left:?}");

    let (status, _, second) = server.get_bytes(&format!("/api/files/{portrait}/raw?size=240"));
    assert_eq!(status, 200);
    let (w, h) = dims(&second);
    assert!(w > h, "re-rendered landscape after the turn: {w}x{h}");
}

/// Three files, two tagged: `deniz.jpg` (deniz), `plaj.jpg` (plaj), `ev.jpg`.
fn tagged_library() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    for (name, hash, date) in [
        ("deniz.jpg", "h1", "2023-07-01T10:00:00"),
        ("plaj.jpg", "h2", "2023-08-01T10:00:00"),
        ("ev.jpg", "h3", "2024-01-01T10:00:00"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, modified_at, exif_date, ext, mime)
             VALUES (?1, ?2, 100, ?3, ?3, 'jpg', 'image/jpeg')",
            rusqlite::params![root.join(name).to_string_lossy().as_ref(), hash, date],
        )
        .unwrap();
    }
    videre_core::tags::ensure_photo_tags_table(&conn).unwrap();
    conn.execute_batch("INSERT INTO photo_tags VALUES ('h1','deniz'), ('h2','plaj');")
        .unwrap();
    lib
}

fn files_for(server: &Server, q: &str) -> (u16, serde_json::Value) {
    let path = format!(
        "/api/files?view=all&limit=50&q={}",
        q.bytes().map(|b| format!("%{b:02X}")).collect::<String>()
    );
    let (status, body) = server.get(&path);
    (
        status,
        serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
    )
}

fn names(v: &serde_json::Value) -> Vec<String> {
    let mut n: Vec<String> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            f["path"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    n.sort();
    n
}

#[test]
fn the_library_grid_narrows_by_a_query() {
    let lib = tagged_library();
    let server = Server::start(&lib);

    let (status, v) = files_for(&server, "tag:deniz OR tag:plaj");
    assert_eq!(status, 200, "{v}");
    assert_eq!(names(&v), ["deniz.jpg", "plaj.jpg"]);
    assert_eq!(v["total"], 2, "{v}");
    assert_eq!(v["library_total"], 3, "N of M needs M: {v}");

    let (_, v) = files_for(&server, "-tag:deniz date:2023");
    assert_eq!(names(&v), ["plaj.jpg"]);
}

#[test]
fn a_query_s_words_are_returned_for_ranking_and_do_not_filter() {
    let lib = tagged_library();
    let server = Server::start(&lib);
    let (status, v) = files_for(&server, "gün batımı tag:deniz");
    assert_eq!(status, 200, "{v}");
    assert_eq!(names(&v), ["deniz.jpg"]);
    assert_eq!(v["text"], "gün batımı");
    let (_, v) = files_for(&server, "gün batımı");
    assert_eq!(v["total"], 3, "words alone leave the grid whole: {v}");
}

#[test]
fn a_bad_query_is_a_400_that_says_what_and_where() {
    let lib = tagged_library();
    let server = Server::start(&lib);
    let (status, v) = files_for(&server, "kişi:özgür");
    assert_eq!(status, 400);
    assert!(v["error"].as_str().unwrap().contains("unknown key"), "{v}");
    let (status, v) = files_for(&server, "(tag:deniz");
    assert_eq!(status, 400);
    assert_eq!(v["at"], 10, "{v}");
}

#[test]
fn a_bad_search_query_is_a_400_too_not_a_server_error() {
    let lib = tagged_library();
    let server = Server::start(&lib);
    let (status, body) = server.get("/api/search?q=%28deniz");
    assert_eq!(status, 400, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["at"], 6, "{v}");
}

#[test]
fn the_nav_box_offers_search_and_filter_in_well_formed_markup() {
    let lib = tagged_library();
    let server = Server::start(&lib);
    let (status, body) = server.get("/");
    assert_eq!(status, 200);
    let start = body
        .find("id=\"nav-search\"")
        .expect("the nav box is on the page");
    let tag_start = body[..start].rfind('<').unwrap();
    let tag = &body[tag_start..tag_start + body[tag_start..].find('>').unwrap() + 1];
    assert_eq!(tag.matches("placeholder=").count(), 1, "{tag}");
    assert_eq!(tag.matches("aria-label=").count(), 1, "{tag}");
    assert!(
        tag.contains("placeholder=\"Search or filter&hellip;\""),
        "{tag}"
    );
    assert!(
        tag.contains("aria-label=\"Search or filter photos\""),
        "{tag}"
    );
}

#[test]
fn the_query_box_gets_suggestions_for_the_term_at_the_cursor() {
    let lib = tagged_library();
    let server = Server::start(&lib);
    let get = |q: &str, cursor: Option<usize>| {
        let q: String = q.bytes().map(|b| format!("%{b:02X}")).collect();
        let cursor = cursor.map(|c| format!("&cursor={c}")).unwrap_or_default();
        let (status, body) = server.get(&format!("/api/query/suggest?q={q}{cursor}"));
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<serde_json::Value>(&body).unwrap()
    };

    let v = get("-tag:d", None);
    assert_eq!(v["start"], 1, "{v}");
    assert_eq!(v["items"][0]["insert"], "tag:deniz", "{v}");
    assert_eq!(v["items"][0]["kind"], "value", "{v}");
    assert_eq!(v["items"][0]["count"], 1, "{v}");

    // The cursor, not the end, picks the term: after "ta", before " date:".
    let v = get("ta date:2023", Some(2));
    assert_eq!(v["items"][0]["insert"], "tag:", "{v}");
    assert_eq!(v["items"][0]["kind"], "key", "{v}");

    assert_eq!(get("\"gün bat", None)["items"], serde_json::json!([]));
}

#[test]
fn the_date_tree_counts_only_files_matching_a_query() {
    let lib = tagged_library();
    let server = Server::start(&lib);
    let dates = |q: &str| {
        let q: String = q.bytes().map(|b| format!("%{b:02X}")).collect();
        let (status, body) = server.get(&format!("/api/dates?level=year&q={q}"));
        (
            status,
            serde_json::from_str::<serde_json::Value>(&body).unwrap_or_default(),
        )
    };
    let years = |v: &serde_json::Value| -> Vec<(String, i64)> {
        v["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["key"].as_str().unwrap().to_string(),
                    b["count"].as_i64().unwrap(),
                )
            })
            .collect()
    };

    let (status, v) = dates("tag:deniz");
    assert_eq!(status, 200, "{v}");
    // An empty year is not shown at all, rather than as 0.
    assert_eq!(years(&v), [("2023".to_string(), 1)]);
    assert_eq!(v["matched"], 1, "{v}");
    assert_eq!(v["library_total"], 3, "{v}");

    let (_, v) = dates("-tag:deniz");
    assert_eq!(
        years(&v),
        [("2024".to_string(), 1), ("2023".to_string(), 1)]
    );

    let (status, v) = dates("kişi:özgür");
    assert_eq!(status, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("unknown key"), "{v}");

    // Without q, the response is as it always was.
    let (_, body) = server.get("/api/dates?level=year");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v.get("library_total").is_none(), "{v}");
}

/// Two duplicate groups: `deniz` (tagged deniz) and `ev`, each on two paths.
fn duplicates_library() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    for (name, hash) in [
        ("deniz-1.jpg", "hd"),
        ("deniz-2.jpg", "hd"),
        ("ev-1.jpg", "he"),
        ("ev-2.jpg", "he"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext, mime)
             VALUES (?1, ?2, 100, 'jpg', 'image/jpeg')",
            rusqlite::params![root.join(name).to_string_lossy().as_ref(), hash],
        )
        .unwrap();
    }
    videre_core::tags::ensure_photo_tags_table(&conn).unwrap();
    conn.execute_batch("INSERT INTO photo_tags VALUES ('hd','deniz');")
        .unwrap();
    lib
}

#[test]
fn duplicates_shows_only_groups_with_a_matching_member_whole() {
    let lib = duplicates_library();
    let server = Server::start(&lib);

    let (status, body) = server.get("/duplicates?q=tag%3Adeniz");
    assert_eq!(status, 200);
    assert!(body.contains("deniz-1.jpg") && body.contains("deniz-2.jpg"));
    assert!(
        !body.contains("ev-1.jpg"),
        "a group with no match is left out"
    );
    assert!(
        body.contains("var GQUERY_RESULT={\"matched\":1,\"library_total\":2};"),
        "the page says how many groups match"
    );

    // A query that cannot run shows no groups and says why.
    let (status, body) = server.get("/duplicates?q=ki%C5%9Fi%3Ax");
    assert_eq!(status, 200);
    assert!(!body.contains("deniz-1.jpg"));
    assert!(
        body.contains("var GQUERY_RESULT={\"error\":"),
        "{}",
        &body[..200]
    );

    let (_, body) = server.get("/duplicates");
    assert!(body.contains("ev-1.jpg") && body.contains("var GQUERY_RESULT=null;"));
}

#[test]
fn people_lists_only_those_in_matching_files() {
    let lib = tagged_library();
    let server = Server::start(&lib);
    lib.conn()
        .execute_batch(
            "INSERT INTO people (name, full_name) VALUES ('ozgur', 'Özgür'), ('ayse', 'Ayşe');
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) VALUES
               ('h1', '0,0,9,9', X'00', 'ozgur', 1),
               ('h2', '0,0,9,9', X'00', 'ayse', 1);",
        )
        .unwrap();

    let (status, body) = server.get("/api/faces?q=tag%3Adeniz");
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let people: Vec<&str> = v["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["label"].as_str().unwrap())
        .collect();
    assert_eq!(people, ["ozgur"]);
    assert_eq!(v["matched"], 1, "{v}");
    assert_eq!(v["library_total"], 3, "{v}");

    let (status, _) = server.get("/api/faces?q=ki%C5%9Fi%3Ax");
    assert_eq!(status, 400);

    let (_, body) = server.get("/api/faces");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["people"].as_array().unwrap().len(), 2);
    assert!(v.get("matched").is_none());
}

#[test]
fn similar_ranks_within_a_query_s_filters() {
    let lib = search_fixture();
    {
        let conn = lib.conn();
        videre_core::tags::ensure_photo_tags_table(&conn).unwrap();
        conn.execute_batch("INSERT INTO photo_tags VALUES ('bbbb','deniz'), ('dddd','deniz');")
            .unwrap();
    }
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/search?like=aaaa&q=tag%3Adeniz&limit=10");
    assert_eq!(status, 200, "{body}");
    let hashes: Vec<&str> = body
        .split("\"hash\":\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    assert_eq!(hashes, ["bbbb", "dddd"], "{body}");

    // Words rank too, so they still cannot come with an example.
    let (status, _) = server.get("/api/search?like=aaaa&q=kedi%20tag%3Adeniz");
    assert_eq!(status, 400);
    let (status, _) = server.get("/api/search?like=aaaa&q=ki%C5%9Fi%3Ax");
    assert_eq!(status, 400);
}

#[test]
fn a_search_has_a_page_of_its_own() {
    let lib = search_fixture();
    let server = Server::start(&lib);
    let (status, body) = server.get("/search?like=aaaa");
    assert_eq!(status, 200);
    assert!(
        body.contains("id=\"gallery\""),
        "the page has the grid to rank into"
    );
}

// ---- capture dates: a Takeout photo lands on the day it was taken ---------

/// An undated photo whose Google Takeout sidecar says when it was taken is
/// filed under that day, not under the day the export was unpacked (its
/// mtime), in the date tree and the date page alike, and the page carries
/// the capture date for the browser to show.
#[test]
fn a_takeout_photo_is_filed_under_its_sidecar_day() {
    let lib = TestLibrary::new();
    let photo = lib
        .context()
        .paths
        .root
        .join("Fotoğraflar/IMG-20160104-WA0000.jpg");
    std::fs::create_dir_all(photo.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(16, 16, image::Rgb([10, 90, 200]))
        .save(&photo)
        .unwrap();
    // Noon UTC on 4 Jan 2016: the same calendar day in every zone from
    // UTC-11 to UTC+11, so the test does not depend on the machine's zone.
    let taken: i64 = 1_451_908_800;
    std::fs::write(
        photo.with_file_name("IMG-20160104-WA0000.jpg.supplemental-metadata.json"),
        format!(r#"{{"photoTakenTime":{{"timestamp":"{taken}"}}}}"#),
    )
    .unwrap();
    // Unpacked years later: the mtime says 2026.
    filetime::set_file_mtime(&photo, filetime::FileTime::from_unix_time(1_789_727_404, 0)).unwrap();
    lib.scan();
    let server = Server::start(&lib);

    let (status, body) = server.get("/api/dates?level=year");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let years: Vec<&str> = v["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["key"].as_str().unwrap())
        .collect();
    assert_eq!(years, ["2016"], "{body}");

    let (status, body) = server.get("/api/files?view=date&date=2016-01-04&limit=10");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let files = v["files"].as_array().expect("files");
    assert_eq!(files.len(), 1, "{body}");
    assert!(
        files[0]["ca"].as_str().unwrap().starts_with("2016-01-04T"),
        "{body}"
    );
}
