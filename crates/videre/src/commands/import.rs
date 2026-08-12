//! `videre import`: bring a library in from another application.
//!
//! The command owns everything that is the same for every source: detection,
//! the location ladder, the confirmation flow, applying recovered dates, and
//! the summary. A source supplies only which rung it starts on (declared as
//! data in `videre_core::import_providers`) and how to recover per-file
//! metadata. Nothing here knows what a Takeout sidecar is.

use filetime::FileTime;
use std::path::{Path, PathBuf};
use videre_core::import_location::{locate_with_database, LocateOptions, Located};
use videre_core::import_providers::{self, ProviderDescriptor};

#[derive(clap::Args)]
pub struct ImportArgs {
    /// The library, package, or export folder to import from
    /// (omit to search the usual places)
    path: Option<PathBuf>,

    /// Where the source's files actually live; overrides every detection rung
    #[arg(long)]
    originals: Option<PathBuf>,

    /// Read the provider's own catalog to locate files (off by default)
    #[arg(long)]
    use_library_db: bool,

    /// Copy into this tree instead of editing in place
    #[arg(long)]
    into: Option<PathBuf>,

    /// Proceed without prompting when the library looks optimised (Apple only)
    #[arg(long)]
    allow_partial: bool,

    /// Report what would change without modifying any file
    #[arg(long)]
    dry_run: bool,

    /// Skip the confirmation prompts and proceed immediately
    #[arg(short = 'y', long = "yes")]
    yes: bool,

    /// Suppress per-file output (errors are always shown)
    #[arg(long)]
    silent: bool,

    /// Print one JSON summary object on stdout
    #[arg(long)]
    json: bool,
}

/// What a run found and did, for the summary and for `--json`.
#[derive(Default)]
pub(crate) struct Summary {
    pub provider: String,
    pub root: PathBuf,
    pub located_via: String,
    pub files: usize,
    pub matched: usize,
    pub unmatched: usize,
    pub ambiguous: usize,
    pub with_location: usize,
    pub updated: usize,
    pub errors: usize,
    pub aborted: bool,
}

pub fn run(args: ImportArgs) -> anyhow::Result<()> {
    if let Some(into) = &args.into {
        anyhow::bail!(
            "--into ({}) is not implemented yet; import currently corrects \
             timestamps in place. Copy the files yourself, then run \
             'videre import' on the copy.",
            into.display()
        );
    }

    let targets = match &args.path {
        Some(path) => {
            anyhow::ensure!(path.exists(), "{} does not exist", path.display());
            match import_providers::detect(path) {
                Some(provider) => vec![(path.clone(), provider)],
                None => {
                    report_nothing_importable(path);
                    return Ok(());
                }
            }
        }
        None => match choose_from_the_usual_places()? {
            Some(chosen) => chosen,
            None => return Ok(()),
        },
    };

    let mut summaries = Vec::new();
    for (root, provider) in targets {
        match import_one(&root, provider, &args)? {
            Some(summary) => summaries.push(summary),
            // Location failed. Already reported in full; nothing to summarise.
            None => std::process::exit(1),
        }
    }

    if args.json {
        let objects: Vec<serde_json::Value> = summaries.iter().map(json_summary).collect();
        println!("{}", serde_json::to_string_pretty(&objects)?);
    }

    if summaries.iter().any(|s| s.errors > 0) {
        std::process::exit(1);
    }
    Ok(())
}

/// One library, end to end. `None` means location failed and was reported.
fn import_one(
    root: &Path,
    provider: &'static ProviderDescriptor,
    args: &ImportArgs,
) -> anyhow::Result<Option<Summary>> {
    if !args.silent {
        eprintln!("Importing from {}", root.display());
        eprintln!("  {}", provider.display);
    }

    let opts = LocateOptions {
        originals_override: args.originals.clone(),
        use_database: args.use_library_db,
    };

    // Only the database rung needs a provider-specific reader, and reading a
    // vendor catalog is command-level work rather than core's.
    let db_roots = None;

    let (roots, via) = match locate_with_database(provider, root, &opts, db_roots)? {
        Located::Found { roots, via } => (roots, via.describe()),
        Located::NotFound { tried } => match self_rooted_export(root, provider) {
            Some(found) => found,
            None => {
                report_not_found(root, &tried);
                return Ok(None);
            }
        },
    };

    let files: Vec<PathBuf> = roots
        .iter()
        .flat_map(|r| videre::scanner::scan(r))
        .collect();

    let mut summary = Summary {
        provider: provider.display.to_string(),
        root: root.to_path_buf(),
        located_via: via.clone(),
        files: files.len(),
        ..Default::default()
    };

    if !args.silent {
        eprintln!("Located {} file(s) via {via}.", files.len());
    }

    if args.dry_run && !args.silent {
        eprintln!("Dry run: no files will be modified.");
    }

    if provider.id == "apple-photos" && !apple_preflight(root, &files, args)? {
        summary.aborted = true;
        return Ok(Some(summary));
    }

    // Per-file metadata recovery: the one thing a source supplies that the
    // command cannot. Everything after this point is source-agnostic again.
    let pending = if provider.id == "google-takeout" {
        recover_takeout_dates(&files, &mut summary, args)
    } else {
        Vec::new()
    };

    let question = if pending.is_empty() {
        "Continue?".to_string()
    } else {
        format!(
            "This will set the modified time on {} file(s) from their import source. Continue?",
            pending.len()
        )
    };
    if !args.yes && !args.dry_run && !confirm(&question)? {
        eprintln!("Aborted; no files modified.");
        summary.aborted = true;
        return Ok(Some(summary));
    }

    apply_dates(&pending, &mut summary, args);

    if !args.silent {
        eprintln!(
            "Next:\n  videre scan {}",
            roots
                .first()
                .map(|r| r.display().to_string())
                .unwrap_or_default()
        );
    }

    Ok(Some(summary))
}

