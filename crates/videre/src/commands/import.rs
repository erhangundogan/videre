//! `videre import`: bring a library in from another application.
//!
//! The command owns everything that is the same for every source: detection,
//! the location ladder, the confirmation flow, applying recovered dates, and
//! the summary. A source supplies only which rung it starts on (declared as
//! data in `videre_core::import_providers`) and how to recover per-file
//! metadata. Nothing here knows what a Takeout sidecar is.

use crate::command_context::CommandContext;
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

    /// Report only. import changes no file either way: scan reads a Takeout
    /// sidecar's date, and fix-dates sets it as the file's time
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
    /// Google Takeout: files whose sidecar records when they were taken.
    pub dated: usize,
    /// Files whose time import changed: none since scan reads sidecar dates
    /// and fix-dates writes them. Kept so the JSON shape holds.
    pub updated: usize,
    pub errors: usize,
    pub aborted: bool,
    /// Google Takeout only: edits exported beside their originals, counted by
    /// name (`videre::takeout_names`), for the hint to `dedupe --edited`.
    pub edited_pairs: usize,
}

pub fn run(args: ImportArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    if let Some(into) = &args.into {
        anyhow::bail!(
            "--into ({}) is not implemented yet; import works in place. Copy \
             the files yourself, then run 'videre import' on the copy.",
            ctx.operand(into).display()
        );
    }

    // A relative provider path is invocation-relative, like any explicit file
    // operand; bare discovery searches the selected library root.
    let targets = match &args.path {
        Some(path) => {
            let path = ctx.operand(path);
            anyhow::ensure!(path.exists(), "{} does not exist", path.display());
            match import_providers::detect(&path) {
                Some(provider) => vec![(path, provider)],
                None => {
                    report_nothing_importable(&path);
                    return Ok(());
                }
            }
        }
        None => match choose_from_the_usual_places(&ctx.library.paths.root)? {
            Some(chosen) => chosen,
            None => return Ok(()),
        },
    };

    let mut summaries = Vec::new();
    for (root, provider) in targets {
        match import_one(&root, provider, &args, ctx)? {
            Some(summary) => summaries.push(summary),
            // Location failed. Already reported in full; nothing to summarise.
            None => return Err(crate::exit::Exit::code(1).into()),
        }
    }

    if args.json {
        let objects: Vec<serde_json::Value> = summaries.iter().map(json_summary).collect();
        println!("{}", serde_json::to_string_pretty(&objects)?);
    }

    if summaries.iter().any(|s| s.errors > 0) {
        return Err(crate::exit::Exit::code(1).into());
    }
    Ok(())
}

/// One library, end to end. `None` means location failed and was reported.
fn import_one(
    root: &Path,
    provider: &'static ProviderDescriptor,
    args: &ImportArgs,
    ctx: &CommandContext,
) -> anyhow::Result<Option<Summary>> {
    if !args.silent {
        tracing::info!("Importing from {}", root.display());
        tracing::info!("  {}", provider.display);
    }

    let opts = LocateOptions {
        // A relative --originals is invocation-relative, like the provider path.
        originals_override: args.originals.as_ref().map(|o| ctx.operand(o)),
        use_database: args.use_library_db,
    };

    // Only the database rung needs a provider-specific reader, and reading a
    // vendor catalog is command-level work rather than core's.
    let db_roots = if provider.id == "lightroom" {
        lightroom_roots(root, args)
    } else {
        None
    };

    let (roots, via) = match locate_with_database(provider, root, &opts, db_roots)? {
        Located::Found { roots, via } => (roots, via.describe()),
        Located::NotFound { tried } => match self_rooted_export(root, provider) {
            Some(found) => found,
            None => {
                report_not_found(root, provider, &tried);
                return Ok(None);
            }
        },
    };

    let files: Vec<PathBuf> = roots
        .iter()
        .flat_map(|r| videre::scanner::scan(r))
        .collect();

    // Whole-batch containment check before any preflight, confirmation or
    // write, including under --yes: import only ever corrects the mtime of
    // files inside the selected library. The provider path and any metadata it
    // reads may be outside, but the originals it writes to may not.
    videre_core::library_guard::validate_paths(&ctx.library, &files)?;

    let mut summary = Summary {
        provider: provider.display.to_string(),
        root: root.to_path_buf(),
        located_via: via.clone(),
        files: files.len(),
        edited_pairs: if provider.id == "google-takeout" {
            videre::takeout_names::edited_name_pairs(&files)
        } else {
            0
        },
        ..Default::default()
    };

    if !args.silent {
        tracing::info!("Located {} file(s) via {via}.", files.len());
    }

    if args.dry_run && !args.silent {
        tracing::info!("Dry run: no files will be modified.");
    }

    if provider.id == "apple-photos" && !apple_preflight(root, &files, args)? {
        summary.aborted = true;
        return Ok(Some(summary));
    }

    // Takeout: what its sidecars hold. scan reads their dates and places and
    // fix-dates writes the dates as file times, so import itself changes no
    // file and asks nothing; two date writers once undid each other.
    let takeout = provider.id == "google-takeout";
    if takeout {
        survey_takeout(&files, &mut summary, args);
    } else if !args.yes && !args.dry_run && !confirm("Continue?")? {
        tracing::info!("Aborted; no files modified.");
        summary.aborted = true;
        return Ok(Some(summary));
    }

    if !args.silent {
        // Every located root, since a Lightroom catalog routinely has several
        // and the useful output there is "these are your folders".
        tracing::info!("Next:");
        for r in &roots {
            tracing::info!("  videre scan {}", r.display());
        }
        if takeout && summary.dated > 0 {
            tracing::info!(
                "  videre fix-dates                  # set {} file time(s) from their sidecar",
                summary.dated
            );
        }
        if summary.edited_pairs > 0 {
            tracing::info!(
                "  videre dedupe --edited --html     # review {} photo(s) Google Photos exported twice",
                summary.edited_pairs
            );
            tracing::info!(
                "  videre dedupe --edited --trash    # keep the originals, trash the edits"
            );
        }
    }

    Ok(Some(summary))
}

