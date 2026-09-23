use clap::{Parser, Subcommand};
use std::ffi::{OsStr, OsString};

mod command_context;
mod commands;
mod exit;
mod logging;
mod removal;
mod render;
mod xmp;

#[derive(Parser)]
#[command(
    name = "videre",
    version,
    about = "Local-first media library toolkit: dedupe, semantic search, faces, and browsing over one SQLite database"
)]
struct Cli {
    /// Library root (default: invocation directory)
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        value_parser = library_value_parser(),
        overrides_with = "library"
    )]
    library: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
// Help lists subcommands alphabetically: clap's default display order renders
// declaration order, which here is historical wiring order, not anything a
// reader of --help can perceive. None collapses every display order, so
// clap's name sort takes over.
#[command(next_display_order = None)]
enum Command {
    /// Report duplicate files from the database and print paths to remove
    Dedupe(commands::dedupe::DedupeArgs),
    /// Browse the library in a local web UI: files, duplicates, dates, people, map, events
    Gallery(commands::gallery::GalleryArgs),
    /// Scan a directory, hash every image, and populate the database
    Scan(commands::scan::ScanArgs),
    /// Set each file's mtime to its EXIF shoot date
    FixDates(commands::fix_dates::FixDatesArgs),
    /// Import from Google Takeout, Apple Photos, or a Lightroom catalog
    Import(commands::import::ImportArgs),
    /// Remove stale rows, sync metadata, clean orphan embeddings
    Prune(commands::prune::PruneArgs),
    /// Cluster GPS coordinates by geographic proximity and persist the result
    Locations(commands::locations::LocationsArgs),
    /// Compute SigLIP embeddings for every image in the database
    Embed(commands::embed::EmbedArgs),
    /// Search images by text, example image, or person name
    Search(commands::search::SearchArgs),
    /// Detect, embed, and cluster faces; enables person search
    Faces(commands::faces::FacesArgs),
    /// Classify images as photo/screenshot/document/meme (zero-shot, reuses embeddings)
    Classify(commands::classify::ClassifyArgs),
    /// Background loop keeping scan/faces/HEIC-cache/location data fresh
    Watch(commands::watch::WatchArgs),
    /// Bring the library fully current: scan, then faces, embed, classify, locations
    Pipeline(commands::pipeline::PipelineArgs),
    /// Show or edit the selected library's configuration
    Config(commands::config::ConfigArgs),
    /// Serve read-only MCP tools (search, find_duplicates, stats) over stdio for LLM agents
    Mcp(commands::mcp::McpArgs),
    /// Pipeline health: staleness, watch liveness, what to run next
    Status(commands::status::StatusArgs),
    /// Show library totals and per-command pipeline run status
    Stats(commands::stats::StatsArgs),
    /// Set ratings, picks, colour labels and likes on photos
    Mark(commands::mark::MarkArgs),
    /// Write videre's labels to portable .xmp sidecars (faces, location, marks)
    Export(commands::export::ExportArgs),
    /// Add or remove free-form tags on photos (filter by them with search --tag)
    Tag(commands::tag::TagArgs),
}

fn library_value_parser() -> impl clap::builder::TypedValueParser<Value = std::path::PathBuf> {
    use clap::builder::TypedValueParser;
    clap::builder::OsStringValueParser::new().try_map(|raw: OsString| {
        if raw.is_empty() {
            Err("library directory must not be empty".to_string())
        } else {
            Ok(std::path::PathBuf::from(raw))
        }
    })
}

/// Count selectors exactly as supplied, stopping at the option terminator.
/// Clap propagates global options into nested matches, so the original argv is
/// the only unambiguous source for detecting duplicates across those levels.
fn library_option_count(args: &[OsString]) -> usize {
    args.iter()
        .skip(1)
        .take_while(|arg| arg.as_os_str() != OsStr::new("--"))
        .filter(|arg| {
            arg.as_os_str() == OsStr::new("--library")
                || arg
                    .as_os_str()
                    .as_encoded_bytes()
                    .starts_with(b"--library=")
        })
        .count()
}

/// Appends `Full documentation: https://docs.videre.sh/commands/<name>/` to
/// every subcommand's `--help`.
///
/// Derived from the subcommand name rather than written on each `Args` struct,
/// because fifteen hand-written URLs are fifteen chances to paste the wrong one
/// and one more thing to forget when a command is added. The docs site uses the
/// subcommand name as its page slug, so the mapping is total by construction;
/// `docs_links_point_at_pages_that_exist` in `tests/docs_flags.rs` asserts a
/// page exists for each.
fn with_docs_links(cmd: clap::Command) -> clap::Command {
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    names.into_iter().fold(cmd, |c, name| {
        let url = format!("Full documentation: https://docs.videre.sh/commands/{name}/");
        c.mut_subcommand(name, |s| s.after_help(url))
    })
}

