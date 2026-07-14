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
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Dedupe(args) => commands::dedupe::run(args),
        Command::Report(args) => commands::report::run(args),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
