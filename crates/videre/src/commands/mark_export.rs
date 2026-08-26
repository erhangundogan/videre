//! `videre mark --export-xmp`: write the portable marks (rating, colour label)
//! to `.xmp` sidecars next to each photo. Selection is resolved through the same
//! `mark::resolve_targets` as a set, so export and set always act on the same
//! files.

use anyhow::Result;
use videre_core::marks;

pub fn run(args: &super::mark::MarkArgs, conn: &rusqlite::Connection) -> Result<()> {
    let hashes = super::mark::resolve_targets(args, conn)?;
    let mut written = 0usize;
    for h in &hashes {
        let m = marks::get(conn, h)?;
        if m.rating.is_none() && m.label.is_none() {
            continue;
        }
        // One hash can map to several paths (duplicates); write a sidecar next
        // to each, so a copy in any folder carries the marks too.
        let mut stmt = conn.prepare("SELECT path FROM file_hashes WHERE hash = ?1")?;
        let paths = stmt.query_map([h], |r| r.get::<_, String>(0))?;
        for p in paths {
            let path = std::path::PathBuf::from(p?);
            if args.dry_run {
                eprintln!(
                    "would write {}",
                    crate::xmp::write::sidecar_path(&path).display()
                );
            } else if crate::xmp::write::write_sidecar(&path, m.rating, m.label.as_deref())? {
                written += 1;
            }
        }
    }
    if !args.silent && !args.dry_run {
        eprintln!("Wrote {written} sidecar(s)");
    }
    Ok(())
}
