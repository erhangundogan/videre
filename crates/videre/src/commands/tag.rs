//! `videre tag <selection> add|remove <tag>...`: free-form tags on photos,
//! stored by content hash so they follow a photo across duplicates and moves.
//! Selection resolves the same way `videre mark` does, and accepts the full
//! filter vocabulary including the mark and `--tag` filters: `--add`/`--remove`
//! are the setters, while a `--tag` here *narrows the input set* to files that
//! already carry that tag before adding or removing another. The two never
//! collide because the setters use their own `--add`/`--remove` flags.

use crate::command_context::CommandContext;
use anyhow::{bail, Result};
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
    presence: super::selection_args::PresenceArgs,
    #[command(flatten)]
    paths: super::selection_args::PathArgs,
    #[command(flatten)]
    marks: super::selection_args::MarkArgs,
    #[command(flatten)]
    tags: super::selection_args::TagFilterArgs,

    /// No per-run output
    #[arg(long)]
    silent: bool,
}

pub fn run(args: TagArgs, ctx: &CommandContext) -> Result<()> {
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

    // Guard every --path against the selected root before any table setup or
    // tag mutation.
    videre_core::library_guard::validate_paths(&ctx.library, &args.paths.path)?;
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // Tagging is ordinary shared work, excluded only by exclusive maintenance.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    videre_core::tags::ensure_photo_tags_table(&conn)?;

    let sel = super::selection_args::row_selection(
        Some(&args.media),
        Some(&args.dates),
        Some(&args.place),
        Some(&args.people),
        Some(&args.presence),
        Some(&args.paths),
        Some(&args.marks),
        Some(&args.tags),
    )?;
    let resolved = sel.resolve_in(&conn, &SelectionCtx::default(), &ctx.library)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        a: TagArgs,
    }

    #[test]
    fn tag_accepts_setters_alongside_the_full_filter_vocabulary() {
        // --add/--remove are the setters; the mark and --tag groups are filters
        // that narrow which files are (un)tagged. They land on distinct fields.
        let a = Wrap::try_parse_from([
            "tag",
            "--add",
            "keeper",
            "--remove",
            "old",
            "--label",
            "Green",
            "--tag",
            "other",
            "--rating",
            "5",
            "--person",
            "Ada",
            "--category",
            "photo",
        ])
        .expect("tag must accept its setters together with every filter group")
        .a;
        assert_eq!(a.add, vec!["keeper".to_string()]);
        assert_eq!(a.remove, vec!["old".to_string()]);
        assert_eq!(a.marks.label.as_deref(), Some("Green"));
        assert_eq!(a.marks.rating, Some(5));
        assert_eq!(a.tags.tags, vec!["other".to_string()]);
        assert_eq!(a.people.person.as_deref(), Some("Ada"));
        assert_eq!(a.people.category.as_deref(), Some("photo"));
    }
}
