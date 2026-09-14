//! The shared status model: one producer for everything that reports
//! pipeline state, so the CLI, `stats`, and the MCP tools cannot drift
//! into telling different stories about the same library (the duplication
//! this closes was flagged as DEBT:8).
//!
//! `status` owns operational health: per-stage coverage, pipeline run
//! health, watch liveness, and the next action + cost per gap. Inventory
//! (what is IN the library) stays with `videre stats` and
//! `library_stats::compute_full_in`; this module composes those outputs
//! rather than re-deriving them.

use anyhow::Result;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

/// One stage's outstanding-vs-done shape. Stages without a true
/// outstanding-vs-done count (scan, dedupe) deliberately get no line here:
/// `status` stays glanceable rather than becoming an everything-dashboard.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StageCoverage {
    pub stage: &'static str,
    pub outstanding: i64,
    pub total: i64,
    /// The command that closes this gap, when there is one.
    pub next_command: Option<&'static str>,
    /// Embed and classify are optional, hours-long stages; a newcomer should
    /// not read "behind" on a stage they may never want (spec decision D4).
    pub heavy: bool,
}

/// Faces-eligible hashes: the same population `videre faces` walks (image
/// extensions), deduplicated by hash with a deterministic representative.
fn faces_eligible_hashes(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT hash FROM file_hashes
         WHERE lower(COALESCE(ext, '')) IN
               ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')
         GROUP BY hash",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(Into::into)
}

/// Embed stage: outstanding under the active model of everything eligible.
fn embed_coverage(conn: &Connection, embed_model: &str) -> Result<StageCoverage> {
    let total = crate::embeddings::embeddable_images(conn, embed_model)?.len() as i64;
    let outstanding = match crate::embeddings::pending_images(conn, embed_model)? {
        v => v.len() as i64,
    };
    Ok(StageCoverage {
        stage: "embed",
        outstanding,
        total,
        next_command: Some("videre embed"),
        heavy: true,
    })
}

/// Classify stage: embedded hashes under the model missing a classification.
fn classify_coverage(conn: &Connection, classify_model: &str) -> Result<StageCoverage> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM emb.embeddings WHERE model_id = ?1",
        [classify_model],
        |r| r.get(0),
    )?;
    let outstanding = crate::classify::pending_hashes(conn, classify_model)?.len() as i64;
    Ok(StageCoverage {
        stage: "classify",
        outstanding,
        total,
        next_command: Some("videre classify"),
        heavy: true,
    })
}

/// Faces stage: eligible hashes with no detection record at all. The skip
/// set is "already tried" (including images where zero faces were found),
/// which is why unscanned - not faceless - is the outstanding shape.
fn faces_coverage(conn: &Connection) -> Result<StageCoverage> {
    let eligible = faces_eligible_hashes(conn)?;
    let scanned: std::collections::HashSet<String> =
        crate::face_db::scanned_hashes(conn)?.into_iter().collect();
    let with_faces: std::collections::HashSet<String> = crate::face_db::hashes_with_faces(conn)?
        .into_iter()
        .collect();
    let outstanding = eligible
        .iter()
        .filter(|h| !scanned.contains(*h) && !with_faces.contains(*h))
        .count() as i64;
    Ok(StageCoverage {
        stage: "faces",
        outstanding,
        total: eligible.len() as i64,
        next_command: Some("videre faces"),
        heavy: false,
    })
}

/// Locations stage: geotagged photos with no place name yet. Grouping is a
/// global recompute, so the count is informational; the command line names
/// what closes it.
fn locations_coverage(conn: &Connection) -> Result<StageCoverage> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_hashes WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let outstanding: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_hashes
         WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL AND location_name IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(StageCoverage {
        stage: "locations",
        outstanding,
        total,
        next_command: Some("videre locations"),
        heavy: false,
    })
}

