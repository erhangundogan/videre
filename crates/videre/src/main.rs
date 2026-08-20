use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(
    name = "videre",
    version,
    about = "Local-first media library toolkit: dedupe, semantic search, faces, and browsing over one SQLite database"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report duplicate files from the database and print paths to remove
    Dedupe(commands::dedupe::DedupeArgs),
    /// Generate an HTML review page, or serve the live report/labeling UI
    /// Deprecated in 0.18.0 and removed in the release after. Use `videre
    /// gallery` to browse; `dedupe` and `search` can each write a page.
    #[command(hide = true)]
    Report(commands::report::ReportArgs),
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
    /// Show or edit videre's config and default paths (~/.videre)
    Config(commands::config::ConfigArgs),
    /// Serve read-only MCP tools (search, find_duplicates, stats) over stdio for LLM agents
    Mcp(commands::mcp::McpArgs),
    /// Show library totals and per-command pipeline run status
    Stats(commands::stats::StatsArgs),
}

/// Pushes the configured floor read rate into `io_timeout` before any command
/// runs, since the timeout is resolved process-wide rather than threaded
/// through `hash_file`'s 13 call sites. A missing or unreadable config is not
/// an error: the built-in default applies, exactly as it did before this key
/// existed.
fn apply_configured_read_rate() {
    if let Ok(home) = videre_core::home::videre_home() {
        if let Ok(cfg) = videre_core::home::load_config(&home) {
            if let Some(rate) = cfg.min_read_rate_mb_s {
                videre_core::io_timeout::set_min_read_rate_mb_s(rate);
            }
        }
    }
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
    let cli = {
        use clap::{CommandFactory, FromArgMatches};
        let matches = with_docs_links(Cli::command()).get_matches();
        match Cli::from_arg_matches(&matches) {
            Ok(c) => c,
            Err(e) => e.exit(),
        }
    };
    videre_core::thumb_cache::migrate_legacy_dupe_cache();
    apply_configured_read_rate();
    let result = match cli.command {
        Command::Dedupe(args) => commands::dedupe::run(args),
        Command::Report(args) => commands::report::run(args),
        Command::Gallery(args) => commands::gallery::run(args),
        Command::Scan(args) => commands::scan::run(args),
        Command::FixDates(args) => commands::fix_dates::run(args),
        Command::Import(args) => commands::import::run(args),
        Command::Prune(args) => commands::prune::run(args),
        Command::Locations(args) => commands::locations::run(args),
        Command::Embed(args) => commands::embed::run(args),
        Command::Search(args) => commands::search::run(args),
        Command::Faces(args) => commands::faces::run(args),
        Command::Classify(args) => commands::classify::run(args),
        Command::Watch(args) => commands::watch::run(args),
        Command::Config(args) => commands::config::run(args),
        Command::Mcp(args) => commands::mcp::run(args),
        Command::Stats(args) => commands::stats::run(args),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
