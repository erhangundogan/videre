use videre::types::ErrorJson;
use std::path::PathBuf;
use std::process;

#[derive(clap::Args)]
pub struct DedupeArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Also report perceptual-hash near-duplicate clusters (review-only)
    #[arg(long)]
    similar: bool,

    /// Suppress progress output on stderr (duplicate paths are always written to stdout)
    #[arg(long)]
    silent: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

pub fn run(args: DedupeArgs) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                process::exit(1);
            }
        }
    } else {
        run_text(args)
    }
}

fn run_text(args: DedupeArgs) -> anyhow::Result<()> {
    let db = match super::resolve_reader_db_must_exist(args.db) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };

    let records = match videre::sqlite_output::load_records(&db) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error reading {:?}: {}", db, e);
            process::exit(1);
        }
    };

    let groups = videre::output::find_duplicate_groups(&records);
    if !args.silent {
        if groups.is_empty() {
            eprintln!("No exact duplicates found.");
        } else {
            eprintln!(
                "{} duplicate group(s), {} file(s) to remove.",
                groups.len(),
                groups.iter().map(|g| g.files.len() - 1).sum::<usize>()
            );
        }
    }
    videre::output::print_losers(&groups);

    if args.similar {
        let similar = videre::output::find_similar_groups(&records, 10);
        if !args.silent && !similar.is_empty() {
            eprintln!(
                "{} visually similar group(s) found: review with videre report before deleting.",
                similar.len()
            );
        }
    }

    Ok(())
}

fn run_json(args: &DedupeArgs) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let db = super::resolve_reader_db_must_exist(args.db.clone())?;
    super::build_find_duplicates(&db, args.similar)
}
