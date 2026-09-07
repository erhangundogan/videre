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

/// Apply XMP marks for a batch of freshly written records under `prec`. The one
/// loop scan/watch/import all call, so the ingest behaviour is defined once.
/// `newest` is not yet implemented; it warns once and behaves as `db`.
pub fn import_xmp_for_records_in(
    conn: &Connection,
    ctx: &videre_core::library::LibraryContext,
    records: &[videre::types::FileRecord],
    prec: XmpPrecedence,
    silent: bool,
) -> Result<()> {
    if matches!(prec, XmpPrecedence::Newest) && !silent {
        eprintln!("Warning: --xmp newest is not yet implemented; treating as db");
    }
    for record in records {
        import_xmp_for_in(conn, ctx, Path::new(&record.path), &record.hash, prec)?;
    }
    Ok(())
}
