use crate::command_context::CommandContext;
use videre::types::{ErrorJson, StatusJson, SCHEMA_VERSION};
use videre_core::status_report::StatusReport;

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,

    /// Exit non-zero if any tracked command's last run is "failed" or
    /// "crashed" (a running row whose lock is no longer held by a live
    /// process), or the latest run of any command logged an error.
    /// Warnings and staleness are deliberately never a failure: a library
    /// that has not embedded anything yet is mid-setup, not broken. Output
    /// is unchanged either way.
    #[arg(long)]
    check: bool,
}

pub fn run(args: StatusArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    if args.json {
        match run_json(ctx) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                if args.check && doc.report.has_problem() {
                    return Err(crate::exit::Exit::code(1).into());
                }
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                Err(crate::exit::Exit::shown(e).into())
            }
        }
    } else {
        run_text(&args, ctx)
    }
}

fn run_text(args: &StatusArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    // A never-embedded library has no model database; the placeholder attach
    // lets the shared coverage queries read it as "zero embeddings" instead
    // of failing, without creating anything on disk.
    videre_core::embeddings_db::attach_for_read_or_placeholder_in(
        &conn,
        &ctx.library,
        &ctx.library.settings.default_model,
    )?;
    let report = videre_core::status_report::compute_status_in(&conn, &ctx.library)?;

    println!("Coverage (model {}):", report.embed_model);
    for c in &report.coverage {
        // total = done + outstanding + skipped, so done must subtract both;
        // skipped files are undecodable, not done.
        let done = c.total - c.outstanding - c.skipped;
        // Files the stage has given up decoding are not work it will do, so they
        // are named separately rather than folded into outstanding.
        let skipped_note = if c.skipped > 0 {
            format!(", {} skipped as undecodable", c.skipped)
        } else {
            String::new()
        };
        if c.stale {
            // Prune or dedupe shrank the data without leaving an unassigned row,
            // so the count looks complete while the clusters are behind. Say so
            // and name the command. Stale and outstanding can coexist (a dedupe
            // shrinks while a scan adds unassigned rows), so keep the outstanding
            // count on the line rather than hiding it behind the stale note.
            println!(
                "  {:10} {} of {} done, {} outstanding, clusters stale (data changed since the last recompute; run videre locations){}",
                c.stage, done, c.total, c.outstanding, skipped_note
            );
        } else if c.outstanding == 0 {
            println!(
                "  {:10} up to date ({} of {} done{})",
                c.stage, done, c.total, skipped_note
            );
        } else {
            println!(
                "  {:10} {} of {} done, {} outstanding{}{}",
                c.stage,
                done,
                c.total,
                c.outstanding,
                if c.heavy {
                    " (optional, can take hours)"
                } else {
                    ""
                },
                skipped_note,
            );
        }
    }

    print_dates(&report.dates);

    println!();
    println!("Pipeline status:");
    for p in &report.pipelines {
        let last_run = p.last_run_at.as_deref().unwrap_or("never run");
        let status = p.status.as_deref().unwrap_or("-");
        let duration = p
            .duration_ms
            .map(|d| videre_core::progress::human_duration_ms(d as u64))
            .unwrap_or_else(|| "-".to_string());
        let flag = if p.currently_running {
            " (running now)".to_string()
        } else {
            skipped_note(p.summary.as_deref())
        };
        println!(
            "  {:10} {:19} {:11} {:>10}{}",
            p.command, last_run, status, duration, flag
        );
        // The runs before the latest, kept per `run-history`: the long ones
        // are the ones worth seeing, and a later short run used to hide them.
        for run in &p.history {
            let duration = run
                .duration_ms
                .map(|d| videre_core::progress::human_duration_ms(d as u64))
                .unwrap_or_else(|| "-".to_string());
            let note = skipped_note(run.summary.as_deref());
            println!(
                "  {:10} {:19} {:11} {:>10}{}",
                "", run.started_at, run.status, duration, note
            );
        }
    }

    println!();
    print_watch(&report.watch);
    print_recent_problems(&report.logs);

    println!();
    println!("Next actions:");
    let mut any = false;
    for (stage, cost) in &report.costs {
        let coverage = report
            .coverage
            .iter()
            .find(|c| c.stage == *stage)
            .expect("cost stage must come from coverage");
        let Some(command) = coverage.next_command else {
            continue;
        };
        any = true;
        // A duration is shown only when it was measured from a prior run;
        // otherwise the item count stands alone, since a guessed time is
        // usually wrong across different hardware.
        let when = match cost.secs {
            Some(s) if s >= 3600 => format!(", ~{:.1}h", s as f64 / 3600.0),
            Some(s) if s >= 60 => format!(", ~{}m", s / 60),
            Some(s) => format!(", ~{s}s"),
            None => String::new(),
        };
        let tag = if coverage.heavy { " (intensive)" } else { "" };
        println!(
            "  run '{}': {} item(s){}{}",
            command, coverage.outstanding, when, tag
        );
    }
    if report.dates.unresolved > 0 {
        any = true;
        println!(
            "  run 'videre scan': {} file(s) still need their capture date resolved",
            report.dates.unresolved
        );
    }
    if !any {
        println!("  nothing outstanding; the library is up to date");
    }

    if args.check && report.has_problem() {
        return Err(crate::exit::Exit::code(1).into());
    }
    Ok(())
}

