use crate::command_context::CommandContext;
use std::time::Instant;
use videre_core::status_report::{StageCoverage, StatusReport};

#[derive(clap::Args)]
pub struct PipelineArgs {
    /// Skip one or more stages. Repeatable and comma-separated:
    /// `--skip embed,faces` or `--skip embed --skip faces`.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "STAGE",
        value_parser = ["scan", "faces", "embed", "classify", "locations", "fix-dates", "export"]
    )]
    skip: Vec<String>,

    /// Also run fix-dates. Opt-in: it rewrites existing date metadata.
    #[arg(long)]
    fix_dates: bool,

    /// Also write XMP sidecars at the end. Opt-in.
    #[arg(long)]
    export: bool,

    /// Proceed through the expensive stages (embed, classify) without asking.
    #[arg(short = 'y', long = "yes")]
    yes: bool,

    /// Print the plan and estimated cost, then exit without doing any work.
    #[arg(long)]
    dry_run: bool,

    /// Emit a single JSON object on stdout instead of the human checklist.
    #[arg(long)]
    json: bool,

    /// Suppress the checklist and per-stage progress (errors always shown).
    #[arg(long)]
    silent: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Scan,
    Faces,
    Embed,
    Classify,
    Locations,
    FixDates,
    Export,
}

impl Stage {
    /// Matches both the `--skip` names and the `status_report` coverage keys.
    fn key(self) -> &'static str {
        match self {
            Stage::Scan => "scan",
            Stage::Faces => "faces",
            Stage::Embed => "embed",
            Stage::Classify => "classify",
            Stage::Locations => "locations",
            Stage::FixDates => "fix-dates",
            Stage::Export => "export",
        }
    }
}

/// The ordered stage list for this invocation: the default correctness stages,
/// plus the opt-ins where enabled, minus anything skipped. scan first (it is
/// the change detector and initializer); embed precedes classify because
/// classify reads embeddings.
fn planned_stages(args: &PipelineArgs) -> Vec<Stage> {
    let mut stages = vec![Stage::Scan];
    if args.fix_dates {
        stages.push(Stage::FixDates);
    }
    stages.extend([
        Stage::Faces,
        Stage::Embed,
        Stage::Classify,
        Stage::Locations,
    ]);
    if args.export {
        stages.push(Stage::Export);
    }
    stages.retain(|s| !args.skip.iter().any(|k| k == s.key()));
    stages
}

/// Read-only coverage snapshot for the plan. Opens its own connection under a
/// shared activity lease and drops both before returning, so the stages that
/// follow can take their own leases and command locks.
fn read_coverage(ctx: &CommandContext) -> anyhow::Result<StatusReport> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    videre_core::embeddings_db::attach_for_read_or_placeholder_in(
        &conn,
        &ctx.library,
        &ctx.library.settings.default_model,
    )?;
    videre_core::status_report::compute_status_in(&conn, &ctx.library)
}

/// One coverage entry for a stage, if the shared model measures it. scan and
/// export have no outstanding-vs-done count.
fn coverage_for(report: &StatusReport, stage: Stage) -> Option<&StageCoverage> {
    report.coverage.iter().find(|c| c.stage == stage.key())
}

/// Approximate duration, matching the `videre status` cost wording.
fn fmt_cost(secs: u64) -> String {
    if secs >= 3600 {
        format!("~{:.1}h", secs as f64 / 3600.0)
    } else if secs >= 60 {
        format!("~{}m", secs / 60)
    } else {
        format!("~{secs}s")
    }
}

/// The initial plan: every planned stage from the top, each with a pending
/// indicator and, where the model measures it, its outstanding count + cost.
fn render_plan(stages: &[Stage], report: Option<&StatusReport>) {
    println!("videre pipeline  ({} stages)\n", stages.len());
    for stage in stages {
        let cov = report.and_then(|r| coverage_for(r, *stage));
        match cov {
            Some(c) if c.outstanding > 0 => {
                let cost = report
                    .and_then(|r| r.costs.iter().find(|(s, _)| *s == c.stage))
                    .and_then(|(_, e)| e.secs)
                    .map(fmt_cost)
                    .unwrap_or_default();
                let flag = if c.heavy { "  needs confirm" } else { "" };
                println!(
                    "  o {:10} {} outstanding   {}{}",
                    c.stage, c.outstanding, cost, flag
                );
            }
            Some(c) => println!("  o {:10} up to date", c.stage),
            None => println!("  o {:10}", stage.key()),
        }
    }
    println!();
}

pub fn run(args: PipelineArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let stages = planned_stages(&args);

    if args.dry_run {
        let report = read_coverage(ctx).ok();
        if args.json {
            let plan: Vec<_> = stages.iter().map(|s| s.key()).collect();
            println!("{}", serde_json::json!({ "dry_run": true, "stages": plan }));
        } else {
            render_plan(&stages, report.as_ref());
            println!("dry run: no work performed");
        }
        return Ok(());
    }

    // Execution is added in later tasks. For now a non-dry-run prints the plan
    // and stops, so the command is wired end to end without doing work yet.
    let report = read_coverage(ctx).ok();
    render_plan(&stages, report.as_ref());
    let _ = Instant::now();
    Ok(())
}
