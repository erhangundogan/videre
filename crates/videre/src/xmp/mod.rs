//! XMP sidecar and embedded-packet handling for photo marks. One reader/writer
//! pair, shared by scan/watch/import (read) and `mark --export-xmp` (write), so
//! there is no second XMP parser anywhere in the tree.

pub mod model;
pub mod read;
pub mod readback;
pub mod write;

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use videre_core::marks::{self, XmpPrecedence};

/// The shared `--xmp <db|file|newest>` flag for scan, watch and import. Resolves
/// to a precedence: the flag if given, else the config default, else `db`.
#[derive(clap::Args, Default)]
pub struct XmpArg {
    /// How ratings/labels already in files interact with the database: db (the
    /// database wins), file (the file wins), or newest. Default from config,
    /// else db.
    #[arg(long = "xmp", value_name = "db|file|newest", value_parser = ["db", "file", "newest"])]
    xmp: Option<String>,
}

impl XmpArg {
    pub fn resolve_from(
        &self,
        config: &videre_core::library_config::LibraryConfig,
    ) -> Result<XmpPrecedence> {
        match &self.xmp {
            Some(value) => XmpPrecedence::parse(value),
            None => Ok(config.xmp_precedence),
        }
    }
}

/// Read a photo's XMP and fold it into the database under `prec`. Shared by scan,
/// watch and every import path so all three apply XMP identically. Marks (rating
/// and label) obey `prec`; `dc:subject` keywords become tags additively (a set,
/// so precedence does not apply and re-import is idempotent). Best-effort read: a
/// read or parse failure yields no change, never an error.
pub fn import_xmp_for_in(
    conn: &Connection,
    ctx: &videre_core::library::LibraryContext,
    path: &Path,
    hash: &str,
    prec: XmpPrecedence,
) -> Result<()> {
    apply_xmp_data(conn, hash, read::read_data_in(ctx, path), prec)
}

fn apply_xmp_data(
    conn: &Connection,
    hash: &str,
    data: read::XmpData,
    prec: XmpPrecedence,
) -> Result<()> {
    if data.rating.is_some() || data.label.is_some() {
        let existing = marks::get(conn, hash)?;
        if let Some(change) = marks::import_change(&existing, data.rating, data.label, prec) {
            marks::set(conn, std::slice::from_ref(&hash.to_string()), &change)?;
        }
    }
    if !data.keywords.is_empty() {
        videre_core::tags::set_tags(
            conn,
            std::slice::from_ref(&hash.to_string()),
            &data.keywords,
        )?;
    }
    Ok(())
}

/// Reconcile XMP for every file recorded in the library, not only the ones a
/// run just hashed.
///
/// Scan is incremental and skips files whose bytes are unchanged, but a
/// sidecar's marks can change without the media file changing, and `--xmp file`
/// is an explicit request to reconcile from sidecars. Keying the XMP pass on the
/// stored rows (each carries the hash marks are stored under) keeps XMP
/// behaviour exactly as it was before scanning became incremental: the reconcile
/// covers the whole library every run, independent of the hash skip. Cheap,
/// because a file with no sidecar is a single failed `open`.
pub fn import_xmp_all_in(
    conn: &Connection,
    ctx: &videre_core::library::LibraryContext,
    prec: XmpPrecedence,
    silent: bool,
) -> Result<()> {
    if matches!(prec, XmpPrecedence::Newest) && !silent {
        eprintln!("Warning: --xmp newest is not yet implemented; treating as db");
    }
    if !videre_core::db::table_exists(conn, "file_hashes")? {
        return Ok(());
    }
    let mut stmt = conn.prepare("SELECT path, hash FROM file_hashes")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // This reconcile reads XMP for every row in the library, so it is a slow
    // phase that begins after the scan progress bar has already reached 100%
    // and been cleared. Without its own indicator the whole run looks frozen
    // here (worst on a large library, and it happens even on an unchanged
    // rescan). Announce the phase and give it a progress bar of its own,
    // gated by `silent` like every other progress surface.
    if !silent && !rows.is_empty() {
        eprintln!("Reading metadata for {} file(s)", rows.len());
    }
    let progress =
        videre_core::progress::Progress::new_counting(rows.len() as u64, silent, "files");
    for (path, hash) in &rows {
        import_xmp_for_in(conn, ctx, Path::new(path), hash, prec)?;
        progress.tick();
    }
    progress.finish();
    Ok(())
}
