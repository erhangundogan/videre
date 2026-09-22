mod common;
use common::TestLibrary;

fn embedding_at(angle: f32) -> Vec<u8> {
    let radians = angle.to_radians();
    [radians.cos(), radians.sin()]
        .into_iter()
        .flat_map(|value| half::f16::from_f32(value).to_le_bytes())
        .collect()
}

fn seed_unlabeled_faces(lib: &TestLibrary, angles: &[f32]) {
    let conn = lib.conn();
    for (index, angle) in angles.iter().enumerate() {
        let id = index as i64 + 1;
        let hash = format!("face-{id}");
        let path = lib.context().paths.root.join(format!("{hash}.jpg"));
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext, mime) VALUES (?1, ?2, 'jpg', 'image/jpeg')",
            rusqlite::params![path.to_string_lossy(), hash],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, det_score, blur)
             VALUES (?1, ?2, '0,0,100,100', ?3, 0.9, 500.0)",
            rusqlite::params![id, hash, embedding_at(*angle)],
        )
        .unwrap();
    }
}

fn recluster(lib: &TestLibrary, tuning: &[&str]) {
    let out = lib
        .cmd()
        .args(["faces", "--recluster"])
        .args(tuning)
        .args([
            "--min-face-size",
            "0",
            "--max-generic-sim",
            "1",
            "--max-landmark-error",
            "1000",
            "--min-blur",
            "0",
            "--attach-sim",
            "1",
            "--silent",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "recluster {tuning:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn cluster_shape(lib: &TestLibrary) -> (i64, i64) {
    lib.conn()
        .query_row(
            "SELECT COUNT(cluster_id), COUNT(DISTINCT cluster_id) FROM faces",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// A library with one labeled face, one person record, a scanned marker,
/// and one image on disk.
fn seeded() -> TestLibrary {
    let lib = TestLibrary::new();
    // Real image bytes, so the post-wipe rebuild can actually detect faces.
    // The stored path uses the context root's spelling: copy_fixture returns
    // a canonicalized /private/... path whose prefix fails the library's
    // root-identity recheck.
    lib.copy_fixture("sample_with_exif.jpg", "a.jpg");
    let path = lib.context().paths.root.join("a.jpg");
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'abc123', 'jpg')",
        [path.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO faces (hash, bbox, embedding, confirmed, person_label)
         VALUES ('abc123', '0,0,50,50', X'0000', 1, 'elena')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO people (name, full_name) VALUES ('elena', 'Elena')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO faces_scanned (hash) VALUES ('abc123')", [])
        .unwrap();
    drop(conn);
    lib
}

#[test]
fn reset_without_yes_on_noninteractive_stdin_refuses_and_changes_nothing() {
    let lib = seeded();
    let out = lib
        .cmd()
        .args(["faces", "--reset"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a non-interactive reset must refuse rather than wipe silently"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
    let n: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "refusal must leave every row in place");
}

#[test]
fn reset_on_a_library_where_faces_never_ran_bails_out() {
    let lib = TestLibrary::new();
    drop(lib.init_db());
    let out = lib
        .cmd()
        .args(["faces", "--reset", "--yes"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "nothing to reset must be an error, not a silent success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nothing to reset"), "{stderr}");
}

/// The startup migrations (notably the cluster-id detach) run inside
/// `create_faces_table`, which the faces command called before any reset
/// check: a dry-run or a refusal reported "nothing was deleted" while
/// silently clearing the machine grouping of labeled faces. Consent-gated
/// means consent-gated for every write, migrations included.
#[test]
fn reset_dry_run_and_refusal_leave_machine_state_untouched() {
    let lib = seeded();
    let conn = lib.conn();
    conn.execute("UPDATE faces SET cluster_id = 42 WHERE hash = 'abc123'", [])
        .unwrap();
    drop(conn);

    let out = lib
        .cmd()
        .args(["faces", "--reset", "--dry-run", "--yes"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cid: Option<i64> = lib
        .conn()
        .query_row(
            "SELECT cluster_id FROM faces WHERE hash = 'abc123'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        cid,
        Some(42),
        "a dry-run must not run migrations that clear machine grouping"
    );

    let out = lib
        .cmd()
        .args(["faces", "--reset"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "non-interactive reset must refuse");
    let cid: Option<i64> = lib
        .conn()
        .query_row(
            "SELECT cluster_id FROM faces WHERE hash = 'abc123'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        cid,
        Some(42),
        "a refused reset must not run migrations either"
    );
}

/// Plain `faces --dry-run` promises "detect but write nothing": the
/// startup table creation runs migrations (the labeled-face cluster detach)
/// that write, so a dry-run must skip it entirely.
#[test]
fn plain_faces_dry_run_writes_nothing() {
    let lib = seeded();
    let conn = lib.conn();
    conn.execute("UPDATE faces SET cluster_id = 42 WHERE hash = 'abc123'", [])
        .unwrap();
    drop(conn);

    // No eligible files (--ext heic, the library holds only a jpg) keeps the
    // run model-free: detection never starts, which isolates the dry-run
    // contract from the detection itself.
    let out = lib
        .cmd()
        .args(["faces", "--dry-run", "--silent", "--ext", "heic"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cid: Option<i64> = lib
        .conn()
        .query_row(
            "SELECT cluster_id FROM faces WHERE hash = 'abc123'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        cid,
        Some(42),
        "a dry-run must not migrate face state, on any path"
    );
}

/// A real dry-run over eligible files must not touch pipeline state either:
/// track_in upserts the faces run row the moment detection is entered, and
/// the round-3 guard on table creation did not cover it. Needs the models
/// (detection actually runs), so it skips on a cold cache like the other
/// faces suites.
#[test]
fn plain_faces_dry_run_writes_no_pipeline_row() {
    let lib = seeded();
    if common::skip_without_models("faces dry-run tracking", common::face_models_cached()) {
        return;
    }
    // A second image whose hash is NOT in the faces skip set, so detection
    // actually starts and reaches the run tracking under test. (The seeded
    // face row marks 'abc123' as already done.)
    {
        let path = lib.context().paths.root.join("b.jpg");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join("sample_with_exif.jpg"),
            &path,
        )
        .unwrap();
        let conn = lib.conn();
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'def456', 'jpg')",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();
    }
    let out = lib
        .cmd()
        .args(["faces", "--dry-run", "--silent", "--min-cluster-size", "1"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM pipeline_runs WHERE command = 'faces'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        runs, 0,
        "a dry-run must not record a faces run in the pipeline table"
    );
}

#[test]
fn reset_with_dry_run_deletes_nothing() {
    let lib = seeded();
    let out = lib
        .cmd()
        .args(["faces", "--reset", "--dry-run", "--yes"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nothing was deleted"),
        "dry-run must say so: {stderr}"
    );
    let n: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "dry-run must leave every row in place");
}

#[test]
fn reset_conflicts_with_recluster_and_limit() {
    let lib = seeded();
    for extra in [&["--recluster"][..], &["--limit", "1"][..]] {
        let mut args = vec!["faces", "--reset", "--yes"];
        args.extend_from_slice(extra);
        let out = lib
            .cmd()
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "--reset with {extra:?} must fail to parse: a wipe followed by a \
             partial or detection-free rebuild is never what it promises"
        );
        let n: i64 = lib
            .conn()
            .query_row("SELECT COUNT(*) FROM faces", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "a rejected combination must wipe nothing");
    }
}

#[test]
fn reset_rejects_a_scoped_rebuild() {
    let lib = seeded();
    let out = lib
        .cmd()
        .args(["faces", "--reset", "--yes", "--ext", "heic"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a scoped reset would wipe everything and rebuild only part"
    );
    let n: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM faces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "a rejected reset must wipe nothing");
}

#[test]
fn reset_reaches_libraries_with_rows_but_no_markers() {
    // An upgraded library: face rows exist from before the marker table
    // existed, so faces_scanned is empty while faces is not. There is
    // state to reset, and the command must not claim otherwise.
    let lib = TestLibrary::new();
    let path = lib.context().paths.root.join("a.jpg");
    lib.copy_fixture("sample_with_exif.jpg", "a.jpg");
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'abc123', 'jpg')",
        [path.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO faces (hash, bbox, embedding, confirmed, person_label)
         VALUES ('abc123', '0,0,50,50', X'0000', 1, 'elena')",
        [],
    )
    .unwrap();
    drop(conn);

    let probe = lib
        .cmd()
        .args(["faces", "--reset", "--yes", "--dry-run"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "an upgraded library has state to reset: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let stderr = String::from_utf8_lossy(&probe.stderr);
    assert!(stderr.contains("1 labeled"), "{stderr}");
}

#[test]
fn reset_with_yes_wipes_and_starts_over() {
    let lib = seeded();
    if common::skip_without_models("faces reset rebuild", common::face_models_cached()) {
        return;
    }
    let out = lib
        .cmd()
        .args([
            "faces",
            "--reset",
            "--yes",
            "--min-cluster-size",
            "1",
            "--silent",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The receipt prints even under --silent: one line naming what was
    // wiped before the rebuild's quiet output.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("reset wiped 1 face row(s)") && stderr.contains("1 labeled"),
        "the reset receipt must name the wipe: {stderr}"
    );
    let conn = lib.conn();
    let labeled: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(labeled, 0, "the rebuild must not resurrect the wiped label");
    let scanned: i64 = conn
        .query_row("SELECT COUNT(*) FROM faces_scanned", [], |r| r.get(0))
        .unwrap();
    assert!(scanned >= 1, "the rebuild re-marks the image as scanned");
}

#[test]
fn reset_with_yes_clears_every_face_state_without_needing_models() {
    let lib = TestLibrary::new();
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO faces
         (hash, bbox, embedding, cluster_id, confirmed, person_label, is_primary)
         VALUES ('orphan', '0,0,50,50', X'0000', 7, 1, 'elena', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO people (name, full_name) VALUES ('elena', 'Elena')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO faces_scanned (hash) VALUES ('orphan')", [])
        .unwrap();
    videre_core::decode_failures::record(
        &conn,
        "orphan",
        videre_core::decode_failures::STAGE_FACES,
        "bad image",
    )
    .unwrap();
    videre_core::decode_failures::record(
        &conn,
        "orphan",
        videre_core::decode_failures::STAGE_EMBED,
        "bad image",
    )
    .unwrap();
    drop(conn);

    // There is deliberately no file_hashes row. The consented wipe completes,
    // then the empty rebuild exits before model loading.
    let out = lib
        .cmd()
        .args(["faces", "--reset", "--yes", "--silent"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conn = lib.conn();
    for table in ["faces", "people", "faces_scanned"] {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "--reset must empty {table}");
    }
    assert_eq!(
        videre_core::decode_failures::fail_count(
            &conn,
            "orphan",
            videre_core::decode_failures::STAGE_FACES,
        )
        .unwrap(),
        0,
        "face decode failures must be retried after reset"
    );
    assert_eq!(
        videre_core::decode_failures::fail_count(
            &conn,
            "orphan",
            videre_core::decode_failures::STAGE_EMBED,
        )
        .unwrap(),
        1,
        "reset must not erase another pipeline's failures"
    );
}

#[test]
fn the_legacy_alias_reaches_the_same_refusal() {
    let lib = seeded();
    let out = lib
        .cmd()
        .args(["faces", "--reprocess"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the alias must parse and refuse identically"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
}

/// The reported scenario: label faces, recluster at a different eps, and
/// assert no unassigned cluster page can contain a labeled face while the
/// person page is unchanged. Model-free: embeddings are seeded, clustering
/// runs on them, detection never runs.
#[test]
fn labeling_then_reclustering_never_contaminates_cluster_pages() {
    let lib = TestLibrary::new();
    let conn = lib.init_db();
    videre_core::face_db::create_faces_table(&conn).unwrap();

    // Two near-identical embeddings (the future Elena pair) and three that
    // resemble each other but not the pair. 512-dim f16 vectors, L2
    // normalized: real model embeddings are unit vectors, and the clustering
    // gates (and attach) are dot products that assume it.
    let mut base: Vec<f32> = (0..512).map(|i| ((i % 7) as f32 - 3.0) / 10.0).collect();
    let mut other: Vec<f32> = (0..512).map(|i| ((i % 5) as f32 - 2.0) / 10.0).collect();
    for v in [&mut base, &mut other] {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    let to_f16 = |v: &[f32]| -> Vec<u8> {
        v.iter()
            .flat_map(|f| half::f16::from_f32(*f).to_le_bytes())
            .collect()
    };
    let pair = to_f16(&base);
    let rest = to_f16(&other);
    for (hash, emb) in [
        ("a.jpg", &pair),
        ("b.jpg", &pair),
        ("c.jpg", &rest),
        ("d.jpg", &rest),
        ("e.jpg", &rest),
    ] {
        let path = lib.context().paths.root.join(hash);
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, 'jpg')",
            rusqlite::params![path.to_string_lossy(), hash],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding) VALUES (?1, '0,0,50,50', ?2)",
            rusqlite::params![hash, emb],
        )
        .unwrap();
    }
    drop(conn);

    // Cluster once, label the pair onto a person through the real assign
    // path, then recluster with a different eps.
    let common_flags = [
        "--min-cluster-size",
        "2",
        "--min-face-size",
        "50",
        "--max-generic-sim",
        "1",
        "--attach-sim",
        "1",
        "--silent",
    ];
    let out = lib
        .cmd()
        .args(["faces", "--recluster", "--eps", "0.5"])
        .args(common_flags)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conn = lib.conn();
    let mut ids: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT id FROM faces WHERE hash IN ('a.jpg', 'b.jpg') AND cluster_id IS NOT NULL",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    ids.sort();
    assert_eq!(ids.len(), 2, "the pair must cluster together");
    videre_api::assign(&conn, &ids, "Elena").unwrap();
    videre_api::set_primary(&conn, ids[1], "Elena").unwrap();
    let before = videre_api::person_detail(&conn, "Elena").unwrap();
    drop(conn);

    for tuning in [
        ["--eps", "0.95", "--min-cluster-size", "2"],
        ["--eps", "0.05", "--min-cluster-size", "3"],
    ] {
        let out = lib
            .cmd()
            .args(["faces", "--recluster"])
            .args(tuning)
            .args([
                "--min-face-size",
                "50",
                "--max-generic-sim",
                "1",
                "--attach-sim",
                "1",
                "--merge-sim",
                "1",
                "--silent",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "recluster {tuning:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let after = videre_api::person_detail(&lib.conn(), "Elena").unwrap();
        assert_eq!(after.full_name, before.full_name);
        assert_eq!(after.faces.len(), before.faces.len());
        assert_eq!(
            after
                .faces
                .iter()
                .map(|face| (face.face_id, face.is_primary))
                .collect::<Vec<_>>(),
            before
                .faces
                .iter()
                .map(|face| (face.face_id, face.is_primary))
                .collect::<Vec<_>>(),
            "reclustering must preserve the person's membership and primary face"
        );
        let attached: i64 = lib
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM faces
                 WHERE confirmed = 1 AND person_label IS NOT NULL AND cluster_id IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            attached, 0,
            "every recluster profile must keep labeled faces out of machine clusters"
        );
    }

    let conn = lib.conn();
    let (labeled, detached): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(cluster_id IS NULL), 0) FROM faces
             WHERE confirmed = 1 AND person_label IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(labeled, 2, "both labels survive");
    assert_eq!(detached, labeled, "no labeled face carries a cluster id");
    let contaminated: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE confirmed = 1 AND cluster_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        contaminated, 0,
        "cluster pages cannot contain labeled faces"
    );
}

#[test]
fn recluster_eps_and_min_cluster_size_control_unlabeled_grouping() {
    let lib = TestLibrary::new();
    drop(lib.init_db());
    seed_unlabeled_faces(&lib, &[0.0, 10.0, 90.0, 100.0]);

    recluster(
        &lib,
        &[
            "--eps",
            "0.01",
            "--min-cluster-size",
            "2",
            "--merge-sim",
            "1",
        ],
    );
    assert_eq!(cluster_shape(&lib), (0, 0), "strict eps splits every pair");

    recluster(
        &lib,
        &[
            "--eps",
            "0.05",
            "--min-cluster-size",
            "2",
            "--merge-sim",
            "1",
        ],
    );
    assert_eq!(
        cluster_shape(&lib),
        (4, 2),
        "wider eps forms the two hand-checked pairs"
    );

    recluster(
        &lib,
        &[
            "--eps",
            "0.05",
            "--min-cluster-size",
            "3",
            "--merge-sim",
            "1",
        ],
    );
    assert_eq!(
        cluster_shape(&lib),
        (0, 0),
        "raising the minimum above each pair returns them to singletons"
    );
}

#[test]
fn recluster_merge_sim_can_reunite_two_subclusters() {
    let lib = TestLibrary::new();
    drop(lib.init_db());
    seed_unlabeled_faces(&lib, &[0.0, 5.0, 45.0, 50.0]);

    recluster(
        &lib,
        &[
            "--eps",
            "0.01",
            "--min-cluster-size",
            "2",
            "--merge-sim",
            "0.8",
        ],
    );
    assert_eq!(cluster_shape(&lib), (4, 2));

    recluster(
        &lib,
        &[
            "--eps",
            "0.01",
            "--min-cluster-size",
            "2",
            "--merge-sim",
            "0.7",
        ],
    );
    assert_eq!(
        cluster_shape(&lib),
        (4, 1),
        "lowering the centroid threshold reunites the nearby subclusters"
    );
}

/// Grouping stays deterministic once the gallery has learned something.
/// Dissolving a cluster through the teaching path records negative evidence
/// about exactly those faces; a recluster must still regroup them the way a
/// library that was never taught does. Learning must never act as a
/// face-specific veto on grouping.
#[test]
fn recluster_ignores_learning_evidence_about_the_same_faces() {
    // Two tight pairs, well apart: the grouping has a knowable answer.
    let angles = [0.0, 3.0, 90.0, 93.0];
    let tuning = ["--eps", "0.1", "--min-cluster-size", "2"];
    let partition = |lib: &TestLibrary| -> Vec<Vec<i64>> {
        let conn = lib.conn();
        let mut statement = conn
            .prepare(
                "SELECT group_concat(id) FROM (SELECT id, cluster_id FROM faces
                 WHERE cluster_id IS NOT NULL ORDER BY id)
                 GROUP BY cluster_id ORDER BY min(id)",
            )
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|ids| {
                ids.unwrap()
                    .split(',')
                    .map(|id| id.parse().unwrap())
                    .collect()
            })
            .collect()
    };

    let control = TestLibrary::new();
    drop(control.init_db());
    seed_unlabeled_faces(&control, &angles);
    recluster(&control, &tuning);
    let expected = partition(&control);
    assert_eq!(
        expected,
        vec![vec![1, 2], vec![3, 4]],
        "fixture must form two pairs"
    );

    let taught = TestLibrary::new();
    drop(taught.init_db());
    seed_unlabeled_faces(&taught, &angles);
    recluster(&taught, &tuning);
    {
        let conn = taught.conn();
        let cluster: i64 = conn
            .query_row("SELECT cluster_id FROM faces WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        videre_api::dissolve_cluster_with_learning(
            &conn,
            cluster,
            &videre_api::TeachingContext {
                embedding_model_id: "arcface/test".into(),
                active_profile_id: None,
            },
        )
        .unwrap();
        let events: i64 = conn
            .query_row("SELECT count(*) FROM face_learning_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            events > 0,
            "the dissolve must have recorded teaching evidence"
        );
    }
    recluster(&taught, &tuning);
    assert_eq!(
        partition(&taught),
        expected,
        "learning evidence changed how recluster groups the same faces"
    );
}
