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
}

impl PruneArgs {
    /// Constructs args for an in-process prune pass against an already-open
    /// connection (used by `videre watch --prune`) - `db`/`dry_run` aren't
    /// meaningful there since the caller already has a connection and always
    /// wants the real (non-preview) pass, so only `silent` is exposed.
    pub(crate) fn for_watch_stage(silent: bool) -> Self {
        Self { db: None, dry_run: false, silent }
    }
}

fn system_time_to_iso(t: SystemTime) -> String {
    let dt: DateTime<Utc> = t.into();
    dt.to_rfc3339()
}

fn embeddings_table_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embeddings'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
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

    let errors = videre_core::pipeline_runs::track(&conn, &db, "prune", || run_prune(&args, &conn))?;

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
pub(crate) fn run_prune(args: &PruneArgs, conn: &Connection) -> anyhow::Result<usize> {
    let paths: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT path FROM file_hashes ORDER BY path")
            .expect("failed to prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("failed to execute")
            .filter_map(|r| r.ok())
            .collect()
    };

    let total = paths.len();
    let mut removed = 0usize;
    let mut synced = 0usize;
    let mut errors = 0usize;

    for path in &paths {
        match std::fs::metadata(path) {
            Err(_) => {
                if !args.silent {
                    let tag = if args.dry_run { "[dry-run] would remove" } else { "[removed]" };
                    println!("{tag} {path}");
                }
                if !args.dry_run {
                    if let Err(e) =
                        conn.execute("DELETE FROM file_hashes WHERE path = ?1", rusqlite::params![path])
                    {
                        eprintln!("Error removing {path}: {e}");
                        errors += 1;
                        continue;
                    }
                }
                removed += 1;
            }
            Ok(meta) => {
                let mtime = match meta.modified() {
                    Ok(t) => system_time_to_iso(t),
                    Err(e) => {
                        eprintln!("Error reading mtime for {path}: {e}");
                        errors += 1;
                        continue;
                    }
                };
                if !args.dry_run {
                    if let Err(e) = conn.execute(
                        "UPDATE file_hashes SET modified_at = ?1 WHERE path = ?2",
                        rusqlite::params![mtime, path],
                    ) {
                        eprintln!("Error syncing {path}: {e}");
                        errors += 1;
                        continue;
                    }
                }
                if !args.silent {
                    let tag = if args.dry_run { "[dry-run] would sync" } else { "[synced]" };
                    println!("{tag} {path}  modified_at -> {mtime}");
                }
                synced += 1;
            }
        }
    }

    // Remove orphan embeddings: hashes with no remaining file_hashes row.
    // In dry-run mode the file_hashes rows were not deleted yet, so the count
    // reflects only pre-existing orphans and is a lower bound.
    let orphans = if embeddings_table_exists(conn) {
        if args.dry_run {
            conn.query_row(
                "SELECT COUNT(*) FROM embeddings \
                 WHERE hash NOT IN (SELECT hash FROM file_hashes)",
                [],
                |r| r.get::<_, usize>(0),
            )
            .unwrap_or(0)
        } else {
            conn.execute(
                "DELETE FROM embeddings \
                 WHERE hash NOT IN (SELECT hash FROM file_hashes)",
                [],
            )
            .unwrap_or(0)
        }
    } else {
        0
    };

    // Remove orphan thumbnail-cache files: any videre_core::thumb_cache entry
    // (240/1200px thumbnail, face crop, or full-res original) whose content
    // hash has no remaining file_hashes row. Same "shared-hash safety" as the
    // embeddings cleanup above - a hash survives here as long as any path
    // still references it, even if this specific path was just removed.
    // Dry-run count is a lower bound for the same reason as above (rows not
    // actually deleted yet). Skips `.tmp*` scratch files unconditionally (see
    // `hash_from_cache_filename`'s doc comment) so an in-flight write from a
    // concurrently running `videre watch` is never touched.
    let mut cache_orphans = 0usize;
    if let Ok(entries) = std::fs::read_dir(videre_core::thumb_cache::cache_dir()) {
        let live_hashes: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("SELECT DISTINCT hash FROM file_hashes").expect("failed to prepare");
            stmt.query_map([], |r| r.get(0))
                .expect("failed to execute")
                .filter_map(|r| r.ok())
                .collect()
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let Some(file_name) = entry.file_name().to_str().map(str::to_string) else { continue };
            let Some(hash) = videre_core::thumb_cache::hash_from_cache_filename(&file_name) else { continue };
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
            let qualifier = if args.dry_run { " (lower bound; actual may be higher after removals)" } else { "" };
            format!(", {orphans} orphan embedding(s) {action} pruned{qualifier}")
        } else {
            String::new()
        };
        let cache_note = if cache_orphans > 0 {
            let qualifier = if args.dry_run { " (lower bound; actual may be higher after removals)" } else { "" };
            format!(", {cache_orphans} orphan cache file(s) {action} pruned{qualifier}")
        } else {
            String::new()
        };
        eprintln!(
            "{total} row(s) checked: {removed} {action} removed, {synced} {action} synced, {errors} error(s){orphan_note}{cache_note}."
        );
    }

    Ok(errors)
}