/// The Apple pre-flight: the checklist, then the two filesystem warnings.
///
/// Returns false when the user declined. The checklist prints even under
/// `--yes`, so it appears in any log: skipping the prompt is a statement about
/// automation, not a reason to hide what the user is being asked to confirm.
fn apple_preflight(root: &Path, files: &[PathBuf], args: &ImportArgs) -> anyhow::Result<bool> {
    use super::import_apple;

    let shape = import_apple::survey(root, files);
    if !args.silent {
        eprint!("{}", import_apple::checklist(&shape));
    }

    if shape.looks_referenced() {
        // Reported instead of the optimised warning, not as well as it: a
        // referenced library is a better explanation of tiny originals than
        // iCloud optimisation, and two warnings for one symptom is noise.
        eprintln!("{}", import_apple::referenced_warning(&shape));
    } else if shape.looks_optimised() {
        eprintln!("{}", import_apple::optimised_warning(&shape));
        if !args.allow_partial && !args.yes && !args.dry_run && !confirm("Continue anyway?")? {
            eprintln!("Aborted; no files modified.");
            return Ok(false);
        }
    }
    Ok(true)
}

/// A file and the capture time recovered for it, in Unix seconds.
type PendingDate = (PathBuf, i64);

fn recover_takeout_dates(
    files: &[PathBuf],
    summary: &mut Summary,
    args: &ImportArgs,
) -> Vec<PendingDate> {
    use super::import_takeout;

    let survey = import_takeout::survey(files);
    summary.matched = survey.matched.len();
    summary.unmatched = survey.unmatched;
    summary.ambiguous = survey.ambiguous;

    let mut pending = Vec::new();
    let mut already_correct = 0usize;
    for m in &survey.matched {
        let meta = match std::fs::read_to_string(&m.sidecar)
            .map_err(anyhow::Error::from)
            .and_then(|text| import_takeout::parse_sidecar(&text))
        {
            Ok(meta) => meta,
            Err(e) => {
                // A malformed sidecar leaves its file untouched and the run
                // continues; a missing date is a normal outcome, not a failure.
                eprintln!("warning: {}: {e}", m.sidecar.display());
                continue;
            }
        };
        if meta.gps.is_some() {
            summary.with_location += 1;
        }
        let Some(taken) = meta.taken_unix else {
            continue;
        };
        if current_mtime(&m.file) == Some(taken) {
            already_correct += 1;
            continue;
        }
        pending.push((m.file.clone(), taken));
    }

    if !args.silent {
        report_takeout_survey(summary, survey.folders);
        eprintln!(
            "  {} would have their date corrected ({already_correct} already agree)",
            pending.len()
        );
    }
    pending
}

/// Sets each recovered capture time as the file's mtime.
///
/// `filetime::set_file_mtime` is the same call `fix-dates` uses, so the two
/// commands cannot disagree about what "correcting a date" means.
fn apply_dates(pending: &[PendingDate], summary: &mut Summary, args: &ImportArgs) {
    for (file, taken) in pending {
        if !args.dry_run {
            if let Err(e) = filetime::set_file_mtime(file, FileTime::from_unix_time(*taken, 0)) {
                if e.kind() == std::io::ErrorKind::NotFound {
                    // Moved or trashed since the walk; not this run's problem.
                    continue;
                }
                eprintln!("Error: {}: {e}", file.display());
                summary.errors += 1;
                continue;
            }
        }
        summary.updated += 1;
        if !args.silent {
            let prefix = if args.dry_run {
                "[dry-run]"
            } else {
                "[updated]"
            };
            println!("{prefix} {}  ->  {}", file.display(), format_date(*taken));
        }
    }
}

fn current_mtime(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()
        .map(|m| FileTime::from_last_modification_time(&m).unix_seconds())
}

