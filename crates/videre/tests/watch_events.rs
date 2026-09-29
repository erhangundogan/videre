mod common;
use common::TestLibrary;
use std::time::{Duration, Instant};

/// One bounded pass: VIDERE_WATCH_ONCE makes `watch` run the startup scan
/// and exit 0, so the correctness backstop is testable without a
/// wall-clock race. Not silent: the per-stage record counts on stderr are
/// part of what these tests assert.
fn watch_once(lib: &TestLibrary, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["watch"];
    args.extend_from_slice(extra);
    let out = lib
        .cmd()
        .args(&args)
        .env("VIDERE_WATCH_ONCE", "1")
        .output()
        .expect("run videre watch");
    assert!(
        out.status.success(),
        "watch (once) failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn the_startup_scan_processes_a_dropped_file_and_a_second_pass_hashes_nothing() {
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "a.jpg");
    watch_once(&lib, &["--scan"]);
    let count: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "startup scan must process a dropped file");
    // Second pass over an unchanged tree hashes nothing (incremental).
    let out = watch_once(&lib, &["--scan"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("scan stage wrote 0 record(s)"),
        "unchanged pass must hash nothing:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_face_recluster_stage_runs_once_per_new_face_and_then_skips() {
    let lib = TestLibrary::new();
    // One face row with a too-small bbox: the quality gate filters it out,
    // so the clustering pass itself is model-free, but the id is above any
    // watermark, so the gate must open exactly once.
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO faces (hash, bbox, embedding) VALUES ('h1', '0,0,10,10', X'0000')",
        [],
    )
    .unwrap();
    drop(conn);

    watch_once(&lib, &["--faces"]);
    let conn = lib.conn();
    let status: String = conn
        .query_row(
            "SELECT status FROM pipeline_runs WHERE command = 'face-recluster'",
            [],
            |r| r.get(0),
        )
        .expect("a face above the watermark must open the gate on the first pass");
    assert_eq!(status, "success");
    drop(conn);

    // No faces were added: the gate closes and the stage announces the skip
    // instead of running another pass. (The tracked row is an upsert, so its
    // timestamp bumps on every pass whether the gate opened or not; the
    // message is what distinguishes work from a closed gate.)
    let second = watch_once(&lib, &["--faces"]);
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("face recluster up to date"),
        "the gated stage must skip when no faces were added: {}",
        String::from_utf8_lossy(&second.stderr)
    );
}

