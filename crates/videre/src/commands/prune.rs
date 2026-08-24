use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(clap::Args)]
pub struct PruneArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Preview changes without modifying the database
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Suppress per-file output (errors are always shown)
    #[arg(long)]
    pub(crate) silent: bool,

    /// Also remove rows whose directory is missing entirely.
    ///
    /// By default those are kept, because a missing directory usually means an
    /// unmounted drive rather than deleted files, and deleting the rows also
    /// orphans their embeddings and cached thumbnails. Pass this when the
    /// folder really is gone for good.
    #[arg(long)]
    pub(crate) prune_unreachable: bool,

    /// Proceed even when a run would remove an implausibly large share of the
    /// library. Separate from --prune-unreachable on purpose: "that folder is
    /// gone" and "yes, delete tens of thousands of rows" are different claims.
    #[arg(long)]
    pub(crate) force: bool,
}

impl PruneArgs {
    /// Constructs args for an in-process prune pass against an already-open
    /// connection (used by `videre watch --prune`), `db`/`dry_run` aren't
    /// meaningful there since the caller already has a connection and always
    /// wants the real (non-preview) pass, so only `silent` is exposed.
    pub(crate) fn for_watch_stage(silent: bool) -> Self {
        Self {
            db: None,
            dry_run: false,
            silent,
            // Never overridable from `watch`: it runs unattended on a loop and
            // cannot ask. A user who genuinely wants either runs `videre prune`
            // by hand.
            prune_unreachable: false,
            force: false,
        }
    }
}

/// How many missing directories to name before summarising the rest. The
/// report exists to stop a flood of near-identical lines, so it must not
/// become one itself.
const MAX_REPORTED_DIRS: usize = 5;

/// Consecutive failures after which the run aborts.
///
/// Consecutive rather than cumulative: a library with a handful of genuinely
/// unreadable files should still prune, while a systemically failing drive
/// should stop immediately instead of emitting one line per row. Any success
/// resets the count.
const MAX_CONSECUTIVE_ERRORS: usize = 10;

/// Share of the library whose removal is implausible enough to stop for.
///
/// Both conditions must hold. A percentage alone would block a five-row
/// fixture where three files were legitimately deleted; a raw count alone
/// would never trip on a small library.
const BULK_DELETE_FRACTION: f64 = 0.20;
const BULK_DELETE_MIN_ROWS: usize = 100;

/// Reports a run that stopped because failures kept coming.
///
/// Prints the first error verbatim: after ten near-identical messages it is
/// the only one that still carries information, and it is the one that has
/// scrolled away.
fn abort_on_repeated_errors(
    consecutive: usize,
    total_errors: usize,
    checked: usize,
    first: &Option<String>,
) {
    eprintln!(
        "aborted after {consecutive} consecutive errors ({total_errors} total), \
         {checked} row(s) checked"
    );
    if let Some(e) = first {
        eprintln!("  first error: {e}");
    }
    eprintln!("  the volume may be failing; earlier changes are already committed and prune is idempotent, so re-run once the cause is fixed");
}

fn system_time_to_iso(t: SystemTime) -> String {
    let dt: DateTime<Utc> = t.into();
    dt.to_rfc3339()
}

/// Whether a row's stored `modified_at` needs replacing with the file's current
/// timestamp.
///
/// Split out as a pure function so the rule is testable without a database, a
/// filesystem, or a spawned binary; same reason `home::decide_db` is separate
/// from `resolve_db`.
///
/// A plain string comparison is exact because both sides are produced by the
/// same formatting: `hasher::system_time_to_iso` writes the stored value at scan
/// time and the local one reproduces it here, both `DateTime<Utc>` then
/// `to_rfc3339()`. Comparing parsed instants instead would be no more correct
/// and would make a malformed stored value an error rather than a difference.
fn needs_sync(stored: Option<&str>, current: &str) -> bool {
    // None is a row that has never been synced, so it always is one.
    stored != Some(current)
}

