use crate::command_context::CommandContext;
use anyhow::{bail, Result};
use std::io::Read;
use videre_core::marks::{self, MarkChange};
use videre_core::selection::SelectionCtx;

#[derive(clap::Args)]
pub struct MarkArgs {
    // Targeting uses the standard selection groups MINUS the mark predicates: a
    // mark flag here means *set*, not *filter*, so allowing both would be
    // circular, exactly as embed/faces exclude --person/--category. The one
    // mark/tag filter it does take is `--tag`, which narrows the target set to
    // already-tagged files before setting a mark; tags are not a mark setter
    // here, so there is no collision.
    #[command(flatten)]
    media: super::selection_args::MediaArgs,
    #[command(flatten)]
    dates: super::selection_args::DateArgs,
    #[command(flatten)]
    place: super::selection_args::PlaceArgs,
    #[command(flatten)]
    people: super::selection_args::PeopleArgs,
    #[command(flatten)]
    presence: super::selection_args::PresenceArgs,
    #[command(flatten)]
    paths: super::selection_args::PathArgs,
    #[command(flatten)]
    tags: super::selection_args::TagFilterArgs,

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

    /// No per-item output
    #[arg(long)]
    pub(crate) silent: bool,
}

pub fn run(args: MarkArgs, ctx: &CommandContext) -> Result<()> {
    // Guard every --path against the selected root before any mutation.
    videre_core::library_guard::validate_paths(&ctx.library, &args.paths.path)?;
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // Marking is ordinary shared work, excluded only by exclusive maintenance.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;

    if args.export_xmp {
        return super::mark_export::run(&args, ctx, &conn);
    }

    let change = build_change(&args);
    if !change.any() {
        bail!("nothing to set; give at least one of --rating/--pick/--label/--like/--no-like");
    }

    let hashes = resolve_targets(&args, ctx, &conn)?;
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
    // Shared with the gallery's POST /api/mark, so the two cannot drift.
    marks::change_from_parts(a.rating, a.pick.as_deref(), a.label.as_deref(), liked)
}

/// Targets come from stdin (a pipe of paths) when stdin is not a terminal, else
/// from the selection flags. Both resolve to content hashes. Shared by set and
/// `--export-xmp` so the two select the same files.
pub(crate) fn resolve_targets(
    a: &MarkArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<Vec<String>> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        let paths: Vec<&str> = buf
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if !paths.is_empty() {
            return hashes_for_paths(conn, &paths);
        }
    }
    let sel = super::selection_args::row_selection(
        Some(&a.media),
        Some(&a.dates),
        Some(&a.place),
        Some(&a.people),
        Some(&a.presence),
        Some(&a.paths),
        None,
        Some(&a.tags),
    )?;
    let resolved = sel.resolve_in(conn, &SelectionCtx::default(), &ctx.library)?;
    match resolved.hashes {
        Some(h) => Ok(h.into_iter().collect()),
        None => all_hashes(conn),
    }
}

fn hashes_for_paths(conn: &rusqlite::Connection, paths: &[&str]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for p in paths {
        if let Ok(h) = conn.query_row("SELECT hash FROM file_hashes WHERE path = ?1", [p], |r| {
            r.get::<_, String>(0)
        }) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        a: MarkArgs,
    }

    #[test]
    fn mark_flags_are_setters_and_tag_is_the_only_filter_it_takes() {
        // --rating/--pick/--label/--like are setters here; --tag is the one
        // mark/tag filter, narrowing the target set. They land on distinct
        // fields and do not collide.
        let a = Wrap::try_parse_from([
            "mark", "--rating", "5", "--pick", "keep", "--label", "Green", "--like", "--tag",
            "vacation",
        ])
        .expect("mark's setters must coexist with the --tag filter")
        .a;
        assert_eq!(a.rating, Some(5));
        assert_eq!(a.pick.as_deref(), Some("keep"));
        assert_eq!(a.label.as_deref(), Some("Green"));
        assert!(a.like);
        assert_eq!(a.tags.tags, vec!["vacation".to_string()]);
    }

    #[test]
    fn mark_like_and_no_like_conflict() {
        assert!(Wrap::try_parse_from(["mark", "--like", "--no-like"]).is_err());
    }

    #[test]
    fn mark_pick_rejects_an_unknown_keyword() {
        assert!(Wrap::try_parse_from(["mark", "--pick", "maybe"]).is_err());
    }
}
