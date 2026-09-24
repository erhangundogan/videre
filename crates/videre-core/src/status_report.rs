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
    /// Files this stage has given up decoding (at the two-strike threshold) and
    /// deliberately no longer attempts. They are excluded from `outstanding` so
    /// the count reflects what the command can actually act on, and reported
    /// here so `done + outstanding + skipped == total` still holds. Zero for
    /// stages with no decode step (`decode_failures` never records them).
    pub skipped: i64,
    /// True only for the locations stage when the live GPS fingerprint differs
    /// from the one the last recompute stored: clusters are stale in a way the
    /// outstanding count (which sees only unassigned rows) cannot. Prune and
    /// dedupe shrink the data without leaving an unassigned row, so this is the
    /// only signal for them. False for every other stage.
    pub stale: bool,
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
    let pending = crate::embeddings::pending_images(conn, embed_model)?;
    // Files embed has given up decoding are still pending (never embedded), so
    // separate them out: they are not work embed will do.
    let failed = crate::decode_failures::failed_hashes(
        conn,
        crate::decode_failures::STAGE_EMBED,
        crate::decode_failures::FAILURE_THRESHOLD,
    )?;
    let skipped = pending.iter().filter(|p| failed.contains(&p.hash)).count() as i64;
    Ok(StageCoverage {
        stage: "embed",
        stale: false,
        outstanding: pending.len() as i64 - skipped,
        total,
        next_command: Some("videre embed"),
        heavy: true,
        skipped,
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
        stale: false,
        outstanding,
        total,
        next_command: Some("videre classify"),
        heavy: true,
        skipped: 0,
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
    // Files the face decode has given up on are never marked scanned, so they
    // would otherwise count as outstanding forever.
    let failed = crate::decode_failures::failed_hashes(
        conn,
        crate::decode_failures::STAGE_FACES,
        crate::decode_failures::FAILURE_THRESHOLD,
    )?;
    let untried: Vec<&String> = eligible
        .iter()
        .filter(|h| !scanned.contains(*h) && !with_faces.contains(*h))
        .collect();
    let skipped = untried.iter().filter(|h| failed.contains(**h)).count() as i64;
    Ok(StageCoverage {
        stage: "faces",
        stale: false,
        outstanding: untried.len() as i64 - skipped,
        total: eligible.len() as i64,
        next_command: Some("videre faces"),
        heavy: false,
        skipped,
    })
}

/// Locations stage: geotagged photos with no cluster assignment yet. Grouping
/// is a global recompute, so the count is informational; the command line
/// names what closes it.
///
/// Computing `stale` recomputes the GPS fingerprint on every call, an ordered
/// scan of all GPS-bearing rows served by the GPS index. Cheap at current
/// scale; a library with very many GPS rows makes `status` marginally slower.
fn locations_coverage(conn: &Connection) -> Result<StageCoverage> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_hashes WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let outstanding: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_hashes
         WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL
           AND location_cluster_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    // Stale only when a recompute has run (a stored fingerprint exists) and the
    // GPS data has moved since. A library that never clustered is "never", not
    // "stale": its unassigned rows already suggest the command.
    let live = crate::location_cluster::gps_fingerprint(conn)?;
    let stale =
        crate::library_state::get_string(conn, crate::location_cluster::LOCATIONS_GPS_FINGERPRINT)?
            .is_some_and(|stored| stored != live);
    Ok(StageCoverage {
        stage: "locations",
        outstanding,
        total,
        next_command: Some("videre locations"),
        heavy: false,
        skipped: 0,
        stale,
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
        if crate::fix_dates_target::is_current(&exif_date, modified_at.as_deref()) == Some(false) {
            outstanding += 1;
        }
    }
    Ok(StageCoverage {
        stage: "fix-dates",
        stale: false,
        outstanding,
        total,
        next_command: Some("videre fix-dates"),
        heavy: false,
        skipped: 0,
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

/// A measured duration for one stage's outstanding work, or `None` when there
/// is no measurement to base one on. `secs` is `Some` only when a real prior
/// run supplied a throughput; otherwise callers show the item count and an
/// "intensive" marker rather than a number.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CostEstimate {
    pub secs: Option<u64>,
    pub approximate: bool,
}

/// A duration for the outstanding work, but only when it can be measured. We
/// deliberately do NOT fabricate one from a per-item constant: throughput
/// swings by orders of magnitude across hardware (GPU vs CPU), batch size and
/// media type, so a guessed duration is confidently wrong more often than it is
/// useful, and a wrong number erodes trust in everything else the tool reports.
/// `secs` is `Some` only when the last successful run recorded both how long it
/// took and how many items it processed; until `pipeline_runs` stores item
/// counts (see the item-count work) that is never, so today this returns `None`
/// and callers render the count plus an "intensive" marker. `None` also when
/// nothing is outstanding.
pub fn estimate_cost(
    outstanding: i64,
    last_run_ms: Option<i64>,
    last_run_items: Option<i64>,
) -> CostEstimate {
    let secs = match (last_run_ms, last_run_items) {
        (Some(ms), Some(items)) if outstanding > 0 && ms > 0 && items > 0 => {
            let per_item = ms as f64 / items as f64 / 1000.0;
            Some((outstanding as f64 * per_item).ceil() as u64)
        }
        _ => None,
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
    /// The latest run of each command, as its log records it.
    pub logs: Vec<crate::error_log::CommandLogSummary>,
}

impl StatusReport {
    /// True only when a pipeline run actually failed or crashed, or the
    /// latest run of some command logged an error. The latter catches runs
    /// whose per-item failures leave the pipeline row at `success`. Warnings
    /// and staleness are informational and never a failure: a library
    /// mid-setup is healthy.
    pub fn has_problem(&self) -> bool {
        self.pipelines
            .iter()
            .any(|p| matches!(p.status.as_deref(), Some("failed") | Some("crashed")))
            || self.logs.iter().any(|l| l.errors > 0)
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
    // A log that cannot be read never breaks status: it reports what it can.
    let logs = crate::error_log::latest_runs(ctx).unwrap_or_else(|e| {
        tracing::debug!("could not read the command logs: {e:#}");
        Vec::new()
    });
    let costs = coverage
        .iter()
        .filter(|c| c.outstanding > 0)
        .map(|c| {
            let prior = pipelines.iter().find(|p| p.command == c.stage);
            (
                c.stage,
                // pipeline_runs records a duration but not an item count yet, so
                // the second input is None and the estimate stays None until
                // that lands. No fabricated fallback.
                estimate_cost(c.outstanding, prior.and_then(|p| p.duration_ms), None),
            )
        })
        .collect();
    Ok(StatusReport {
        coverage,
        pipelines,
        watch,
        costs,
        embed_model,
        logs,
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
                location_name TEXT,
                location_cluster_id INTEGER
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
    fn decode_failed_files_are_skipped_not_outstanding() {
        // A file the embed/faces decode has given up on (at the two-strike
        // threshold) is not work either command will do, so it must not inflate
        // the outstanding count or keep suggesting the command. It is reported
        // separately as `skipped` so the arithmetic stays legible.
        let conn = seed_db("status_cov_skipped");
        crate::decode_failures::ensure_table(&conn).unwrap();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", "jpg");
        insert_file(&conn, "/a/3.jpg", "h3", "jpg");
        // h3 is permanently undecodable for embed; h2 for faces.
        for _ in 0..crate::decode_failures::FAILURE_THRESHOLD {
            crate::decode_failures::record(&conn, "h3", crate::decode_failures::STAGE_EMBED, "x")
                .unwrap();
            crate::decode_failures::record(&conn, "h2", crate::decode_failures::STAGE_FACES, "x")
                .unwrap();
        }

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();

        let embed = cov.iter().find(|c| c.stage == "embed").unwrap();
        assert_eq!(embed.total, 3);
        assert_eq!(embed.outstanding, 2, "h3 is skipped, not outstanding");
        assert_eq!(embed.skipped, 1, "h3 is reported as undecodable");

        let faces = cov.iter().find(|c| c.stage == "faces").unwrap();
        assert_eq!(faces.outstanding, 2, "h2 is skipped, not outstanding");
        assert_eq!(faces.skipped, 1, "h2 is reported as undecodable");

        // A stage with no decode-failure notion reports zero, never a wrong sum.
        let classify = cov.iter().find(|c| c.stage == "classify").unwrap();
        assert_eq!(classify.skipped, 0);
    }

    #[test]
    fn a_single_decode_failure_still_counts_as_outstanding() {
        // Below the threshold the file may still succeed (a transient timeout),
        // so it stays outstanding and unskipped.
        let conn = seed_db("status_cov_one_strike");
        crate::decode_failures::ensure_table(&conn).unwrap();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        crate::decode_failures::record(&conn, "h1", crate::decode_failures::STAGE_EMBED, "x")
            .unwrap();

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let embed = cov.iter().find(|c| c.stage == "embed").unwrap();
        assert_eq!(embed.outstanding, 1, "one strike does not skip");
        assert_eq!(embed.skipped, 0);
    }

    #[test]
    fn locations_coverage_tracks_cluster_assignment_not_place_name() {
        let conn = seed_db("status_cov_locations");
        // The locations command owns location_cluster_id. A cluster can be
        // assigned even when reverse geocoding produced no display name.
        conn.execute(
            "INSERT INTO file_hashes
             (path, hash, ext, gps_lat, gps_lon, location_cluster_id)
             VALUES ('/a/1.jpg', 'h1', 'jpg', 52.5, 13.4, 7)",
            [],
        )
        .unwrap();

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let locations = cov.iter().find(|c| c.stage == "locations").unwrap();
        assert_eq!(locations.total, 1);
        assert_eq!(
            locations.outstanding, 0,
            "an assigned cluster completes locations even without a place name"
        );

        // A display name belongs to reverse geocoding and must not make an
        // unclustered row look complete.
        conn.execute(
            "UPDATE file_hashes
             SET location_cluster_id = NULL, location_name = 'Berlin, Germany'
             WHERE path = '/a/1.jpg'",
            [],
        )
        .unwrap();

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let locations = cov.iter().find(|c| c.stage == "locations").unwrap();
        assert_eq!(
            locations.outstanding, 1,
            "a place name does not replace the missing cluster assignment"
        );
    }

    #[test]
    fn locations_coverage_reports_staleness_the_outstanding_count_cannot_see() {
        let conn = seed_db("status_cov_locations_stale");
        // Two GPS rows, both assigned to a cluster: outstanding is zero, so the
        // coverage count alone looks complete.
        conn.execute(
            "INSERT INTO file_hashes
             (path, hash, ext, gps_lat, gps_lon, location_cluster_id)
             VALUES ('/a/1.jpg','h1','jpg',52.5,13.4,1), ('/a/2.jpg','h2','jpg',48.8,2.35,2)",
            [],
        )
        .unwrap();
        // The fingerprint the recompute would have stored for this data.
        let fresh = crate::location_cluster::gps_fingerprint(&conn).unwrap();
        crate::library_state::set_string(
            &conn,
            crate::location_cluster::LOCATIONS_GPS_FINGERPRINT,
            &fresh,
        )
        .unwrap();

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let locations = cov.iter().find(|c| c.stage == "locations").unwrap();
        assert_eq!(locations.outstanding, 0);
        assert!(!locations.stale, "a fresh fingerprint is not stale");

        // A row disappears (the prune case): no unassigned row remains, but the
        // fingerprint moved.
        conn.execute("DELETE FROM file_hashes WHERE path = '/a/2.jpg'", [])
            .unwrap();
        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let locations = cov.iter().find(|c| c.stage == "locations").unwrap();
        assert_eq!(
            locations.outstanding, 0,
            "the surviving row is still assigned"
        );
        assert!(
            locations.stale,
            "shrunk data with an old fingerprint must read stale"
        );
    }

    #[test]
    fn fix_dates_coverage_compares_rfc3339_values_as_instants() {
        let conn = seed_db("status_cov_fix_dates");
        let exif = "2021-07-04T15:30:00";
        let target = crate::fix_dates_target::target_modified_at(exif).unwrap();
        let target_dt = chrono::DateTime::parse_from_rfc3339(&target).unwrap();
        let other_offset = if target_dt.offset().local_minus_utc() == 14 * 60 * 60 {
            chrono::FixedOffset::west_opt(12 * 60 * 60).unwrap()
        } else {
            chrono::FixedOffset::east_opt(14 * 60 * 60).unwrap()
        };
        let same_instant = target_dt.with_timezone(&other_offset).to_rfc3339();
        assert_ne!(
            target, same_instant,
            "fixture must use different RFC3339 text"
        );

        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext, exif_date, modified_at)
             VALUES ('/a/1.jpg', 'h1', 'jpg', ?1, ?2)",
            [exif, &same_instant],
        )
        .unwrap();

        let cov = coverage_in(&conn, TEST_MODEL, TEST_MODEL).unwrap();
        let fix = cov.iter().find(|c| c.stage == "fix-dates").unwrap();
        assert_eq!(fix.total, 1);
        assert_eq!(
            fix.outstanding, 0,
            "equivalent RFC3339 representations describe the same file time"
        );
    }

    #[test]
    fn cost_is_shown_only_when_it_can_be_measured() {
        // 100 items took 50_000ms -> 500ms/item; 10 outstanding -> ~5s.
        let c = estimate_cost(10, Some(50_000), Some(100));
        assert_eq!(c.secs, Some(5));
        assert!(c.approximate);
        // No prior run with an item count: no fabricated estimate.
        assert_eq!(estimate_cost(10, None, None).secs, None);
        // A duration but no item count (today's pipeline_runs): still None.
        assert_eq!(estimate_cost(10, Some(50_000), None).secs, None);
        // Nothing outstanding: no estimate.
        assert_eq!(estimate_cost(0, Some(50_000), Some(100)).secs, None);
        // A nonsense prior run (zero items) must not divide by zero -> None.
        assert_eq!(estimate_cost(10, Some(50_000), Some(0)).secs, None);
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
        // embed has an entry, but with no measured prior run there is no
        // fabricated duration.
        let embed_cost = report
            .costs
            .iter()
            .find(|(stage, _)| *stage == "embed")
            .expect("embed cost entry");
        assert_eq!(embed_cost.1.secs, None);
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