pub fn run(args: PruneArgs) -> anyhow::Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;

    if !db.exists() {
        eprintln!("Error: {:?} does not exist", db);
        std::process::exit(1);
    }

    if args.dry_run && !args.silent {
        eprintln!("Dry run: no changes will be made to the database.");
    }

    let conn = videre_core::db::open_wal(&db).expect("failed to open database");

    let errors =
        videre_core::pipeline_runs::track(&conn, &db, "prune", || run_prune(&args, &conn, &db))?;

    if errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// The actual prune work, wrapped by `track()` above. Returns the error
/// count so the caller can decide the exit code after tracking has already
/// finalized the run. `pub(crate)` so `videre watch --prune` can reuse it
/// directly against its own already-open connection instead of duplicating
/// the orphan-cleanup logic.
pub(crate) fn run_prune(
    args: &PruneArgs,
    conn: &Connection,
    db: &std::path::Path,
) -> anyhow::Result<usize> {
    // `modified_at` is selected because the sync below compares against it.
    // Without it there is nothing to compare to, so every extant row looked
    // like it needed syncing and was rewritten on every run: see the loop.
    let paths: Vec<(String, Option<String>)> = {
        let mut stmt = conn
            .prepare("SELECT path, modified_at FROM file_hashes ORDER BY path")
            .expect("failed to prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("failed to execute")
            .filter_map(|r| r.ok())
            .collect()
    };

    let total = paths.len();
    let mut removed = 0usize;
    let mut synced = 0usize;
    let mut errors = 0usize;

    // Directories whose absence made rows unreachable, for the summary. A set,
    // because one missing drive accounts for thousands of rows and the useful
    // report is "these 2 directories are gone", not 12,431 identical lines.
    let mut unreachable_dirs: std::collections::BTreeSet<String> = Default::default();
    let mut unreachable = 0usize;
    // Consecutive, not cumulative: a few unreadable files should not abort an
    // otherwise good run, but a systemically failing drive should stop at once
    // instead of printing one line per row.
    let mut consecutive = 0usize;
    let mut first_error: Option<String> = None;

    // Classify every row before touching the database, so the bulk guard below
    // can see the true number of removals and refuse *before* any of them
    // happen. Acting as we classify would mean discovering the run was
    // implausible only after deleting most of it.
    enum Fate {
        Remove,
        Sync(String),
    }
    let mut planned: Vec<(&String, Fate)> = Vec::with_capacity(paths.len());

    for (path, stored_mtime) in &paths {
        match std::fs::metadata(path) {
            Err(_) => {
                // A missing file is only a deletion if its parent is still
                // there. Parent gone means the directory, or the whole volume,
                // is gone: keep the row. Deleting it would additionally orphan
                // its embedding and cached thumbnail, which is hours of
                // recompute against minutes to re-scan the row.
                let p = std::path::Path::new(path);
                if !videre_core::io_timeout::absence_is_trustworthy(p) && !args.prune_unreachable {
                    if let Some(parent) = p.parent() {
                        unreachable_dirs.insert(parent.display().to_string());
                    }
                    unreachable += 1;
                    continue;
                }
                planned.push((path, Fate::Remove));
            }
            Ok(meta) => match meta.modified() {
                // Only a row whose stored value actually differs is a sync.
                // This used to push a Sync unconditionally, so a run on an
                // unchanged library rewrote every row with the value already
                // there, reported the whole library as synced, and never
                // converged: a second run said exactly the same. That made real
                // drift indistinguishable from the constant background, and
                // `watch --prune` repeated it every cycle, taking the single WAL
                // writer lock for every row each time.
                //
                // A plain string comparison is sound because both sides come
                // from the same `to_rfc3339()` formatting: `hasher.rs` writes it
                // at scan time, `system_time_to_iso` reproduces it here.
                Ok(t) => {
                    let current = system_time_to_iso(t);
                    if needs_sync(stored_mtime.as_deref(), &current) {
                        planned.push((path, Fate::Sync(current)));
                    }
                }
                Err(e) => {
                    eprintln!("Error reading mtime for {path}: {e}");
                    errors += 1;
                    if first_error.is_none() {
                        first_error = Some(format!("reading mtime for {path}: {e}"));
                    }
                    consecutive += 1;
                    if consecutive >= MAX_CONSECUTIVE_ERRORS {
                        abort_on_repeated_errors(consecutive, errors, total, &first_error);
                        return Ok(errors);
                    }
                    continue;
                }
            },
        }
        consecutive = 0;
    }

    // Both conditions, deliberately. See the constants' doc comments.
    let to_remove = planned
        .iter()
        .filter(|(_, f)| matches!(f, Fate::Remove))
        .count();
    if !args.force
        && to_remove >= BULK_DELETE_MIN_ROWS
        && (to_remove as f64) > (total as f64) * BULK_DELETE_FRACTION
    {
        eprintln!(
            "refusing to remove {to_remove} of {total} row(s) ({:.0}% of the library): \
             that is more likely a mounting accident than deleted photos.",
            (to_remove as f64 / total as f64) * 100.0
        );
        eprintln!("  nothing was changed; re-run with --force if this is intended");
        return Ok(errors);
    }

    for (path, fate) in &planned {
        match fate {
            Fate::Remove => {
                if !args.silent {
                    let tag = if args.dry_run {
                        "[dry-run] would remove"
                    } else {
                        "[removed]"
                    };
                    println!("{tag} {path}");
                }
                if !args.dry_run {
                    if let Err(e) = conn.execute(
                        "DELETE FROM file_hashes WHERE path = ?1",
                        rusqlite::params![path],
                    ) {
                        eprintln!("Error removing {path}: {e}");
                        errors += 1;
                        if first_error.is_none() {
                            first_error = Some(format!("removing {path}: {e}"));
                        }
                        consecutive += 1;
                        if consecutive >= MAX_CONSECUTIVE_ERRORS {
                            abort_on_repeated_errors(consecutive, errors, total, &first_error);
                            return Ok(errors);
                        }
                        continue;
                    }
                }
                removed += 1;
            }
            Fate::Sync(mtime) => {
                if !args.dry_run {
                    if let Err(e) = conn.execute(
                        "UPDATE file_hashes SET modified_at = ?1 WHERE path = ?2",
                        rusqlite::params![mtime, path],
                    ) {
                        eprintln!("Error syncing {path}: {e}");
                        errors += 1;
                        if first_error.is_none() {
                            first_error = Some(format!("syncing {path}: {e}"));
                        }
                        consecutive += 1;
                        if consecutive >= MAX_CONSECUTIVE_ERRORS {
                            abort_on_repeated_errors(consecutive, errors, total, &first_error);
                            return Ok(errors);
                        }
                        continue;
                    }
                }
                if !args.silent {
                    let tag = if args.dry_run {
                        "[dry-run] would sync"
                    } else {
                        "[synced]"
                    };
                    println!("{tag} {path}  modified_at -> {mtime}");
                }
                synced += 1;
            }
        }
    }

    // Remove orphan embeddings from every model database, not just one.
    // Each model is attached, swept, and detached in turn rather than joined
    // in a single statement: SQLite gives no atomic commit across attached
    // databases in WAL mode, so batching buys nothing, and a per-model count
    // is more useful output anyway.
    //
    // In dry-run mode the file_hashes rows were not deleted yet, so counts
    // reflect only pre-existing orphans and are a lower bound.
    let mut orphans = 0usize;
    for model_id in videre_core::embeddings_db::list_models(db).unwrap_or_default() {
        if videre_core::embeddings_db::attach(conn, db, &model_id, false).is_err() {
            continue;
        }
        let removed = if args.dry_run {
            conn.query_row(
                "SELECT COUNT(*) FROM emb.embeddings \
                 WHERE hash NOT IN (SELECT hash FROM file_hashes)",
                [],
                // rusqlite 0.40 dropped `FromSql for usize`; SQLite returns
                // i64, and the sibling branch (`conn.execute`) yields usize.
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n.max(0) as usize)
            .unwrap_or(0)
        } else {
            conn.execute(
                "DELETE FROM emb.embeddings \
                 WHERE hash NOT IN (SELECT hash FROM file_hashes)",
                [],
            )
            .unwrap_or(0)
        };
        let _ = videre_core::embeddings_db::detach(conn);
        if !args.silent && removed > 0 {
            eprintln!("removed {removed} orphan embedding(s) ({model_id})");
        }
        orphans += removed;
    }

    // Remove orphan thumbnail-cache files: any videre_core::thumb_cache entry
    // (240/1200px thumbnail, face crop, or full-res original) whose content
    // hash has no remaining file_hashes row. Same "shared-hash safety" as the
    // embeddings cleanup above, a hash survives here as long as any path
    // still references it, even if this specific path was just removed.
    // Dry-run count is a lower bound for the same reason as above (rows not
    // actually deleted yet). Skips `.tmp*` scratch files unconditionally (see
    // `hash_from_cache_filename`'s doc comment) so an in-flight write from a
    // concurrently running `videre watch` is never touched.
    let mut cache_orphans = 0usize;
    if let Ok(entries) = std::fs::read_dir(videre_core::thumb_cache::cache_dir()) {
        let live_hashes: std::collections::HashSet<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT hash FROM file_hashes")
                .expect("failed to prepare");
            stmt.query_map([], |r| r.get(0))
                .expect("failed to execute")
                .filter_map(|r| r.ok())
                .collect()
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(hash) = videre_core::thumb_cache::hash_from_cache_filename(&file_name) else {
                continue;
            };
            if live_hashes.contains(hash) {
                continue;
            }
            if args.dry_run {
                cache_orphans += 1;
            } else if std::fs::remove_file(entry.path()).is_ok() {
                cache_orphans += 1;
            } else {
                errors += 1;
            }
        }
    }

    if !args.silent {
        let action = if args.dry_run { "would be" } else { "were" };
        let orphan_note = if orphans > 0 {
            let qualifier = if args.dry_run {
                " (lower bound; actual may be higher after removals)"
            } else {
                ""
            };
            format!(", {orphans} orphan embedding(s) {action} pruned{qualifier}")
        } else {
            String::new()
        };
        let cache_note = if cache_orphans > 0 {
            let qualifier = if args.dry_run {
                " (lower bound; actual may be higher after removals)"
            } else {
                ""
            };
            format!(", {cache_orphans} orphan cache file(s) {action} pruned{qualifier}")
        } else {
            String::new()
        };
        eprintln!(
            "{total} row(s) checked: {removed} {action} removed, {synced} {action} synced, {errors} error(s){orphan_note}{cache_note}."
        );
    }

    // Printed even under --silent. A run that quietly skips thousands of rows
    // is exactly the silence this guard exists to end: the count is how a user
    // learns their drive was not mounted.
    if unreachable > 0 {
        eprintln!("{unreachable} row(s) skipped as unreachable{}", {
            let mut it = unreachable_dirs.iter();
            let shown: Vec<&String> = it.by_ref().take(MAX_REPORTED_DIRS).collect();
            let rest = unreachable_dirs.len().saturating_sub(shown.len());
            let more = if rest > 0 {
                format!(", and {rest} more")
            } else {
                String::new()
            };
            format!(
                " ({} director{} missing: {}{more})",
                unreachable_dirs.len(),
                if unreachable_dirs.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                shown
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        eprintln!("  run with --prune-unreachable to remove them anyway");
    }

    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unchanged row must not be synced. This is the whole of BUG:21: the
    /// classifier used to push a Sync for every file that existed, so the count
    /// meant "rows whose file exists" and a run never converged.
    #[test]
    fn an_identical_timestamp_is_not_a_sync() {
        let t = "2016-12-02T18:51:10+00:00";
        assert!(!needs_sync(Some(t), t));
    }

    /// The case the sync exists for. `fix-dates` rewrites mtimes from EXIF and
    /// deliberately leaves the database alone, pointing the user at prune to
    /// re-sync; those rows must be detected.
    #[test]
    fn a_row_fix_dates_changed_is_a_sync() {
        assert!(needs_sync(
            Some("2026-08-24T19:41:36.805406992+00:00"), // what scan recorded
            "2017-02-06T19:02:28+00:00",                 // what fix-dates set
        ));
    }

    /// A row that has never been synced always is one. Without this a NULL would
    /// compare unequal to itself by accident rather than by rule.
    #[test]
    fn a_null_stored_timestamp_is_always_a_sync() {
        assert!(needs_sync(None, "2016-12-02T18:51:10+00:00"));
    }

    /// `fix-dates` zeroes the sub-second part, since EXIF has one-second
    /// resolution, so a synced row and a freshly scanned one differ in exactly
    /// that. The comparison must not treat "same second" as equal.
    #[test]
    fn a_differing_sub_second_part_is_a_sync() {
        assert!(needs_sync(
            Some("2026-08-24T19:41:36.805406992+00:00"),
            "2026-08-24T19:41:36+00:00",
        ));
        // ...and full nanosecond precision still compares equal to itself, which
        // is what makes `watch --prune` quiet between real changes.
        let ns = "2026-08-24T19:41:36.805406992+00:00";
        assert!(!needs_sync(Some(ns), ns));
    }

    /// Timestamps that differ only in offset notation are different strings, and
    /// are treated as a difference. Sound because only one writer produces these:
    /// both sides come from `to_rfc3339()` on a `DateTime<Utc>`, which always
    /// renders `+00:00`. A test so that stays a deliberate property rather than
    /// something discovered when a second writer appears.
    #[test]
    fn the_comparison_is_textual_not_temporal() {
        assert!(needs_sync(
            Some("2016-12-02T18:51:10Z"),
            "2016-12-02T18:51:10+00:00",
        ));
    }

    /// `videre watch --prune` runs unattended on a loop and cannot ask, so
    /// neither override may be reachable from it. Cheap to assert, and it is
    /// exactly the regression that would silently re-enable unattended
    /// deletion of an unmounted drive's rows.
    #[test]
    fn watch_stage_cannot_override_either_guard() {
        for silent in [true, false] {
            let a = PruneArgs::for_watch_stage(silent);
            assert!(!a.prune_unreachable, "watch must never prune unreachable");
            assert!(!a.force, "watch must never bypass the bulk guard");
            assert!(!a.dry_run);
            assert_eq!(a.silent, silent);
        }
    }

    /// Both bulk-guard conditions must hold. Encoded as a test so the constants
    /// cannot drift into "percentage only", which would block small libraries
    /// where most files were legitimately deleted.
    #[test]
    fn the_bulk_guard_needs_both_a_fraction_and_a_floor() {
        let trips = |to_remove: usize, total: usize| {
            to_remove >= BULK_DELETE_MIN_ROWS
                && (to_remove as f64) > (total as f64) * BULK_DELETE_FRACTION
        };
        // 3 of 5 is 60%, way over the fraction, but far under the floor.
        assert!(!trips(3, 5), "small library must not be blocked");
        // 100 of 300 is 33%, over both.
        assert!(trips(100, 300), "a third of a real library must stop");
        // 100 of 10,000 is 1%: over the floor, under the fraction.
        assert!(!trips(100, 10_000), "routine cleanup must not be blocked");
    }
}
