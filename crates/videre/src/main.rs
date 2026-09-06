use clap::{Parser, Subcommand};
use std::ffi::{OsStr, OsString};

mod command_context;
mod commands;
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
enum Command {
    /// Report duplicate files from the database and print paths to remove
    Dedupe(commands::dedupe::DedupeArgs),
    /// Browse the library in a local web UI: all files, people, dates
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
    /// Show or edit the selected library's configuration
    Config(commands::config::ConfigArgs),
    /// Serve read-only MCP tools (search, find_duplicates, stats) over stdio for LLM agents
    Mcp(commands::mcp::McpArgs),
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

fn unconverted(name: &str) -> anyhow::Result<()> {
    anyhow::bail!(
        "'{name}' is temporarily unavailable while its directory-local library support is being completed"
    )
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
    let result = match command {
        Command::Scan(args) => match command_context::CommandContext::capture(library) {
            Ok(ctx) => commands::scan::run(args, &ctx),
            Err(error) => commands::scan::report_startup_error(args, error),
        },
        Command::Config(args) => command_context::CommandContext::capture(library)
            .and_then(|ctx| commands::config::run(args, &ctx)),
        Command::Dedupe(args) => command_context::CommandContext::capture(library)
            .and_then(|ctx| commands::dedupe::run(args, &ctx)),
        Command::Gallery(_) => unconverted("gallery"),
        Command::FixDates(_) => unconverted("fix-dates"),
        Command::Import(_) => unconverted("import"),
        Command::Prune(_) => unconverted("prune"),
        Command::Locations(args) => command_context::CommandContext::capture(library)
            .and_then(|ctx| commands::locations::run(args, &ctx)),
        Command::Embed(args) => command_context::CommandContext::capture(library)
            .and_then(|ctx| commands::embed::run(args, &ctx)),
        Command::Search(args) => command_context::CommandContext::capture(library)
            .and_then(|ctx| commands::search::run(args, &ctx)),
        Command::Faces(args) => command_context::CommandContext::capture(library)
            .and_then(|ctx| commands::faces::run(args, &ctx)),
        Command::Classify(args) => command_context::CommandContext::capture(library)
            .and_then(|ctx| commands::classify::run(args, &ctx)),
        Command::Watch(_) => unconverted("watch"),
        Command::Mcp(_) => unconverted("mcp"),
        Command::Stats(args) => command_context::CommandContext::capture(library)
            .and_then(|ctx| commands::stats::run(args, &ctx)),
        Command::Mark(_) => unconverted("mark"),
        Command::Export(_) => unconverted("export"),
        Command::Tag(_) => unconverted("tag"),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