/// The Lightroom catalog rung: which folders the catalog points at.
///
/// `None` means the catalog could not be read, which for Lightroom drops
/// straight to asking the user: there is no folder layout to fall through to,
/// because the files live wherever the user put them.
fn lightroom_roots(root: &Path, args: &ImportArgs) -> Option<Vec<PathBuf>> {
    use super::import_lightroom;

    let catalog = match import_lightroom::find_catalog(root) {
        Some(c) => c,
        None => {
            tracing::error!("Could not find a .lrcat catalog at {}.", root.display());
            return None;
        }
    };
    let roots = match import_lightroom::read_root_folders(&catalog) {
        Ok(roots) => roots,
        Err(e) => {
            tracing::error!("Could not read the catalog: {e}");
            return None;
        }
    };

    let classified = import_lightroom::classify(roots);
    if !args.silent {
        tracing::info!("Catalog references {} root folder(s):", classified.len());
        for r in &classified {
            tracing::info!(
                "  {:<50} {}",
                r.path.display(),
                if r.online { "online" } else { "OFFLINE" }
            );
        }
        if classified.iter().any(|r| !r.online) {
            // Offline is a normal state of a Lightroom catalog, not an error
            // and not a missing file.
            tracing::info!(
                "Using online folders only. Reconnect the others and re-run to include them."
            );
        }
    }

    let online = import_lightroom::online_paths(&classified);
    (!online.is_empty()).then_some(online)
}

/// The Apple pre-flight: the checklist, then the two filesystem warnings.
///
/// Returns false when the user declined. The checklist prints even under
/// `--yes`, so it appears in any log: skipping the prompt is a statement about
/// automation, not a reason to hide what the user is being asked to confirm.
fn apple_preflight(root: &Path, files: &[PathBuf], args: &ImportArgs) -> anyhow::Result<bool> {
    use super::import_apple;

    let shape = import_apple::survey(root, files);

    // Nothing to prepare for, so the checklist would be noise on top of a
    // message that already says the run cannot happen.
    if shape.files == 0 {
        tracing::info!("{}", import_apple::empty_originals_warning());
        return Ok(false);
    }

    if !args.silent {
        eprint!("{}", import_apple::checklist(&shape));
    }

    if shape.looks_referenced() {
        // Reported instead of the optimised warning, not as well as it: a
        // referenced library is a better explanation of tiny originals than
        // iCloud optimisation, and two warnings for one symptom is noise.
        tracing::info!("{}", import_apple::referenced_warning(&shape));
    } else if shape.looks_optimised() {
        tracing::info!("{}", import_apple::optimised_warning(&shape));
        if !args.allow_partial && !args.yes && !args.dry_run && !confirm("Continue anyway?")? {
            tracing::info!("Aborted; no files modified.");
            return Ok(false);
        }
    }
    Ok(true)
}

