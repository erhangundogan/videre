use videre::types::{ErrorJson, StatsJson, SCHEMA_VERSION};
use std::path::PathBuf;
use std::process;

#[derive(clap::Args)]
pub struct StatsArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

pub fn run(args: StatsArgs) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                process::exit(1);
            }
        }
    } else {
        run_text(&args)
    }
}

fn resolve_and_open(args: &StatsArgs) -> anyhow::Result<(std::path::PathBuf, rusqlite::Connection)> {
    let db = super::resolve_reader_db_must_exist(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)?;
    Ok((db, conn))
}

fn run_text(args: &StatsArgs) -> anyhow::Result<()> {
    let (db, conn) = resolve_and_open(args)?;
    let library = videre_core::library_stats::compute(&conn)?;
    let pipelines = videre_core::pipeline_runs::read_all(&conn, &db)?;

    println!(
        "Library: {} file(s) ({}), {} photo(s), {} video(s)",
        library.total_files,
        super::report::format_bytes(library.total_size_bytes),
        library.total_photos,
        library.total_videos,
    );
    println!(
        "Duplicates: {} group(s), {} file(s), {} wasted",
        library.duplicate_group_count,
        library.duplicate_file_count,
        super::report::format_bytes(library.wasted_bytes),
    );
    println!(
        "Faces: {} detected, {} people named",
        library.faces_detected, library.people_named
    );
    println!();
    println!("Pipeline status:");
    for p in &pipelines {
        let last_run = p.last_run_at.as_deref().unwrap_or("never run");
        let status = p.status.as_deref().unwrap_or("-");
        let duration = p
            .duration_ms
            .map(|d| format!("{d}ms"))
            .unwrap_or_else(|| "-".to_string());
        let running_note = if p.currently_running { "  (running now)" } else { "" };
        println!(
            "  {:10} {:12} last_run={:<20} duration={:<8}{}",
            p.command, status, last_run, duration, running_note
        );
    }
    Ok(())
}

fn run_json(args: &StatsArgs) -> anyhow::Result<StatsJson> {
    let (db, conn) = resolve_and_open(args)?;
    let library = videre_core::library_stats::compute(&conn)?;
    let pipelines = videre_core::pipeline_runs::read_all(&conn, &db)?;
    Ok(StatsJson { schema_version: SCHEMA_VERSION, library, pipelines })
}