/// The latest run of each command that logged errors or warnings, with the
/// last error. Nothing is printed when every latest run was clean.
/// One line on where the dates come from: a library dated by file times
/// alone (an unpacked export, say) is otherwise invisible.
fn print_dates(d: &videre_core::status_report::DateSources) {
    let mut parts = vec![format!("{} from EXIF or video", d.exif + d.video)];
    if d.sidecar > 0 {
        parts.push(format!("{} from Takeout sidecars", d.sidecar));
    }
    if d.mvhd > 0 {
        parts.push(format!("{} from video container times", d.mvhd));
    }
    parts.push(format!("{} from file times only", d.mtime));
    println!();
    println!("Dates: {}", parts.join(", "));
}

fn print_recent_problems(logs: &[videre_core::error_log::CommandLogSummary]) {
    let problems: Vec<_> = logs.iter().filter(|l| l.errors + l.warnings > 0).collect();
    if problems.is_empty() {
        return;
    }
    println!();
    println!("Recent problems (latest run of each command, see .videre/logs/):");
    for l in problems {
        let stages: Vec<String> = l
            .by_stage
            .iter()
            .filter(|(_, (errors, _))| *errors > 0)
            .map(|(stage, (errors, _))| format!("{stage} {errors}"))
            .collect();
        let stages = if stages.is_empty() {
            String::new()
        } else {
            format!(" ({})", stages.join(", "))
        };
        println!(
            "  {:10} run {}: {} error(s){}, {} warning(s)",
            l.command,
            local_time(&l.started),
            l.errors,
            stages,
            l.warnings
        );
        if let Some(last) = &l.last_error {
            let kind = last
                .kind
                .as_deref()
                .map(|k| format!("{k}: "))
                .unwrap_or_default();
            let first_line = last.message.lines().next().unwrap_or_default();
            println!("             last: {kind}{first_line}");
        }
    }
}

/// A log timestamp (UTC) in local time to the minute, or as written when it
/// does not parse.
fn local_time(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| ts.to_owned())
}

fn print_watch(watch: &videre_core::status_report::WatchLiveness) {
    match (watch.running, &watch.last_cycle_at) {
        (true, Some(at)) => println!("Watch: running (last cycle {at})"),
        (true, None) => println!("Watch: running (first cycle in progress)"),
        (false, Some(at)) => println!("Watch: not running (last cycle {at})"),
        (false, None) => println!("Watch: never run"),
    }
}

fn run_json(ctx: &CommandContext) -> anyhow::Result<StatusJson> {
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
    let report: StatusReport = videre_core::status_report::compute_status_in(&conn, &ctx.library)?;
    Ok(StatusJson {
        schema_version: SCHEMA_VERSION,
        report,
    })
}

/// ` (13 file(s) skipped)` for a run that skipped files, else nothing: the
/// one summary worth a column, since a failure's message is in the logs.
fn skipped_note(summary: Option<&str>) -> String {
    summary
        .filter(|s| s.ends_with("skipped"))
        .map(|s| format!(" ({s})"))
        .unwrap_or_default()
}
