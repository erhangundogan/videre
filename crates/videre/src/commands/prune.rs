use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(clap::Args)]
pub struct PruneArgs {
    /// SQLite database produced by: videre dedupe --output-sqlite <db>
    db: PathBuf,

    /// Preview changes without modifying the database
    #[arg(long)]
    dry_run: bool,

    /// Suppress per-file output (errors are always shown)
    #[arg(long)]
    silent: bool,
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
    if !args.db.exists() {
        eprintln!("Error: {:?} does not exist", args.db);
        std::process::exit(1);
    }

    if args.dry_run && !args.silent {
        eprintln!("Dry run: no changes will be made to the database.");
    }

    let conn = videre_core::db::open_wal(&args.db).expect("failed to open database");

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
    let orphans = if embeddings_table_exists(&conn) {
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

    if !args.silent {
        let action = if args.dry_run { "would be" } else { "were" };
        let orphan_note = if orphans > 0 {
            let qualifier = if args.dry_run { " (lower bound; actual may be higher after removals)" } else { "" };
            format!(", {orphans} orphan embedding(s) {action} pruned{qualifier}")
        } else {
            String::new()
        };
        eprintln!(
            "{total} row(s) checked: {removed} {action} removed, {synced} {action} synced, {errors} error(s){orphan_note}."
        );
    }

    if errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}
