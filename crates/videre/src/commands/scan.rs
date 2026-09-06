use crate::command_context::CommandContext;
use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::HashSet;
use std::process;
use videre::{
    hasher, scanner, sqlite_output,
    types::{ErrorJson, ScanJson, ScanOutputJson, SCHEMA_VERSION},
};

#[derive(clap::Args)]
pub struct ScanArgs {
    /// Also compute and store perceptual hashes for near-duplicate detection
    #[arg(long)]
    similar: bool,

    /// Only process files no scan has finished
    #[arg(long)]
    retry_incomplete: bool,

    /// Suppress progress output on stderr
    #[arg(long)]
    silent: bool,

    #[command(flatten)]
    xmp: crate::xmp::XmpArg,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

pub fn run(args: ScanArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    if args.json {
        match run_inner(&args, ctx) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(error) => report_json_error(error),
        }
    } else {
        run_inner(&args, ctx).map(|_| ())
    }
}

pub fn report_startup_error(args: ScanArgs, error: anyhow::Error) -> anyhow::Result<()> {
    if args.json {
        report_json_error(error)
    } else {
        Err(error)
    }
}

fn report_json_error(error: anyhow::Error) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string(&ErrorJson::from_err(&error))?);
    process::exit(1);
}

fn run_inner(args: &ScanArgs, ctx: &CommandContext) -> anyhow::Result<ScanJson> {
    let conn = videre_core::library_db::initialize(&ctx.library)?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "scan")?;
    if let Err(error) =
        videre_core::pipeline_runs::install_sigint_handler_in(ctx.library.clone(), "scan")
    {
        eprintln!("Warning: could not install interrupt handler: {error:#}");
    }

    let (records, skipped, walked) =
        videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "scan", || {
            let completed = completed_paths(&conn, args)?;
            let (records, skipped, walked) = gather_records(args, ctx, &completed);
            sqlite_output::write_records_in(&conn, &ctx.library, &records)?;
            let precedence = args.xmp.resolve_from(&ctx.library.settings)?;
            crate::xmp::import_xmp_for_records_in(
                &conn,
                &ctx.library,
                &records,
                precedence,
                args.silent,
            )?;
            Ok((records, skipped, walked))
        })?;

    if args.retry_incomplete && !args.silent {
        eprintln!("{}", format_retry_summary(walked, &records, skipped));
    }
    if !args.silent {
        eprintln!(
            "{}",
            format_write_summary(
                records.len(),
                skipped,
                &format!("{:?}", ctx.library.paths.db)
            )
        );
    }

    Ok(ScanJson {
        schema_version: SCHEMA_VERSION,
        total_files: records.len(),
        output: ScanOutputJson {
            kind: "sqlite",
            path: ctx.library.paths.db.display().to_string(),
        },
    })
}

fn completed_paths(conn: &Connection, args: &ScanArgs) -> anyhow::Result<HashSet<String>> {
    if args.retry_incomplete {
        Ok(videre_core::db::paths_with_known_mime(conn)?)
    } else {
        Ok(HashSet::new())
    }
}

fn gather_records(
    args: &ScanArgs,
    ctx: &CommandContext,
    completed: &HashSet<String>,
) -> (Vec<videre::types::FileRecord>, usize, usize) {
    let all_paths = scanner::scan(&ctx.library.paths.root);
    let walked = all_paths.len();
    let paths: Vec<_> = all_paths
        .into_iter()
        .filter(|path| !completed.contains(path.to_string_lossy().as_ref()))
        .collect();
    let progress = videre_core::progress::Progress::new(paths.len() as u64, args.silent);

    let records: Vec<_> = paths
        .par_iter()
        .filter_map(|path| {
            let result = hasher::hash_file_in(&ctx.library, path)
                .map_err(|error| {
                    progress.println(&format!("Warning: skipping {:?}: {error}", path));
                })
                .ok();
            progress.tick();
            result
        })
        .collect();
    progress.finish();
    let skipped = paths.len() - records.len();

    let records = if args.similar {
        if !args.silent {
            eprintln!("Computing perceptual hashes for {} file(s)", records.len());
        }
        apply_phashes(ctx, records, args.silent)
    } else {
        records
    };
    (records, skipped, walked)
}

fn apply_phashes(
    ctx: &CommandContext,
    records: Vec<videre::types::FileRecord>,
    silent: bool,
) -> Vec<videre::types::FileRecord> {
    let progress = videre_core::progress::Progress::new(records.len() as u64, silent);
    let out = records
        .into_par_iter()
        .map(|mut record| {
            record.phash = hasher::compute_dhash_in(
                &ctx.library,
                std::path::Path::new(&record.path),
                record.mime.as_deref(),
            );
            progress.tick();
            record
        })
        .collect();
    progress.finish();
    out
}

fn format_retry_summary(
    walked: usize,
    records: &[videre::types::FileRecord],
    skipped: usize,
) -> String {
    let unresolved = records
        .iter()
        .filter(|record| record.mime.as_deref() == Some(videre_core::mime_probe::UNKNOWN_MIME))
        .count();
    format!(
        "{walked} file(s) walked, {} incomplete; {} processed, {} identified, {unresolved} still unrecognised",
        records.len() + skipped,
        records.len(),
        records.len() - unresolved,
    )
}

fn format_write_summary(written: usize, skipped: usize, destination: &str) -> String {
    if skipped > 0 {
        format!("Wrote {written} record(s) to {destination} ({skipped} skipped)")
    } else {
        format!("Wrote {written} record(s) to {destination}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_summary_reports_identified_and_unrecognised_records() {
        let records = vec![videre::types::FileRecord {
            path: "/a.jpg".into(),
            hash: "hash".into(),
            size_bytes: 1,
            created_at: None,
            modified_at: None,
            ext: "jpg".into(),
            mime: Some(videre_core::mime_probe::UNKNOWN_MIME.into()),
            phash: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
            duration_secs: None,
            codec: None,
        }];
        assert_eq!(
            format_retry_summary(2, &records, 1),
            "2 file(s) walked, 2 incomplete; 1 processed, 0 identified, 1 still unrecognised"
        );
    }
}
