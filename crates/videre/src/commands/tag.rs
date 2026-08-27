//! `videre tag <selection> add|remove <tag>...`: free-form tags on photos,
//! stored by content hash so they follow a photo across duplicates and moves.
//! Selection resolves the same way `videre mark` does. A `--tag` filter would be
//! circular here (it means "set", not "filter"), so this command does not take
//! one.

use anyhow::{bail, Result};
use std::path::PathBuf;
use videre_core::selection::SelectionCtx;

#[derive(clap::Args)]
pub struct TagArgs {
    /// Add this tag to the selection. Repeatable
    #[arg(long = "add", value_name = "TAG")]
    add: Vec<String>,
    /// Remove this tag from the selection. Repeatable
    #[arg(long = "remove", value_name = "TAG")]
    remove: Vec<String>,

    #[command(flatten)]
    media: super::selection_args::MediaArgs,
    #[command(flatten)]
    dates: super::selection_args::DateArgs,
    #[command(flatten)]
    place: super::selection_args::PlaceArgs,
    #[command(flatten)]
    people: super::selection_args::PeopleArgs,
    #[command(flatten)]
    paths: super::selection_args::PathArgs,

    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,
    /// No per-run output
    #[arg(long)]
    silent: bool,
}

pub fn run(args: TagArgs) -> Result<()> {
    let add: Vec<String> = args
        .add
        .iter()
        .filter(|t| !t.trim().is_empty())
        .cloned()
        .collect();
    let remove: Vec<String> = args
        .remove
        .iter()
        .filter(|t| !t.trim().is_empty())
        .cloned()
        .collect();
    if add.is_empty() && remove.is_empty() {
        bail!("give at least one --add <tag> or --remove <tag>");
    }

    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)?;
    videre_core::tags::ensure_photo_tags_table(&conn)?;

    let sel = super::selection_args::row_selection(
        Some(&args.media),
        Some(&args.dates),
        Some(&args.place),
        Some(&args.people),
        Some(&args.paths),
    )?;
    let resolved = sel.resolve(&conn, &SelectionCtx::default())?;
    let hashes: Vec<String> = match resolved.hashes {
        Some(h) => h.into_iter().collect(),
        None => {
            let mut stmt = conn.prepare("SELECT DISTINCT hash FROM file_hashes")?;
            let rows: Vec<String> = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            rows
        }
    };
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    if !args.silent {
        eprintln!("Tagging {} of {} file(s)", hashes.len(), total);
    }
    // Remove first, then add, so a value in both ends up present.
    if !remove.is_empty() {
        videre_core::tags::remove_tags(&conn, &hashes, &remove)?;
    }
    if !add.is_empty() {
        videre_core::tags::set_tags(&conn, &hashes, &add)?;
    }
    Ok(())
}