fn main() {
    let raw: Vec<OsString> = std::env::args_os().collect();
    let cli = {
        use clap::{CommandFactory, FromArgMatches};
        let matches = match with_docs_links(Cli::command()).try_get_matches_from(&raw) {
            Ok(matches) => matches,
            Err(error) => error.exit(),
        };
        if library_option_count(&raw) > 1 {
            clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--library may be supplied only once",
            )
            .exit();
        }
        match Cli::from_arg_matches(&matches) {
            Ok(c) => c,
            Err(e) => e.exit(),
        }
    };
    let Cli { library, command } = cli;
    let code = match command {
        Command::Scan(args) => {
            let json = args.json();
            with_ctx_or(
                library,
                "scan",
                move |e| commands::scan::report_startup_error(json, e),
                |ctx| commands::scan::run(args, ctx),
            )
        }
        Command::Config(args) => {
            with_ctx(library, "config", |ctx| commands::config::run(args, ctx))
        }
        Command::Dedupe(args) => {
            with_ctx(library, "dedupe", |ctx| commands::dedupe::run(args, ctx))
        }
        Command::Gallery(args) => {
            with_ctx(library, "gallery", |ctx| commands::gallery::run(args, ctx))
        }
        Command::FixDates(args) => with_ctx(library, "fix-dates", |ctx| {
            commands::fix_dates::run(args, ctx)
        }),
        Command::Import(args) => {
            with_ctx(library, "import", |ctx| commands::import::run(args, ctx))
        }
        Command::Prune(args) => with_ctx(library, "prune", |ctx| commands::prune::run(args, ctx)),
        Command::Locations(args) => with_ctx(library, "locations", |ctx| {
            commands::locations::run(args, ctx)
        }),
        Command::Embed(args) => with_ctx(library, "embed", |ctx| commands::embed::run(args, ctx)),
        Command::Search(args) => {
            with_ctx(library, "search", |ctx| commands::search::run(args, ctx))
        }
        Command::Faces(args) => with_ctx(library, "faces", |ctx| commands::faces::run(args, ctx)),
        Command::Classify(args) => with_ctx(library, "classify", |ctx| {
            commands::classify::run(args, ctx)
        }),
        Command::Watch(args) => with_ctx(library, "watch", |ctx| commands::watch::run(args, ctx)),
        Command::Pipeline(args) => with_ctx(library, "pipeline", |ctx| {
            commands::pipeline::run(args, ctx)
        }),
        Command::Mcp(args) => with_ctx(library, "mcp", |ctx| commands::mcp::run(args, ctx)),
        Command::Stats(args) => with_ctx(library, "stats", |ctx| commands::stats::run(args, ctx)),
        Command::Status(args) => {
            with_ctx(library, "status", |ctx| commands::status::run(args, ctx))
        }
        Command::Mark(args) => with_ctx(library, "mark", |ctx| commands::mark::run(args, ctx)),
        Command::Export(args) => {
            with_ctx(library, "export", |ctx| commands::export::run(args, ctx))
        }
        Command::Tag(args) => with_ctx(library, "tag", |ctx| commands::tag::run(args, ctx)),
    };
    // The only process exit in the binary: everything a command held,
    // buffered log writers included, has been dropped by now.
    std::process::exit(code);
}

/// Capture the invocation's library, run one command in it, and turn the
/// result into an exit code. `command` is the subcommand's name as typed.
fn with_ctx(
    library: Option<std::path::PathBuf>,
    command: &'static str,
    run: impl FnOnce(&command_context::CommandContext) -> anyhow::Result<()>,
) -> i32 {
    with_ctx_or(library, command, Err, run)
}

/// `with_ctx` with a handler for a library that cannot be captured, for the
/// one command (`scan --json`) that reports even that failure as JSON.
fn with_ctx_or(
    library: Option<std::path::PathBuf>,
    command: &'static str,
    on_capture_error: impl FnOnce(anyhow::Error) -> anyhow::Result<()>,
    run: impl FnOnce(&command_context::CommandContext) -> anyhow::Result<()>,
) -> i32 {
    match command_context::CommandContext::capture(library) {
        Ok(ctx) => {
            // Held until the result is reported, so the final error is in the
            // log before the writers flush on drop.
            let _log = logging::install(&ctx.library, command);
            finish(run(&ctx))
        }
        Err(e) => {
            logging::install_terminal_only();
            finish(on_capture_error(e))
        }
    }
}

/// Report a command's outcome once, through the logging layers (the terminal
/// shows `error: ...` exactly as before), and return its exit code.
fn finish(result: anyhow::Result<()>) -> i32 {
    let Err(e) = result else { return 0 };
    match e.downcast::<exit::Exit>() {
        Ok(exit) => {
            match &exit.error {
                Some(error) if exit.shown => {
                    videre_core::error_log::report_file_only(&format!("{error:#}"))
                }
                Some(error) => videre_core::error_log::report(tracing::Level::ERROR, error, None),
                None => {}
            }
            exit.code
        }
        Err(e) => {
            videre_core::error_log::report(tracing::Level::ERROR, &e, None);
            1
        }
    }
}
