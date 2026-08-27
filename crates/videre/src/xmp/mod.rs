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
    pub fn precedence(&self) -> Result<XmpPrecedence> {
        let s = match &self.xmp {
            Some(s) => s.clone(),
            None => videre_core::home::videre_home()
                .ok()
                .and_then(|h| videre_core::home::load_config(&h).ok())
                .and_then(|c| c.xmp_precedence)
                .unwrap_or_else(|| "db".to_string()),
        };
        XmpPrecedence::parse(&s)
    }
}

/// Read a photo's XMP and fold it into the database under `prec`. Shared by scan,
/// watch and every import path so all three apply XMP identically. Marks (rating
/// and label) obey `prec`; `dc:subject` keywords become tags additively (a set,
/// so precedence does not apply and re-import is idempotent). Best-effort read: a
/// read or parse failure yields no change, never an error.
pub fn import_xmp_for(
    conn: &Connection,
    path: &Path,
    hash: &str,
    prec: XmpPrecedence,
) -> Result<()> {
    let data = read::read_data(path);
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
/// `newest` is not yet implemented (DEBT:27); it warns once and behaves as `db`.
pub fn import_xmp_for_records(
    conn: &Connection,
    records: &[videre::types::FileRecord],
    prec: XmpPrecedence,
    silent: bool,
) -> Result<()> {
    if matches!(prec, XmpPrecedence::Newest) && !silent {
        eprintln!("Warning: --xmp newest is not yet implemented; treating as db");
    }
    for r in records {
        import_xmp_for(conn, Path::new(&r.path), &r.hash, prec)?;
    }
    Ok(())
}
