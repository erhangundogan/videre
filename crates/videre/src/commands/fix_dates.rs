use filetime::FileTime;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct FixDatesArgs {
    /// SQLite database produced by: videre dedupe --output-sqlite <db>
    db: PathBuf,

    /// Preview changes without modifying any files
    #[arg(long)]
    dry_run: bool,

    /// Suppress per-file output (errors are always shown)
    #[arg(long)]
    silent: bool,
}

pub fn run(args: FixDatesArgs) -> anyhow::Result<()> {
    if !args.db.exists() {
        eprintln!("Error: {:?} does not exist", args.db);
        std::process::exit(1);
    }

    if args.dry_run && !args.silent {
        eprintln!("Dry run: no files will be modified.");
    }

    let conn = videre_core::db::open_wal(&args.db).expect("failed to open database");

    let mut stmt = conn
        .prepare(
            "SELECT path, exif_date FROM file_hashes \
             WHERE exif_date IS NOT NULL \
             ORDER BY path",
        )
        .expect("failed to prepare query");

    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("failed to execute query")
        .filter_map(|r| r.ok())
        .collect();

    let total = rows.len();
    let mut changed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for (path, exif_date) in &rows {
        // Parse exif_date: "YYYY-MM-DDTHH:MM:SS" camera-local, no timezone.
        // Treat as local time when converting to a UNIX timestamp.
        let ndt = match chrono::NaiveDateTime::parse_from_str(exif_date, "%Y-%m-%dT%H:%M:%S") {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error: {path}: bad exif_date {exif_date:?}: {e}");
                errors += 1;
                continue;
            }
        };

        use chrono::TimeZone;
        let local_dt = match chrono::Local.from_local_datetime(&ndt).single() {
            Some(d) => d,
            None => {
                eprintln!("Error: {path}: ambiguous local time for {exif_date}");
                errors += 1;
                continue;
            }
        };

        let ft = FileTime::from_unix_time(local_dt.timestamp(), 0);

        if !args.dry_run {
            if let Err(e) = filetime::set_file_mtime(path, ft) {
                if e.kind() == std::io::ErrorKind::NotFound {
                    // File was trashed or moved after the scan; skip silently.
                    skipped += 1;
                    continue;
                }
                eprintln!("Error: {path}: {e}");
                errors += 1;
                continue;
            }
        }

        if !args.silent {
            let prefix = if args.dry_run { "[dry-run]" } else { "[updated]" };
            println!("{prefix} {path}  →  {exif_date}");
        }
        changed += 1;
    }

    if !args.silent {
        let skipped_note = if skipped > 0 {
            format!(", {skipped} no longer on disk (skipped)")
        } else {
            String::new()
        };
        eprintln!(
            "{} file(s) with exif_date, {} {}, {} error(s){}.",
            total,
            changed,
            if args.dry_run { "would be updated" } else { "updated" },
            errors,
            skipped_note,
        );
    }

    if errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}