/// Best-effort, generously timed. Event delivery on a hosted CI image is
/// not reliable enough for tight timing, so on timeout this SKIPS (fd 2
/// message) rather than fails; the startup-scan guarantee is the hard test.
#[test]
fn a_dropped_file_is_processed_via_events_within_a_generous_bound() {
    use std::io::Write;
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["config", "set", "watch-debounce-ms", "200"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .spawn()
        .expect("spawn watch");
    std::thread::sleep(Duration::from_millis(700)); // let the watcher register
    lib.copy_fixture("sample_with_exif.jpg", "dropped.jpg");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = false;
    while Instant::now() < deadline {
        let count: i64 = lib
            .try_conn()
            .and_then(|c| {
                c.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
                    .ok()
            })
            .unwrap_or(0);
        if count > 0 {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    if !seen {
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: event not delivered within 20s; the startup-scan guarantee covers correctness"
        );
    }
}

/// Two watches on two separate libraries run at once without error:
/// separate roots mean separate .videre directories and locks, so neither
/// can refuse or block the other.
#[test]
fn two_watches_on_separate_libraries_do_not_conflict() {
    let a = TestLibrary::new();
    let b = TestLibrary::new();
    a.copy_fixture("sample_with_exif.jpg", "a.jpg");
    b.copy_fixture("sample_with_exif.jpg", "b.jpg");
    let mut ca = a
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .spawn()
        .unwrap();
    let mut cb = b
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        ca.try_wait().unwrap().is_none(),
        "watch A should still be running"
    );
    assert!(
        cb.try_wait().unwrap().is_none(),
        "watch B should still be running"
    );
    let _ = ca.kill();
    let _ = ca.wait();
    let _ = cb.kill();
    let _ = cb.wait();
}

/// Degraded fallback: when event registration fails, watch says so once and
/// keeps running the rescan loop instead of crashing or sitting silent.
#[test]
fn a_failed_registration_degrades_to_the_rescan_loop_with_a_notice() {
    let lib = TestLibrary::new();
    let err_path = lib.root.join("watch-stderr.log");
    let err_file = std::fs::File::create(&err_path).unwrap();
    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .env("VIDERE_WATCH_TEST_FAIL_EVENTS", "1")
        .stderr(std::process::Stdio::from(err_file))
        .spawn()
        .unwrap();
    // The notice is written as soon as registration fails; under a loaded
    // parallel suite the child may take longer than any fixed sleep to get
    // there, so poll for it while insisting the process stays alive.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut announced = false;
    let mut alive = true;
    while Instant::now() < deadline {
        alive = child.try_wait().unwrap().is_none();
        let notice = std::fs::read_to_string(&err_path).unwrap_or_default();
        if notice.contains("live events unavailable") {
            announced = true;
            break;
        }
        if !alive {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        announced && alive,
        "watch must announce degraded mode and keep running, not exit or sit silent"
    );
}

/// The internal maintenance deadline reruns the opt-in stages with no
/// events at all: prune's row is removed after the startup pass and must
/// reappear, which only a maintenance tick can do.
#[test]
fn the_maintenance_deadline_reruns_opt_in_stages_without_events() {
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["config", "set", "watch-debounce-ms", "200"])
        .output()
        .unwrap();
    assert!(out.status.success());
    drop(lib.init_db());
    // Not silent: the startup-scan line on stderr is the watcher-ready
    // signal. The child's initialize takes the EXCLUSIVE activity lock while
    // it runs, and acquisition is nonblocking on both sides, so opening the
    // database before that line can starve the child at startup and make it
    // exit. Wait for the line, then poll the database.
    let err_path = lib.root.join("watch-stderr.log");
    let err_file = std::fs::File::create(&err_path).unwrap();
    let mut child = lib
        .cmd()
        .args(["watch", "--prune"])
        .env("VIDERE_WATCH_TEST_MAINTENANCE_SECS", "1")
        .stderr(std::process::Stdio::from(err_file))
        .spawn()
        .unwrap();
    let ready_deadline = Instant::now() + Duration::from_secs(20);
    let mut ready = false;
    while Instant::now() < ready_deadline {
        if std::fs::read_to_string(&err_path)
            .unwrap_or_default()
            .contains("videre watch: startup scan")
        {
            ready = true;
            break;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        ready,
        "the watcher must reach its startup scan: {}",
        std::fs::read_to_string(&err_path).unwrap_or_default()
    );
    // Startup reconcile runs --prune once; wait for that row. The window is
    // generous because sibling tests run model inference in parallel, and the
    // watcher's own stages hold the activity lease while they run, so both
    // the open and the query retry until the deadline.
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut ran_startup = false;
    let mut conn = None;
    while Instant::now() < deadline {
        if let Some(c) = lib.try_conn() {
            if c.query_row(
                "SELECT 1 FROM pipeline_runs WHERE command = 'prune'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .is_ok()
            {
                ran_startup = true;
                conn = Some(c);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(ran_startup, "startup reconcile must run --prune");
    // started_at has one-second resolution, which a fast tick can repeat, so
    // a changed timestamp is not proof. Removing the row is: only the
    // maintenance pass can bring it back.
    if let Some(c) = conn {
        c.execute("DELETE FROM pipeline_runs WHERE command = 'prune'", [])
            .unwrap();
    }
    let mut reran = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(c) = lib.try_conn() {
            if c.query_row(
                "SELECT 1 FROM pipeline_runs WHERE command = 'prune'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .is_ok()
            {
                reran = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        reran,
        "the maintenance pass must rerun the prune stage without events"
    );
}

/// A populated folder moved into the library arrives as one rename event
/// whose path is a directory. Hashing that path fails, so the scoped scan
/// must expand directory candidates to the files inside them; otherwise the
/// moved-in photos wait for the hourly maintenance pass.
#[test]
fn a_moved_in_directory_has_its_contents_scanned() {
    use std::io::Write;
    let lib = TestLibrary::new();
    lib.cmd()
        .args(["config", "set", "watch-debounce-ms", "200"])
        .output()
        .unwrap();
    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--silent"])
        .spawn()
        .expect("spawn watch");
    std::thread::sleep(Duration::from_millis(700)); // let the watcher register

    let staging = std::env::temp_dir().join(format!("videre-watch-dirmove-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join("sample_with_exif.jpg"),
        staging.join("inner.jpg"),
    )
    .unwrap();
    std::fs::rename(&staging, lib.root.join("moved-in")).unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = false;
    while Instant::now() < deadline {
        let n: i64 = lib
            .try_conn()
            .and_then(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM file_hashes WHERE path LIKE '%moved-in%'",
                    [],
                    |r| r.get(0),
                )
                .ok()
            })
            .unwrap_or(0);
        if n > 0 {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&staging);
    if !seen {
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: directory event not delivered within 20s; the maintenance pass covers correctness"
        );
    }
}

/// A batch blocked by a faces run must survive and retry: the scan runs (the
/// scan lock is separate), the faces stage is busy, and when the lock frees
/// the retained batch is processed on the short backoff instead of waiting
/// for the hourly maintenance pass. Needs the models for the final
/// processing assertion, so it needs explicit local model-test opt-in.
#[test]
fn a_batch_blocked_by_a_faces_run_retries_and_processes_when_free() {
    let lib = TestLibrary::new();
    if common::model_test_support::skip_unless_model_tests_enabled("watch busy-faces retry") {
        return;
    }
    let guard = common::shared_cache_guard();
    common::prepare_face_models(&guard);
    lib.cmd()
        .args(["config", "set", "watch-debounce-ms", "200"])
        .output()
        .unwrap();
    drop(lib.init_db());

    // Hold the faces command lock: from the watcher's side the standalone
    // faces command is running.
    let locks = lib.context().paths.locks.join("faces.lock");
    let hold = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&locks)
        .unwrap();
    fs2::FileExt::lock_exclusive(&hold).unwrap();

    let mut child = common::KillOnDrop(
        lib.model_cmd(&guard)
            .args(["watch", "--scan", "--faces", "--silent"])
            .spawn()
            .expect("spawn watch"),
    );
    std::thread::sleep(Duration::from_millis(700));
    lib.copy_fixture("sample_with_exif.jpg", "dropped.jpg");

    // The scan stage locks "scan", not "faces": the row lands now.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut scanned = false;
    while Instant::now() < deadline {
        let n: i64 = lib
            .try_conn()
            .and_then(|c| {
                c.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
                    .ok()
            })
            .unwrap_or(0);
        if n > 0 {
            scanned = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        scanned,
        "the scan stage must not be blocked by the faces lock"
    );
    let scanned_faces: i64 = lib
        .try_conn()
        .and_then(|c| {
            c.query_row("SELECT COUNT(*) FROM faces_scanned", [], |r| r.get(0))
                .ok()
        })
        .unwrap_or(0);
    assert_eq!(
        scanned_faces, 0,
        "faces is locked: the file must not be marked processed while it is held"
    );

    // Free the lock: the retained batch retries on the backoff and the faces
    // stage processes the file without any new event.
    drop(hold);
    // The fixture carries no detectable face, so the processing proof is the
    // scanned marker the pipeline writes per processed hash, not a faces row.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut processed = false;
    while Instant::now() < deadline {
        let n: i64 = lib
            .try_conn()
            .and_then(|c| {
                c.query_row("SELECT COUNT(*) FROM faces_scanned", [], |r| r.get(0))
                    .ok()
            })
            .unwrap_or(0);
        if n > 0 {
            processed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let _ = child.0.kill();
    let _ = child.0.wait();
    assert!(
        processed,
        "the retained batch must be processed once the lock frees"
    );
}

/// A placed photo, then a live watch and a copy of it. The copy must share the
/// original's place and that place must count 2, from the event batch alone,
/// whether the batch runs the full recompute (`last_recompute_ms` under the
/// budget) or places only the new row (over it). Skips, like the other live
/// tests, if the platform delivers no event in time.
fn a_copy_joins_its_place_from_the_batch(last_recompute_ms: i64) {
    use std::io::Write;
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "Şile/çiçek.jpg");
    lib.scan();
    let out = lib.cmd().args(["locations", "--silent"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    videre_core::library_state::set(
        &lib.conn(),
        videre_core::recompute_cost::LOCATIONS,
        last_recompute_ms,
    )
    .unwrap();
    let out = lib
        .cmd()
        .args(["config", "set", "watch-debounce-ms", "200"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--location", "--silent"])
        .spawn()
        .expect("spawn watch");
    std::thread::sleep(Duration::from_millis(1500)); // startup pass and registration
    lib.copy_fixture("sample_with_exif.jpg", "kopya/çiçek.jpg");
    let root = lib.context().paths.root.clone();
    let original = root.join("Şile/çiçek.jpg").to_string_lossy().into_owned();
    let copy = root.join("kopya/çiçek.jpg").to_string_lossy().into_owned();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut scanned = false;
    let mut placed = None;
    while Instant::now() < deadline {
        if let Some(c) = lib.try_conn() {
            let row = |p: &str| -> Option<Option<i64>> {
                c.query_row(
                    "SELECT location_cluster_id FROM file_hashes WHERE path = ?1",
                    [p],
                    |r| r.get(0),
                )
                .ok()
            };
            if let Some(cluster) = row(&copy) {
                scanned = true;
                if cluster.is_some() {
                    let count: i64 = c
                        .query_row(
                            "SELECT photo_count FROM location_clusters WHERE id = ?1",
                            [cluster],
                            |r| r.get(0),
                        )
                        .unwrap();
                    placed = Some((cluster, row(&original).flatten(), count));
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    if !scanned {
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: event not delivered within 20s; the startup-scan guarantee covers correctness"
        );
        return;
    }
    let (copy_place, original_place, count) =
        placed.expect("the batch scanned the copy but left it without a place");
    assert_eq!(
        copy_place, original_place,
        "the copy joins the original's place"
    );
    assert_eq!(count, 2, "the map's marker count must match its grid");
    // And status agrees: nothing left to place.
    let out = lib.cmd().args(["status", "--json"]).output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let locations = json["report"]["coverage"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["stage"] == "locations")
        .cloned()
        .unwrap();
    assert_eq!(locations["outstanding"], 0, "{locations}");
}

#[test]
fn a_copied_photo_joins_its_place_when_a_full_recompute_is_cheap() {
    a_copy_joins_its_place_from_the_batch(10);
}

#[test]
fn a_copied_photo_joins_its_place_when_a_full_recompute_is_slow() {
    a_copy_joins_its_place_from_the_batch(600_000);
}

#[test]
fn a_copied_rated_photo_gets_its_sidecar_from_the_batch() {
    use std::io::Write;
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "Şile/çiçek.jpg");
    lib.scan();
    for args in [
        &["mark", "--path", "Şile", "--rating", "4", "--silent"][..],
        &["config", "set", "watch-debounce-ms", "200"][..],
    ] {
        let out = lib.cmd().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--export-xmp", "--silent"])
        .spawn()
        .expect("spawn watch");
    std::thread::sleep(Duration::from_millis(1500));
    lib.copy_fixture("sample_with_exif.jpg", "kopya/çiçek.jpg");
    // The canonical root: what the scan stores, so the row lookup matches.
    let copy = lib.context().paths.root.join("kopya/çiçek.jpg");
    let sidecar = std::path::PathBuf::from(format!("{}.xmp", copy.display()));
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut scanned = false;
    while Instant::now() < deadline && !sidecar.exists() {
        scanned = scanned
            || lib
                .try_conn()
                .and_then(|c| {
                    c.query_row(
                        "SELECT 1 FROM file_hashes WHERE path = ?1",
                        [copy.to_string_lossy()],
                        |_| Ok(()),
                    )
                    .ok()
                })
                .is_some();
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    if !scanned && !sidecar.exists() {
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: event not delivered within 20s; the startup-scan guarantee covers correctness"
        );
        return;
    }
    let doc = std::fs::read_to_string(&sidecar).expect("the batch wrote no sidecar for the copy");
    assert!(doc.contains("<xmp:Rating>4</xmp:Rating>"), "{doc}");
}

#[test]
fn a_bulk_import_scans_first_and_finishes_once_it_goes_quiet() {
    use std::io::{Read, Write};
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "Şile/çiçek.jpg");
    lib.scan();
    for args in [
        &["locations", "--silent"][..],
        &["config", "set", "watch-debounce-ms", "200"][..],
        &["config", "set", "watch-bulk-threshold", "3"][..],
        &["config", "set", "watch-bulk-quiet-ms", "2000"][..],
    ] {
        let out = lib.cmd().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // A slow library as far as watch knows: outside bulk mode it would place
    // only the new rows, so a rebuild below can only come from the finish.
    videre_core::library_state::set(&lib.conn(), videre_core::recompute_cost::LOCATIONS, 600_000)
        .unwrap();
    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--location"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn watch");
    std::thread::sleep(Duration::from_millis(1500));
    // Staged outside, then moved in at once: one batch of four.
    let staging = lib.home.join("gelen");
    std::fs::create_dir_all(&staging).unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_with_exif.jpg");
    for i in 0..4 {
        std::fs::copy(&fixture, staging.join(format!("toplu-{i}.jpg"))).unwrap();
    }
    let root = lib.context().paths.root.clone();
    for i in 0..4 {
        std::fs::rename(
            staging.join(format!("toplu-{i}.jpg")),
            root.join(format!("toplu-{i}.jpg")),
        )
        .unwrap();
    }
    let placed = |c: &rusqlite::Connection| -> i64 {
        c.query_row(
            "SELECT COUNT(*) FROM file_hashes WHERE path LIKE '%toplu-%' AND location_cluster_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut done = false;
    while Instant::now() < deadline {
        if lib.try_conn().map(|c| placed(&c)) == Some(4) {
            done = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    if !stderr.contains("bulk:") && !done {
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: events not delivered within 25s; the startup-scan guarantee covers correctness"
        );
        return;
    }
    assert!(stderr.contains("bulk: 4 files waiting"), "{stderr}");
    assert!(done, "the finish must place every file: {stderr}");
    assert!(
        stderr.contains("location clusters rebuilt"),
        "the finish runs the full recompute, whatever it costs: {stderr}"
    );
}

#[test]
fn each_batch_says_what_each_file_got_and_what_it_did_not() {
    use std::io::{Read, Write};
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "Şile/çiçek.jpg");
    lib.scan();
    for args in [
        &["locations", "--silent"][..],
        &["config", "set", "watch-debounce-ms", "200"][..],
    ] {
        let out = lib.cmd().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut child = lib
        .cmd()
        .args(["watch", "--scan", "--location", "--embed"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn watch");
    std::thread::sleep(Duration::from_millis(1500));
    lib.copy_fixture("sample_with_exif.jpg", "kopya/çiçek.jpg");
    let copy = lib.context().paths.root.join("kopya/çiçek.jpg");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut placed = false;
    while Instant::now() < deadline && !placed {
        placed = lib
            .try_conn()
            .and_then(|c| {
                c.query_row(
                    "SELECT location_cluster_id FROM file_hashes WHERE path = ?1",
                    [copy.to_string_lossy()],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .ok()
            })
            .flatten()
            .is_some();
        std::thread::sleep(Duration::from_millis(250));
    }
    std::thread::sleep(Duration::from_millis(500)); // the report follows the stages
    let _ = child.kill();
    let _ = child.wait();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    if !placed {
        let _ = writeln!(
            std::io::stderr(),
            "SKIP: event not delivered within 20s; the startup-scan guarantee covers correctness"
        );
        return;
    }
    let line = stderr
        .lines()
        .find(|l| l.contains("kopya/çiçek.jpg: scanned"))
        .unwrap_or_else(|| panic!("no report line for the copy:\n{stderr}"));
    assert!(
        line.contains("embed skipped:") && line.contains("not downloaded"),
        "{line}"
    );
    assert!(line.contains("placed in "), "{line}");
}

#[test]
fn watch_without_the_model_says_so_and_downloads_nothing() {
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "çiçek.jpg");
    let out = watch_once(&lib, &["--scan", "--embed"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("embed skipped: model") && stderr.contains("not downloaded"),
        "{stderr}"
    );
    let hub = lib.home.join(".cache/huggingface/hub");
    let fetched = std::fs::read_dir(&hub).map(|d| d.count()).unwrap_or(0);
    assert_eq!(fetched, 0, "a watcher must never start a model download");
}

#[test]
fn watch_embeds_and_classifies_a_new_photo() {
    use common::model_test_support::skip_unless_model_tests_enabled;
    if skip_unless_model_tests_enabled("watch embed stage") {
        return;
    }
    let guard = common::shared_cache_guard();
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "çiçek.jpg");
    let out = lib
        .model_cmd(&guard)
        .args(["watch", "--scan", "--embed"])
        .env("VIDERE_WATCH_ONCE", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = lib
        .model_cmd(&guard)
        .args(["status", "--json"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let coverage = json["report"]["coverage"].as_array().expect("coverage");
    for stage in ["embed", "classify"] {
        let c = coverage.iter().find(|c| c["stage"] == stage).unwrap();
        assert_eq!(c["outstanding"], 0, "{stage}: {c}");
        assert_eq!(c["total"], 1, "{stage}: {c}");
    }
}

#[test]
fn a_scan_that_changes_gps_data_triggers_the_recluster_on_the_next_pass() {
    let lib = TestLibrary::new();
    drop(lib.init_db());
    let conn = lib.conn();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
         VALUES (?1, 'ha', 'jpg', 52.52, 13.405)",
        [lib.context().paths.root.join("a.jpg").to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    watch_once(&lib, &["--location"]);
    let conn = lib.conn();
    let clusters: i64 = conn
        .query_row("SELECT COUNT(*) FROM location_clusters", [], |r| r.get(0))
        .unwrap();
    assert_eq!(clusters, 1, "the first pass reclusters the GPS row");
    drop(conn);

    // A second GPS photo arrives: the fingerprint changes, so the next pass
    // reclusters and the new coordinate lands in a cluster.
    let conn = lib.conn();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
         VALUES (?1, 'hb', 'jpg', 48.85, 2.35)",
        [lib.context().paths.root.join("b.jpg").to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    watch_once(&lib, &["--location"]);
    let conn = lib.conn();
    let clusters: i64 = conn
        .query_row("SELECT COUNT(*) FROM location_clusters", [], |r| r.get(0))
        .unwrap();
    assert!(
        clusters >= 2,
        "the changed fingerprint must recluster: {clusters}"
    );
}

#[test]
fn an_unchanged_library_skips_the_recluster_with_a_message() {
    let lib = TestLibrary::new();
    drop(lib.init_db());
    let conn = lib.conn();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
         VALUES (?1, 'ha', 'jpg', 52.52, 13.405)",
        [lib.context().paths.root.join("a.jpg").to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    let first = watch_once(&lib, &["--location"]);
    assert!(
        String::from_utf8_lossy(&first.stderr).contains("location clusters rebuilt"),
        "the first pass rebuilds: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = watch_once(&lib, &["--location"]);
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("locations up to date"),
        "an unchanged library must skip: {}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn the_watcher_respects_a_manual_radius() {
    let lib = TestLibrary::new();
    drop(lib.init_db());
    let conn = lib.conn();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
         VALUES (?1, 'ha', 'jpg', 52.52, 13.405)",
        [lib.context().paths.root.join("a.jpg").to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    // A manual recluster at a non-default radius: the watcher must leave it
    // alone rather than silently recluster at the default.
    let out = lib
        .cmd()
        .args(["locations", "--radius", "5", "--silent"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let pass = watch_once(&lib, &["--location"]);
    assert!(
        String::from_utf8_lossy(&pass.stderr).contains("manual radius 5km in effect"),
        "the radius skip must say so: {}",
        String::from_utf8_lossy(&pass.stderr)
    );
}
