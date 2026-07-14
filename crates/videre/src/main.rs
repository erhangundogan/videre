use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(
    name = "videre",
    version,
    about = "Local-first media library toolkit: dedupe, semantic search, faces, and reports over one SQLite database"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a directory, hash every image, and print duplicate paths to stdout
    Dedupe(commands::dedupe::DedupeArgs),
    /// Generate an HTML review page, or serve the live report/labeling UI
    Report(commands::report::ReportArgs),
    /// Set each file's mtime to its EXIF shoot date
    FixDates(commands::fix_dates::FixDatesArgs),
    /// Remove stale rows, sync metadata, clean orphan embeddings
    Prune(commands::prune::PruneArgs),
    /// Compute SigLIP embeddings for every image in the database
    Embed(commands::embed::EmbedArgs),
    /// Search images by text, example image, or person name
    Search(commands::search::SearchArgs),
    /// Detect, embed, and cluster faces; enables person search
    Faces(commands::faces::FacesArgs),
    /// Background loop keeping scan/faces/HEIC-cache/location data fresh
    Watch(commands::watch::WatchArgs),
}

fn main() {
    let cli = Cli::parse();
    videre_core::thumb_cache::migrate_legacy_dupe_cache();
    let result = match cli.command {
        Command::Dedupe(args) => commands::dedupe::run(args),
        Command::Report(args) => commands::report::run(args),
        Command::FixDates(args) => commands::fix_dates::run(args),
        Command::Prune(args) => commands::prune::run(args),
        Command::Embed(args) => commands::embed::run(args),
        Command::Search(args) => commands::search::run(args),
        Command::Faces(args) => commands::faces::run(args),
        Command::Watch(args) => commands::watch::run(args),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
