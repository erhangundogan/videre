//! `videre mark --export-xmp`: write the portable marks (rating, colour label)
//! to `.xmp` sidecars next to each photo. Selection is resolved through the same
//! `mark::resolve_targets` as a set, so export and set always act on the same
//! files.

use crate::command_context::CommandContext;
use anyhow::Result;
use videre_core::marks;

pub fn run(
    args: &super::mark::MarkArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    let hashes = super::mark::resolve_targets(args, ctx, conn)?;
    let mut written = 0usize;
    for h in &hashes {
        let m = marks::get(conn, h)?;
        // No early skip for a photo without marks: its sidecar may still hold a
        // rating cleared since the last export, and leaving it there let the
        // next `scan --xmp file` restore it. The writer creates no sidecar for
        // an empty set, and clears the marks from one that exists.
        let owned = crate::xmp::model::OwnedXmp {
            rating: m.rating,
            label: m.label.clone(),
            ..Default::default()
        };
        // One hash can map to several paths (duplicates); write a sidecar next
        // to each, so a copy in any folder carries the marks too.
        let mut stmt = conn.prepare("SELECT path FROM file_hashes WHERE hash = ?1")?;
        let paths = stmt.query_map([h], |r| r.get::<_, String>(0))?;
        for p in paths {
            let path = std::path::PathBuf::from(p?);
            let side = crate::xmp::write::sidecar_path(&path);
            if owned.is_empty() && !side.exists() {
                continue;
            }
            if args.dry_run {
                tracing::info!("would write {}", side.display());
            } else if crate::xmp::write::write_sidecar_in(
                &ctx.library,
                &path,
                &owned,
                crate::xmp::write::Scope::Marks,
            )? {
                written += 1;
            }
        }
    }
    if !args.silent && !args.dry_run {
        tracing::info!("Wrote {written} sidecar(s)");
    }
    Ok(())
}
