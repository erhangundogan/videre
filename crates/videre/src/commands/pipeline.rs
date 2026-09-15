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

struct Outcome {
    stage: Stage,
    ran: bool,
    ok: bool,
    skipped: Option<&'static str>,
    duration_ms: Option<u64>,
}

/// Run one stage by calling that command's own `run`, so it keeps its own
/// connection, command lock, `pipeline_runs` row, progress and summary.
fn run_stage(stage: Stage, ctx: &CommandContext, silent: bool) -> anyhow::Result<()> {
    match stage {
        Stage::Scan => super::scan::run(super::scan::ScanArgs::for_pipeline(silent), ctx),
        Stage::Faces => super::faces::run(super::faces::FacesArgs::for_pipeline(silent), ctx),
        Stage::Embed => super::embed::run(super::embed::EmbedArgs::for_pipeline(silent), ctx),
        Stage::Classify => {
            super::classify::run(super::classify::ClassifyArgs::for_pipeline(silent), ctx)
        }
        Stage::Locations => {
            super::locations::run(super::locations::LocationsArgs::for_pipeline(silent), ctx)
        }
        Stage::FixDates => {
            super::fix_dates::run(super::fix_dates::FixDatesArgs::for_pipeline(silent), ctx)
        }
        Stage::Export => super::export::run(super::export::ExportArgs::for_pipeline(silent), ctx),
    }
}

fn indicator(o: &Outcome) -> char {
    if o.skipped.is_some() {
        '-'
    } else if !o.ok {
        '!'
    } else {
        '+'
    }
}

/// Prompt once before the expensive stages. Lists each heavy stage that has
/// outstanding work with its count and approximate cost.
fn confirm_heavy(report: &StatusReport) -> anyhow::Result<bool> {
    let parts: Vec<String> = report
        .coverage
        .iter()
        .filter(|c| c.heavy && c.outstanding > 0)
        .map(|c| {
            let cost = report
                .costs
                .iter()
                .find(|(s, _)| *s == c.stage)
                .and_then(|(_, e)| e.secs)
                .map(fmt_cost)
                .unwrap_or_default();
            format!("{} {} files ({})", c.stage, c.outstanding, cost)
        })
        .collect();
    super::confirm(&format!("About to {}. Proceed?", parts.join(" and ")))
}

fn render_resolved(outcomes: &[Outcome]) {
    for o in outcomes {
        let note = match (o.skipped, o.duration_ms) {
            (Some(reason), _) => format!("skipped ({reason})"),
            (None, Some(ms)) => videre_core::progress::human_duration_ms(ms),
            (None, None) => String::new(),
        };
        println!("  {} {:10} {}", indicator(o), o.stage.key(), note);
    }
}

#[derive(serde::Serialize)]
struct StageOutcomeJson {
    stage: &'static str,
    ran: bool,
    ok: bool,
    skipped: Option<&'static str>,
    duration_ms: Option<u64>,
}

#[derive(serde::Serialize)]
struct PipelineJson {
    stages: Vec<StageOutcomeJson>,
    failed: usize,
}

fn report_json(outcomes: &[Outcome], failed: usize) -> PipelineJson {
    PipelineJson {
        stages: outcomes
            .iter()
            .map(|o| StageOutcomeJson {
                stage: o.stage.key(),
                ran: o.ran,
                ok: o.ok,
                skipped: o.skipped,
                duration_ms: o.duration_ms,
            })
            .collect(),
        failed,
    }
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

    let stage_silent = args.silent || args.json;
    let mut outcomes: Vec<Outcome> = Vec::new();
    // Coverage is read once, after scan, so the plan reflects freshly scanned
    // files. Computed lazily on the first non-scan stage.
    let mut report: Option<StatusReport> = None;
    let mut asked_heavy = false;
    let mut heavy_declined = false;

    for stage in &stages {
        if *stage != Stage::Scan && report.is_none() {
            report = read_coverage(ctx).ok();
            if !args.silent && !args.json {
                render_plan(&stages, report.as_ref());
            }
        }

        // Skip a measured stage with nothing outstanding.
        if let Some(cov) = report.as_ref().and_then(|r| coverage_for(r, *stage)) {
            if cov.outstanding == 0 {
                outcomes.push(Outcome {
                    stage: *stage,
                    ran: false,
                    ok: true,
                    skipped: Some("up to date"),
                    duration_ms: None,
                });
                continue;
            }
            // One confirmation before the first heavy stage (embed/classify)
            // with outstanding work. `--yes` bypasses it; declining skips every
            // heavy stage and the run continues with the rest. The prompt fires
            // before any model load, so declining downloads nothing.
            if cov.heavy {
                if !args.yes && !asked_heavy {
                    asked_heavy = true;
                    heavy_declined = report
                        .as_ref()
                        .map(confirm_heavy)
                        .transpose()?
                        .map(|proceed| !proceed)
                        .unwrap_or(false);
                }
                if heavy_declined {
                    outcomes.push(Outcome {
                        stage: *stage,
                        ran: false,
                        ok: true,
                        skipped: Some("declined"),
                        duration_ms: None,
                    });
                    continue;
                }
            }
        }

        let started = Instant::now();
        match run_stage(*stage, ctx, stage_silent) {
            Ok(()) => outcomes.push(Outcome {
                stage: *stage,
                ran: true,
                ok: true,
                skipped: None,
                duration_ms: Some(started.elapsed().as_millis() as u64),
            }),
            Err(e) => {
                eprintln!("videre pipeline: {} stage error: {e:#}", stage.key());
                outcomes.push(Outcome {
                    stage: *stage,
                    ran: true,
                    ok: false,
                    skipped: None,
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                });
            }
        }
    }

    let failed = outcomes.iter().filter(|o| o.ran && !o.ok).count();
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&report_json(&outcomes, failed))?
        );
    } else if !args.silent {
        render_resolved(&outcomes);
    }
    if failed > 0 {
        anyhow::bail!("pipeline: {failed} stage(s) failed");
    }
    Ok(())
}
