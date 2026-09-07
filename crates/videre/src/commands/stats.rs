use crate::command_context::CommandContext;
use std::process;
use videre::types::{ErrorJson, StatsJson, SCHEMA_VERSION};
use videre_core::pipeline_runs::PipelineRunStatus;

#[derive(clap::Args)]
pub struct StatsArgs {
    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,

    /// Exit non-zero if any tracked command's last run is "failed" or
    /// "crashed" (a running row whose lock is no longer held by a live
    /// process). Output is unchanged either way, this only adds an exit
    /// code, so `videre stats --check` composes with cron/launchd's own
    /// failure handling without needing to parse text or JSON output.
    #[arg(long)]
    check: bool,
}

/// True if any tracked command's last recorded run needs attention.
/// "interrupted" (a clean Ctrl-C) is deliberately not included, that's an
/// intentional stop, not a failure.
fn has_problem(pipelines: &[PipelineRunStatus]) -> bool {
    pipelines
        .iter()
        .any(|p| matches!(p.status.as_deref(), Some("failed") | Some("crashed")))
}

pub fn run(args: StatsArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args, ctx) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                if args.check && has_problem(&doc.pipelines) {
                    process::exit(1);
                }
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                process::exit(1);
            }
        }
    } else {
        run_text(&args, ctx)
    }
}

fn run_text(args: &StatsArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let library = videre_core::library_stats::compute_full_in(&conn, &ctx.library)?;
    let pipelines = videre_core::pipeline_runs::read_all_in(&conn, &ctx.library)?;

    println!(
        "Library: {} file(s) ({}), {} photo(s), {} video(s)",
        library.total_files,
        videre_core::disk::human_bytes(library.total_size_bytes.max(0) as u64),
        library.total_photos,
        library.total_videos,
    );
    println!(
        "Duplicates: {} group(s), {} file(s), {} wasted",
        library.duplicate_group_count,
        library.duplicate_file_count,
        videre_core::disk::human_bytes(library.wasted_bytes.max(0) as u64),
    );
    println!(
        "Faces: {} detected, {} people named",
        library.faces_detected, library.people_named
    );
    println!(
        "Marks: {} rated, {} picked, {} labelled, {} liked",
        library.marks.rated, library.marks.picked, library.marks.labelled, library.marks.liked
    );
    println!();
    println!("Embeddings:");
    if library.embeddings.is_empty() {
        println!("  none; run 'videre embed' to create some");
    } else {
        for e in &library.embeddings {
            println!(
                "  {:38} {:>8} {:>5}-dim {:>10}",
                e.model_id,
                e.count,
                e.dims,
                videre_core::disk::human_bytes(e.size_bytes.max(0) as u64),
            );
        }
    }
    println!();
    println!("By type:");
    let types = videre_core::library_stats::by_type(&conn, 12)?;
    if types.is_empty() {
        println!("  nothing scanned yet");
    } else {
        for ty in &types {
            println!(
                "  {:8} {:24} {:>8} {:>10}",
                ty.ext,
                ty.mime,
                ty.files,
                videre_core::disk::human_bytes(ty.bytes.max(0) as u64),
            );
        }
    }

    println!();
    println!("Disk use:");
    // Every location is derived from the selected library context, so a run
    // reports only that library's own database, embeddings and locks plus the
    // caches it uses, never another library's vectors or thumbnails.
    let usage = videre_core::disk::usage_in(&ctx.library);
    if usage.is_empty() {
        println!("  nothing stored yet");
    } else {
        let total: u64 = usage.iter().map(|u| u.bytes).sum();
        let rebuildable: u64 = usage
            .iter()
            .filter(|u| u.rebuildable)
            .map(|u| u.bytes)
            .sum();
        for u in &usage {
            println!(
                "  {:18} {:>10}  {}",
                u.label,
                videre_core::disk::human_bytes(u.bytes),
                if u.rebuildable { "(rebuildable)" } else { "" },
            );
        }
        println!(
            "  {:18} {:>10}  ({} of it rebuildable)",
            "total",
            videre_core::disk::human_bytes(total),
            videre_core::disk::human_bytes(rebuildable),
        );
    }

    println!();
    println!("Pipeline status:");
    for p in &pipelines {
        let last_run = p.last_run_at.as_deref().unwrap_or("never run");
        let status = p.status.as_deref().unwrap_or("-");
        let duration = p
            .duration_ms
            .map(|d| videre_core::progress::human_duration_ms(d as u64))
            .unwrap_or_else(|| "-".to_string());
        let running_note = if p.currently_running {
            "  (running now)"
        } else {
            ""
        };
        println!(
            "  {:10} {:12} last_run={:<20} duration={:<8}{}",
            p.command, status, last_run, duration, running_note
        );
    }
    if args.check && has_problem(&pipelines) {
        process::exit(1);
    }
    Ok(())
}

fn run_json(args: &StatsArgs, ctx: &CommandContext) -> anyhow::Result<StatsJson> {
    let _ = args;
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let library = videre_core::library_stats::compute_full_in(&conn, &ctx.library)?;
    let pipelines = videre_core::pipeline_runs::read_all_in(&conn, &ctx.library)?;
    Ok(StatsJson {
        schema_version: SCHEMA_VERSION,
        library,
        pipelines,
    })
}