/// Fix-dates stage: rows with a camera date whose stored modified time does
/// not match what fix-dates would write. Judged in Rust because the target
/// depends on the local timezone, which SQL cannot compute.
fn fix_dates_coverage(conn: &Connection) -> Result<StageCoverage> {
    let mut stmt =
        conn.prepare("SELECT exif_date, modified_at FROM file_hashes WHERE exif_date IS NOT NULL")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut total = 0i64;
    let mut outstanding = 0i64;
    for row in rows {
        let (exif_date, modified_at) = row?;
        total += 1;
        if let Some(target) = crate::fix_dates_target::target_modified_at(&exif_date) {
            if modified_at.as_deref() != Some(target.as_str()) {
                outstanding += 1;
            }
        }
    }
    Ok(StageCoverage {
        stage: "fix-dates",
        outstanding,
        total,
        next_command: Some("videre fix-dates"),
        heavy: false,
    })
}

/// Coverage for every stage that has a true outstanding-vs-done count.
/// One aggregate query per stage; no per-file work outside the fix-dates
/// transform, which is in-memory arithmetic over already-stored strings.
pub fn coverage_in(
    conn: &Connection,
    embed_model: &str,
    classify_model: &str,
) -> Result<Vec<StageCoverage>> {
    Ok(vec![
        embed_coverage(conn, embed_model)?,
        classify_coverage(conn, classify_model)?,
        faces_coverage(conn)?,
        locations_coverage(conn)?,
        fix_dates_coverage(conn)?,
    ])
}

/// Whether a watcher is alive, and when its last cycle completed. Running-
/// ness comes from the watch lock; the last-cycle time from the heartbeat
/// row `videre watch` writes at the end of each successful cycle. A watcher
/// that died mid-cycle leaves the previous heartbeat, so it reads as a stale
/// last-cycle time rather than pretending nothing is wrong.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WatchLiveness {
    pub running: bool,
    pub last_cycle_at: Option<String>,
}

/// Watch liveness for one library. Never errors on a library that never
/// watched: that is the "never run" case, not a failure.
pub fn watch_liveness_in(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
) -> Result<WatchLiveness> {
    crate::pipeline_runs::ensure_pipeline_runs_table(conn)?;
    let last_cycle_at: Option<String> = conn
        .query_row(
            "SELECT started_at FROM pipeline_runs WHERE command = 'watch'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(WatchLiveness {
        running: crate::library_locks::command_locked(ctx, "watch")?,
        last_cycle_at,
    })
}

/// An approximate duration for one stage's outstanding work, clearly a
/// guess and rendered as one ("~1.6h"). `secs` is `None` when there is
/// nothing outstanding: no work, no estimate.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CostEstimate {
    pub secs: Option<u64>,
    pub approximate: bool,
}

/// Estimate cost from the last successful run's measured throughput when the
/// run recorded an item count, else from a coarse per-item constant. The
/// estimator is deliberately isolated: pipeline_runs does not yet store item
/// counts, so today the measured path is exercised by tests and the fallback
/// is what users see; when runs start recording items, this function
/// improves without touching any caller.
pub fn estimate_cost(
    outstanding: i64,
    last_run_ms: Option<i64>,
    last_run_items: Option<i64>,
    fallback_secs_per_item: f64,
) -> CostEstimate {
    let secs = if outstanding <= 0 {
        None
    } else {
        let per_item = match (last_run_ms, last_run_items) {
            (Some(ms), Some(items)) if ms > 0 && items > 0 => ms as f64 / items as f64 / 1000.0,
            _ => fallback_secs_per_item,
        };
        Some((outstanding as f64 * per_item).ceil() as u64)
    };
    CostEstimate {
        secs,
        approximate: true,
    }
}

/// The whole operational picture for one library, from one call. Rendered
/// by `videre status` (text and --json); `stats` and the MCP tools read the
/// same model rather than re-deriving it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StatusReport {
    pub coverage: Vec<StageCoverage>,
    pub pipelines: Vec<crate::pipeline_runs::PipelineRunStatus>,
    pub watch: WatchLiveness,
    /// Per-stage cost estimate for the outstanding work, in the same stage
    /// order as `coverage` entries that have outstanding work.
    pub costs: Vec<(&'static str, CostEstimate)>,
    /// The embedding model the coverage numbers were measured against.
    pub embed_model: String,
}

impl StatusReport {
    /// True only when a pipeline run actually failed or crashed. Staleness
    /// is informational and never a failure: a library mid-setup is healthy
    /// (spec decision D5).
    pub fn has_problem(&self) -> bool {
        self.pipelines
            .iter()
            .any(|p| matches!(p.status.as_deref(), Some("failed") | Some("crashed")))
    }
}

