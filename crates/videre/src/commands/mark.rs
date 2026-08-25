use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::PathBuf;
use videre_core::marks::{self, Field, MarkChange, Pick};
use videre_core::selection::SelectionCtx;

#[derive(clap::Args)]
pub struct MarkArgs {
    // Targeting uses the standard selection groups MINUS the mark predicates: a
    // mark flag here means *set*, not *filter*, so allowing both would be
    // circular, exactly as embed/faces exclude --person/--category.
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

    /// Set the star rating (0-5; 0 clears)
    #[arg(long, value_name = "N")]
    rating: Option<i64>,
    /// Set the pick state
    #[arg(long, value_name = "keep|reject|none", value_parser = ["keep", "reject", "none"])]
    pick: Option<String>,
    /// Set the colour label, or 'none' to clear
    #[arg(long, value_name = "COLOUR|none")]
    label: Option<String>,
    /// Mark as liked (a favourite)
    #[arg(long)]
    like: bool,
    /// Remove the like
    #[arg(long, conflicts_with = "like")]
    no_like: bool,

    /// Write standard XMP sidecars for the selection instead of setting marks
    #[arg(long)]
    export_xmp: bool,
    /// Show what would change, write nothing
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    pub(crate) db: Option<PathBuf>,
    /// No per-item output
    #[arg(long)]
    pub(crate) silent: bool,
}

pub fn run(args: MarkArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db).with_context(|| format!("open {}", db.display()))?;

    if args.export_xmp {
        return super::mark_export::run(&args, &conn);
    }

    let change = build_change(&args);
    if !change.any() {
        bail!("nothing to set; give at least one of --rating/--pick/--label/--like/--no-like");
    }

    let hashes = resolve_targets(&args, &conn)?;
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    if !args.silent {
        eprintln!("Marking {} of {} file(s)", hashes.len(), total);
    }
    if !args.dry_run {
        marks::set(&conn, &hashes, &change)?;
    }
    Ok(())
}

fn build_change(a: &MarkArgs) -> MarkChange {
    let liked = if a.like {
        Some(true)
    } else if a.no_like {
        Some(false)
    } else {
        None
    };
    MarkChange {
        rating: a.rating.map(|r| if r == 0 { Field::Clear } else { Field::Set(r) }),
        pick: a.pick.as_deref().map(|p| match p {
            "keep" => Field::Set(Pick::Keep),
            "reject" => Field::Set(Pick::Reject),
            _ => Field::Clear, // "none"
        }),
        label: a.label.as_deref().map(|l| {
            if l == "none" {
                Field::Clear
            } else {
                Field::Set(l.to_string())
            }
        }),
        liked,
    }
}

/// Targets come from stdin (a pipe of paths) when stdin is not a terminal, else
/// from the selection flags. Both resolve to content hashes. Shared by set and
/// `--export-xmp` so the two select the same files.
pub(crate) fn resolve_targets(a: &MarkArgs, conn: &rusqlite::Connection) -> Result<Vec<String>> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let paths: Vec<&str> = buf.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if !paths.is_empty() {
            return hashes_for_paths(conn, &paths);
        }
    }
    let sel = super::selection_args::row_selection(
        Some(&a.media),
        Some(&a.dates),
        Some(&a.place),
        Some(&a.people),
        Some(&a.paths),
    )?;
    let resolved = sel.resolve(conn, &SelectionCtx::default())?;
    match resolved.hashes {
        Some(h) => Ok(h.into_iter().collect()),
        None => all_hashes(conn),
    }
}

fn hashes_for_paths(conn: &rusqlite::Connection, paths: &[&str]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for p in paths {
        if let Ok(h) =
            conn.query_row("SELECT hash FROM file_hashes WHERE path = ?1", [p], |r| {
                r.get::<_, String>(0)
            })
        {
            out.push(h);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn all_hashes(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT hash FROM file_hashes")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