fn format_date(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| unix.to_string())
}

/// Takeout is routinely handed the export folder itself rather than its
/// parent, and there is then no layout directory to find because the folder
/// *is* the layout. Detection has already proved sidecars are present, so this
/// is a match rather than a guess. Reported honestly: the folder is not
/// pretending to be a `Google Photos/` directory it does not contain.
fn self_rooted_export(
    root: &Path,
    provider: &ProviderDescriptor,
) -> Option<(Vec<PathBuf>, String)> {
    if provider.id != "google-takeout" {
        return None;
    }
    Some((
        vec![root.to_path_buf()],
        "the export folder itself".to_string(),
    ))
}

/// A plain folder of photos is a normal outcome with a useful answer, not a
/// failure: there is no import step for it at all, and saying so is more use
/// than an error.
fn report_nothing_importable(path: &Path) {
    let media = videre::scanner::scan(path).len();
    eprintln!("Nothing importable found under {}.", path.display());
    if media > 0 {
        eprintln!("  {media} media file(s) are there, in ordinary folders.");
    }
    eprintln!(
        "  Nothing to import: run 'videre scan {}' to use them directly.",
        path.display()
    );
}

/// The matched percentage is the number to watch: far below ~95% means the
/// matching rules have missed a naming variant and the run wants inspecting
/// rather than trusting.
fn report_takeout_survey(s: &Summary, folders: usize) {
    let pct = if s.files > 0 {
        (s.matched as f64) * 100.0 / (s.files as f64)
    } else {
        0.0
    };
    eprintln!("  {} media file(s) in {folders} folder(s)", s.files);
    eprintln!("  {} matched a sidecar ({pct:.1}%)", s.matched);
    eprintln!("  {} unmatched, left untouched", s.unmatched);
    eprintln!("  {} ambiguous, left untouched", s.ambiguous);
}

fn report_not_found(root: &Path, tried: &[String]) {
    eprintln!("Could not locate the files in this library.");
    for line in tried {
        eprintln!("  Tried {line}");
    }
    eprintln!();
    eprintln!("  This usually means the application changed its structure in a version");
    eprintln!("  newer than this build of videre knows about.");
    eprintln!();
    eprintln!("  If you know where the photos are, point videre at them directly:");
    eprintln!(
        "    videre import {} --originals <path/to/photos>",
        root.display()
    );
    eprintln!("    videre scan <path/to/photos>       # or use them as an ordinary folder");
}

/// `videre import` with no path: glob the known locations and ask.
///
/// One match still prints what it found and goes through the same confirmation.
/// Detection narrows the question; it never removes the confirmation.
#[allow(clippy::type_complexity)]
fn choose_from_the_usual_places(
) -> anyhow::Result<Option<Vec<(PathBuf, &'static ProviderDescriptor)>>> {
    let found = import_providers::discover();
    if found.is_empty() {
        eprintln!("Nothing importable found in the usual places.");
        eprintln!("  If your library is somewhere else, point videre at it:");
        eprintln!("    videre import <path>");
        eprintln!("  For an ordinary folder of photos there is nothing to import:");
        eprintln!("    videre scan <path>");
        return Ok(None);
    }

    eprintln!("Found {} librar(ies):", found.len());
    for (i, c) in found.iter().enumerate() {
        eprintln!("\n  {}. {}", i + 1, c.path.display());
        eprintln!("     {}", c.provider.display);
    }

    let answer = prompt(&format!(
        "\nImport which? [{}/a=all/q]",
        (1..=found.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("/")
    ))?;

    let answer = answer.trim().to_lowercase();
    if answer == "a" || answer == "all" {
        return Ok(Some(
            found.into_iter().map(|c| (c.path, c.provider)).collect(),
        ));
    }
    match answer.parse::<usize>() {
        Ok(n) if n >= 1 && n <= found.len() => {
            let c = &found[n - 1];
            Ok(Some(vec![(c.path.clone(), c.provider)]))
        }
        _ => {
            eprintln!("Aborted; nothing imported.");
            Ok(None)
        }
    }
}

fn prompt(text: &str) -> anyhow::Result<String> {
    use std::io::Write;
    eprint!("{text} ");
    std::io::stderr().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input)
}

fn confirm(text: &str) -> anyhow::Result<bool> {
    super::confirm(text)
}

fn json_summary(s: &Summary) -> serde_json::Value {
    serde_json::json!({
        "schema_version": videre::types::SCHEMA_VERSION,
        "provider": s.provider,
        "path": s.root.display().to_string(),
        "located_via": s.located_via,
        "files": s.files,
        "matched": s.matched,
        "unmatched": s.unmatched,
        "ambiguous": s.ambiguous,
        "with_location": s.with_location,
        "updated": s.updated,
        "errors": s.errors,
        "aborted": s.aborted,
    })
}
