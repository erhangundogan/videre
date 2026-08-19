use std::path::PathBuf;
use std::process;
use videre::types::ErrorJson;

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

    /// Also write the duplicate groups to a browsable HTML page.
    /// Bare --html targets <db>_duplicates.html.
    #[arg(long, num_args = 0..=1)]
    html: Option<Option<PathBuf>>,
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

/// `--html`: the same duplicate groups, as a page you can keep.
///
/// Static on purpose. `videre gallery` is for browsing a library and writes
/// nothing; this renders the set the command just produced, so it survives the
/// process and can be archived or opened later.
fn write_html(
    conn: &rusqlite::Connection,
    arg: Option<&std::path::Path>,
    db: &std::path::Path,
) -> anyhow::Result<()> {
    let output = if let Some(p) = arg {
        p.to_path_buf()
    } else {
        let mut p = db.to_path_buf();
        let stem = db
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        p.set_file_name(format!("{stem}_duplicates.html"));
        p
    };
    let groups = super::report::query_groups(conn);
    super::report::write_static_page(conn, &output, &groups, None)
}

fn run_text(args: DedupeArgs) -> anyhow::Result<()> {
    let db = match super::resolve_reader_db_must_exist(args.db.clone()) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };
    let conn = match videre_core::db::open_wal(&db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error opening {:?}: {}", db, e);
            process::exit(1);
        }
    };

    let result =
        videre_core::pipeline_runs::track(&conn, &db, "dedupe", || run_dedupe_text(&args, &db));
    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        process::exit(1);
    }

    if let Some(arg) = args.html.as_ref() {
        if let Err(e) = write_html(&conn, arg.as_deref(), &db) {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    }
    Ok(())
}

/// The actual dedupe-reporting work, wrapped by `track()` above.
fn run_dedupe_text(args: &DedupeArgs, db: &std::path::Path) -> anyhow::Result<()> {
    let records = videre::sqlite_output::load_records(db)
        .map_err(|e| anyhow::anyhow!("reading {:?}: {}", db, e))?;

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
    let conn = videre_core::db::open_wal(&db)?;
    videre_core::pipeline_runs::track(&conn, &db, "dedupe", || {
        super::build_find_duplicates(&db, args.similar)
    })
}