/// Fallback per-item seconds for stages with no measured history. Coarse by
/// design and always rendered as approximate; embed/classify dominate real
/// runs, so their constants err on the slow side of honest.
fn fallback_secs_per_item(stage: &str) -> f64 {
    match stage {
        "embed" => 2.0,
        "classify" => 0.05,
        "faces" => 1.0,
        "locations" => 0.001,
        "fix-dates" => 0.001,
        _ => 1.0,
    }
}

/// Assemble the report: coverage, pipeline health, watch liveness, and the
/// cost of closing each gap. One call, one connection, read-only.
pub fn compute_status_in(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
) -> Result<StatusReport> {
    let embed_model = ctx.settings.default_model.clone();
    let coverage = coverage_in(conn, &embed_model, &embed_model)?;
    let pipelines = crate::pipeline_runs::read_all_in(conn, ctx)?;
    let watch = watch_liveness_in(conn, ctx)?;
    let costs = coverage
        .iter()
        .filter(|c| c.outstanding > 0)
        .map(|c| {
            let prior = pipelines.iter().find(|p| p.command == c.stage);
            (
                c.stage,
                estimate_cost(
                    c.outstanding,
                    prior.and_then(|p| p.duration_ms),
                    None,
                    fallback_secs_per_item(c.stage),
                ),
            )
        })
        .collect();
    Ok(StatusReport {
        coverage,
        pipelines,
        watch,
        costs,
        embed_model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MODEL: &str = "test/model";

    /// A main database with `file_hashes`, plus a real attached model
    /// database (mirroring production's split), and the faces tables.
    fn seed_db(tag: &str) -> Connection {
        let ctx = crate::embeddings_db::test_context(tag);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path        TEXT PRIMARY KEY,
                hash        TEXT NOT NULL,
                mime        TEXT,
                size_bytes  INTEGER,
                created_at  TEXT,
                modified_at TEXT,
                ext         TEXT,
                phash       INTEGER,
                exif_date   TEXT,
                gps_lat     REAL,
                gps_lon     REAL,
                width       INTEGER,
                height      INTEGER,
                location_name TEXT
            );",
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS classifications (
                model_id      TEXT NOT NULL,
                hash          TEXT NOT NULL,
                category      TEXT NOT NULL,
                confidence    REAL NOT NULL,
                classified_at TEXT NOT NULL,
                PRIMARY KEY (model_id, hash)
            );",
        )
        .unwrap();
        crate::embeddings_db::attach_in(&conn, &ctx, TEST_MODEL, true).unwrap();
        crate::face_db::create_faces_table(&conn).unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str, hash: &str, ext: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, ?3)",
            [path, hash, ext],
        )
        .unwrap();
    }

    #[test]
    fn coverage_counts_outstanding_per_stage() {
        let conn = seed_db("status_cov_main");
        // 3 embeddable images: 1 embedded+classified, 1 embedded, 1 neither.
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", "jpg");
        insert_file(&conn, "/a/3.jpg", "h3", "jpg");
        crate::embeddings::insert_embeddings(
            &conn,
            TEST_MODEL,
            &[("h1".into(), vec![0u8; 4]), ("h2".into(), vec![0u8; 4])],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO classifications (model_id, hash, category, confidence, classified_at)
             VALUES ('test/model', 'h1', 'cat', 0.9, 'now')",
            [],
        )
        .unwrap();

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let embed = cov.iter().find(|c| c.stage == "embed").unwrap();
        assert_eq!(embed.outstanding, 1, "h3 is the only un-embedded image");
        assert_eq!(embed.total, 3);
        assert!(embed.heavy);
        assert_eq!(embed.next_command, Some("videre embed"));

        let classify = cov.iter().find(|c| c.stage == "classify").unwrap();
        assert_eq!(classify.outstanding, 1, "h2 is embedded but unclassified");
        assert_eq!(classify.total, 2);
    }

    #[test]
    fn faces_coverage_counts_unscanned_not_faceless() {
        let conn = seed_db("status_cov_faces");
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", "jpg");
        insert_file(&conn, "/a/3.png", "h3", "png");
        insert_file(&conn, "/a/v.mp4", "h4", "mp4"); // not faces-eligible
                                                     // h1 was scanned and found faceless: still done. h2 has faces: done.
        conn.execute("INSERT INTO faces_scanned (hash) VALUES ('h1')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding) VALUES ('h2', '0,0,10,10', X'00')",
            [],
        )
        .unwrap();

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let faces = cov.iter().find(|c| c.stage == "faces").unwrap();
        assert_eq!(faces.total, 3, "videos are not faces-eligible");
        assert_eq!(faces.outstanding, 1, "only h3 was never tried");
        assert!(!faces.heavy);
    }

    #[test]
    fn locations_and_fix_dates_count_only_true_gaps() {
        let conn = seed_db("status_cov_locfix");
        // "In sync" is judged against THIS machine's target: the transform
        // resolves the camera-local time through the local timezone, so a
        // hardcoded offset passes on one continent and fails on another (it
        // did, on CI). Rows in sync here carry exactly what
        // `target_modified_at` would write.
        let exif = "2021-07-04T15:30:00";
        let in_sync = crate::fix_dates_target::target_modified_at(exif).unwrap();
        // geotagged, named: done. geotagged, unnamed: outstanding.
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon, location_name,
                                      exif_date, modified_at)
             VALUES ('/a/1.jpg', 'h1', 'jpg', 52.5, 13.4, 'Berlin, Germany', ?1, ?2)",
            [exif.to_string(), in_sync.clone()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon, exif_date, modified_at)
             VALUES ('/a/2.jpg', 'h2', 'jpg', 48.8, 2.3, '2020-01-02T03:04:05', '1970-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext, exif_date, modified_at)
             VALUES ('/a/3.jpg', 'h3', 'jpg', ?1, ?2)",
            [exif.to_string(), in_sync],
        )
        .unwrap();

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let locations = cov.iter().find(|c| c.stage == "locations").unwrap();
        assert_eq!(locations.total, 2);
        assert_eq!(locations.outstanding, 1, "h2 has GPS and no place name");

        let fix = cov.iter().find(|c| c.stage == "fix-dates").unwrap();
        assert_eq!(fix.total, 3);
        assert_eq!(
            fix.outstanding, 1,
            "only h2's mtime disagrees with its exif_date"
        );
    }

    #[test]
    fn cost_uses_measured_rate_then_falls_back() {
        // 100 items took 50_000ms -> 500ms/item; 10 outstanding -> ~5s.
        let c = estimate_cost(10, Some(50_000), Some(100), 2.0);
        assert_eq!(c.secs, Some(5));
        // no prior run -> fallback 2s/item * 10 = 20s.
        let f = estimate_cost(10, None, None, 2.0);
        assert_eq!(f.secs, Some(20));
        assert!(f.approximate);
        // nothing outstanding: no estimate, not zero seconds of work.
        assert_eq!(estimate_cost(0, None, None, 2.0).secs, None);
        // a nonsense prior run (zero items) must not divide by zero.
        assert_eq!(estimate_cost(10, Some(50_000), Some(0), 2.0).secs, Some(20));
    }

    #[test]
    fn compute_status_assembles_the_whole_report() {
        let ctx = crate::embeddings_db::test_context("status_compute");
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        let conn = seed_db("status_compute");
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", "jpg");

        let report = compute_status_in(&conn, &ctx).unwrap();
        assert_eq!(report.embed_model, ctx.settings.default_model);
        assert!(!report.coverage.is_empty());
        let embed = report.coverage.iter().find(|c| c.stage == "embed").unwrap();
        assert_eq!(embed.outstanding, 2);
        assert!(report
            .costs
            .iter()
            .any(|(stage, cost)| *stage == "embed" && cost.secs.is_some()));
        assert!(report.pipelines.iter().all(|p| p.status.is_none()));
        assert!(!report.watch.running);
        assert_eq!(report.watch.last_cycle_at, None);
        assert!(
            !report.has_problem(),
            "a fresh library is healthy, not failing"
        );

        // A failed run is what flips --check, and staleness never does.
        crate::pipeline_runs::start_run(&conn, "faces").unwrap();
        crate::pipeline_runs::finish_run(&conn, "faces", "failed", 5, Some("boom")).unwrap();
        let report = compute_status_in(&conn, &ctx).unwrap();
        assert!(report.has_problem());
    }
}
