use crate::command_context::CommandContext;
use std::process;
use videre::types::ErrorJson;

#[derive(clap::Args)]
pub struct DedupeArgs {
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
    html: Option<Option<std::path::PathBuf>>,
}

pub fn run(args: DedupeArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args, ctx) {
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
        run_text(args, ctx)
    }
}

/// `--html`: the same duplicate groups, as a page you can keep.
///
/// Static on purpose. `videre gallery` is for browsing a library and writes
/// nothing; this renders the set the command just produced, so it survives the
/// process and can be archived or opened later.
fn write_html(
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    arg: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let db = &ctx.library.paths.db;
    // A bare --html targets a page beside the selected database; an explicit
    // relative path is an operand, resolved against the launch directory.
    let output = match arg {
        Some(p) => ctx.operand(p),
        None => {
            let mut p = db.clone();
            let stem = db
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            p.set_file_name(format!("{stem}_duplicates.html"));
            p
        }
    };
    let groups = crate::render::query_groups(conn);
    crate::render::write_static_page(conn, &output, &groups, None)
}

fn run_text(args: DedupeArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let conn = match videre_core::library_db::open_existing(&ctx.library) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };
    let guard = match videre_core::library_locks::try_command(&ctx.library, "dedupe") {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };

    let result =
        videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "dedupe", || {
            run_dedupe_text(&args, &conn)
        });
    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        process::exit(1);
    }

    if let Some(arg) = args.html.as_ref() {
        if let Err(e) = write_html(ctx, &conn, arg.as_deref()) {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    }
    Ok(())
}

/// The actual dedupe-reporting work, wrapped by `track_in()` above.
fn run_dedupe_text(args: &DedupeArgs, conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let records = videre::sqlite_output::load_records_from(conn)
        .map_err(|e| anyhow::anyhow!("reading the library database: {e}"))?;

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
                "{} visually similar group(s) found: review with videre dedupe --html before deleting.",
                similar.len()
            );
        }
    }

    Ok(())
}

fn run_json(
    args: &DedupeArgs,
    ctx: &CommandContext,
) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "dedupe")?;
    videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "dedupe", || {
        super::build_find_duplicates_from(&conn, args.similar)
    })
}
