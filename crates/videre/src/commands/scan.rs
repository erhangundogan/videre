use crate::command_context::CommandContext;
use rayon::prelude::*;
use rusqlite::Connection;
use videre::{
    hasher, scanner, sqlite_output,
    types::{ErrorJson, ScanJson, ScanOutputJson, SCHEMA_VERSION},
};

#[derive(clap::Args)]
pub struct ScanArgs {
    /// Re-read and re-hash every file, ignoring the unchanged-since-last-scan
    /// skip. The honest full pass (and what a future integrity check wants).
    #[arg(long)]
    force: bool,

    /// Suppress progress output on stderr
    #[arg(long)]
    silent: bool,

    #[command(flatten)]
    xmp: crate::xmp::XmpArg,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

impl ScanArgs {
    /// Pipeline-stage defaults: clap defaults for every knob, silence
    /// controlled by the pipeline. Parsing an empty argv keeps defaults from
    /// drifting from the flag definitions.
    pub(crate) fn for_pipeline(silent: bool) -> Self {
        #[derive(clap::Parser)]
        struct P {
            #[command(flatten)]
            a: ScanArgs,
        }
        let argv: &[&str] = if silent {
            &["scan", "--silent"]
        } else {
            &["scan"]
        };
        <P as clap::Parser>::parse_from(argv).a
    }

    /// Whether failures are reported as a JSON document on stdout.
    pub(crate) fn json(&self) -> bool {
        self.json
    }
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

pub fn report_startup_error(json: bool, error: anyhow::Error) -> anyhow::Result<()> {
    if json {
        report_json_error(error)
    } else {
        Err(error)
    }
}

/// `--json` carries its failure on stdout as one error document; the error is
/// still logged, but not printed to stderr a second time.
fn report_json_error(error: anyhow::Error) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string(&ErrorJson::from_err(&error))?);
    Err(crate::exit::Exit::shown(error).into())
}

fn run_inner(args: &ScanArgs, ctx: &CommandContext) -> anyhow::Result<ScanJson> {
    let conn = videre_core::library_db::initialize(&ctx.library)?;
    // initialize has already done any exclusive schema preparation and released
    // its init lock; the scan itself is ordinary shared work, coexisting with
    // readers and excluded only by exclusive maintenance.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "scan")?;
    if let Err(error) =
        videre_core::pipeline_runs::install_sigint_handler_in(ctx.library.clone(), "scan")
    {
        tracing::warn!("could not install interrupt handler: {error:#}");
    }

    let (records, skipped) =
        videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "scan", || {
            let (records, skipped, _walked) = gather_records(args, ctx, &conn);
            sqlite_output::write_records_in(&conn, &ctx.library, &records)?;
            // Rows written before capture dates get theirs from what they
            // already hold; nothing is hashed again.
            let resolved = sqlite_output::resolve_unresolved(&conn)?;
            if resolved > 0 && !args.silent {
                tracing::info!("Resolved the capture date of {resolved} existing file(s)");
            }
            let precedence = args.xmp.resolve_from(&ctx.library.settings)?;
            // Reconcile XMP incrementally: files this run hashed get a full
            // reconcile, files whose sidecar changed get a sidecar-only read,
            // everything else is skipped. `--xmp file`/`newest` still reconcile
            // every row so the revert semantics hold.
            let changed: std::collections::HashSet<String> =
                records.iter().map(|r| r.path.clone()).collect();
            crate::xmp::reconcile_xmp_in(&conn, &ctx.library, precedence, &changed, args.silent)?;
            Ok((records, skipped))
        })?;

    if !args.silent {
        tracing::info!(
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

fn gather_records(
    args: &ScanArgs,
    ctx: &CommandContext,
    conn: &Connection,
) -> (Vec<videre::types::FileRecord>, usize, usize) {
    // Incremental by default: skip a file whose row is already current. `--force`
    // loads no signatures, so everything is reprocessed. A file the walk sees
    // but cannot stat, and any new path, falls through to the hash path.
    let sigs = if args.force {
        std::collections::HashMap::new()
    } else {
        videre_core::db::stored_signatures(conn).unwrap_or_default()
    };
    let all_paths = scanner::scan(&ctx.library.paths.root);
    let walked = all_paths.len();
    let paths: Vec<_> = all_paths
        .into_iter()
        .filter(|path| videre::incremental::needs_processing(&sigs, path))
        .collect();
    let progress = videre_core::progress::Progress::new(paths.len() as u64, args.silent);

    let records: Vec<_> = paths
        .par_iter()
        .filter_map(|path| {
            // Tracked while it hashes, so a large file shows its bytes beside
            // the bar instead of the counter sitting still for minutes.
            let handle = videre_core::io_timeout::ProgressHandle::new();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let tracked = progress.track(&name, handle.clone());
            let hashed = hasher::hash_file_in_observed(&ctx.library, path, &handle);
            drop(tracked);
            let result = record_or_skip(hashed, &progress, &path.display().to_string());
            progress.tick();
            result
        })
        .collect();
    progress.finish();
    let skipped = paths.len() - records.len();

    (records, skipped, walked)
}

fn record_or_skip<T>(
    result: std::io::Result<T>,
    progress: &videre_core::progress::Progress,
    path: &str,
) -> Option<T> {
    match result {
        Ok(record) => Some(record),
        Err(error) => {
            progress.skip(path, videre_core::error_kind::from_io(error));
            None
        }
    }
}

fn format_write_summary(written: usize, skipped: usize, destination: &str) -> String {
    if skipped > 0 {
        format!("Wrote {written} record(s) to {destination} ({skipped} skipped)")
    } else {
        format!("Wrote {written} record(s) to {destination}")
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn capacity_refusal_is_skipped_without_a_record() {
        let progress = videre_core::progress::Progress::new(1, true);
        let error = videre_core::io_timeout::IoRunError::Capacity {
            active: 1,
            limit: 1,
        }
        .into_io_error();
        let result: Option<u8> = record_or_skip(Err(error), &progress, "photo.jpg");
        assert!(result.is_none());
    }
}