/// Counts what a Takeout export's sidecars hold. Nothing is written: scan
/// reads the dates and places, fix-dates sets the dates as file times.
fn survey_takeout(files: &[PathBuf], summary: &mut Summary, args: &ImportArgs) {
    use videre::takeout_sidecar as import_takeout;

    let survey = import_takeout::survey(files);
    summary.matched = survey.matched.len();
    summary.unmatched = survey.unmatched;
    summary.ambiguous = survey.ambiguous;

    for m in &survey.matched {
        let meta = match std::fs::read_to_string(&m.sidecar)
            .map_err(anyhow::Error::from)
            .and_then(|text| import_takeout::parse_sidecar(&text))
        {
            Ok(meta) => meta,
            Err(e) => {
                // A malformed sidecar leaves its file untouched and the run
                // continues; a missing date is a normal outcome, not a failure.
                tracing::warn!("{}: {e}", m.sidecar.display());
                continue;
            }
        };
        if meta.gps.is_some() {
            summary.with_location += 1;
        }
        if meta.taken_unix.is_some() {
            summary.dated += 1;
        }
    }

    if !args.silent {
        report_takeout_survey(summary, survey.folders);
        tracing::info!(
            "  {} with a capture date and {} with a place; videre scan reads both",
            summary.dated,
            summary.with_location
        );
    }
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
    tracing::info!("Nothing importable found under {}.", path.display());
    if media > 0 {
        tracing::info!("  {media} media file(s) are there, in ordinary folders.");
    }
    tracing::info!(
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
    tracing::info!("  {} media file(s) in {folders} folder(s)", s.files);
    tracing::info!("  {} matched a sidecar ({pct:.1}%)", s.matched);
    tracing::info!("  {} unmatched, left untouched", s.unmatched);
    tracing::info!("  {} ambiguous, left untouched", s.ambiguous);
}

fn report_not_found(root: &Path, provider: &ProviderDescriptor, tried: &[String]) {
    tracing::error!("Could not locate the files in this library.");
    for line in tried {
        tracing::info!("  Tried {line}");
    }
    tracing::info!("");
    if provider.layouts.is_empty() {
        // Lightroom and anything else with no folder layout: there is nothing
        // below the catalog rung, so say why rather than implying a guess is
        // still possible.
        tracing::info!(
            "  {} stores photos in folders you chose, so videre cannot guess",
            provider.display
        );
        tracing::info!("  where they are.");
        tracing::info!("");
    }
    if videre_core::import_location::access_is_denied(root) {
        // The folders are almost certainly there; the OS is hiding them.
        tracing::info!("  videre is not allowed to read this folder, so it cannot see what is");
        tracing::info!("  inside it. This is a permissions problem, not a missing library.");
        tracing::info!("");
        if cfg!(target_os = "macos") {
            tracing::info!(
                "  macOS protects a Photos library. Grant access to the program you run"
            );
            tracing::info!("  videre from (Terminal, iTerm, your editor):");
            tracing::info!("");
            tracing::info!("    System Settings -> Privacy & Security -> Full Disk Access");
            tracing::info!("");
            tracing::info!(
                "  Add it, switch it on, then quit and reopen it. The setting only takes"
            );
            tracing::info!("  effect in a newly started program.");
            tracing::info!("");
        }
    } else {
        tracing::info!("  This usually means the application changed its structure in a version");
        tracing::info!("  newer than this build of videre knows about.");
        tracing::info!("");
    }
    tracing::info!("  If you know where the photos are, point videre at them directly:");
    tracing::info!(
        "    videre import {} --originals <path/to/photos>",
        root.display()
    );
    tracing::info!("    videre scan <path/to/photos>       # or use them as an ordinary folder");
}

/// `videre import` with no path: glob the known locations and ask.
///
/// One match still prints what it found and goes through the same confirmation.
/// Detection narrows the question; it never removes the confirmation.
#[allow(clippy::type_complexity)]
fn choose_from_the_usual_places(
    search_root: &Path,
) -> anyhow::Result<Option<Vec<(PathBuf, &'static ProviderDescriptor)>>> {
    let found = import_providers::discover_in(&[search_root.to_path_buf()]);
    if found.is_empty() {
        tracing::info!("Nothing importable found in the usual places.");
        tracing::info!("  If your library is somewhere else, point videre at it:");
        tracing::info!("    videre import <path>");
        tracing::info!("  For an ordinary folder of photos there is nothing to import:");
        tracing::info!("    videre scan <path>");
        return Ok(None);
    }

    tracing::info!("Found {} librar(ies):", found.len());
    for (i, c) in found.iter().enumerate() {
        tracing::info!("\n  {}. {}", i + 1, c.path.display());
        tracing::info!("     {}", c.provider.display);
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
            tracing::info!("Aborted; nothing imported.");
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
        "dated": s.dated,
        "updated": s.updated,
        "errors": s.errors,
        "aborted": s.aborted,
        "edited_pairs": s.edited_pairs,
    })
}
