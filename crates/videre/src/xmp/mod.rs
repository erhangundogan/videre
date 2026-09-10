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

/// What a reconcile pass should do for one row, decided without reading the
/// media file. See `decide_reconcile`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ReconcileAction {
    /// Nothing changed since the last reconcile: read nothing.
    Skip,
    /// The sidecar changed (appeared, was edited, or removed) but the media did
    /// not: read only the sidecar, never the media.
    SidecarOnly,
    /// The media changed or was never reconciled, or the caller forces it: read
    /// the sidecar and, if absent, the embedded packet (full `read_data_in`).
    Full,
}

/// Decide the reconcile action for one row purely from the precedence, whether
/// this run already reprocessed the file's bytes, the stored sidecar state
/// (`None` for a SQL NULL), and the sidecar's current state (`""` when there is
/// no sidecar, else its rfc3339 mtime). Pure and total, so it is table-tested.
pub fn decide_reconcile(
    prec: XmpPrecedence,
    media_changed: bool,
    stored: Option<&str>,
    current: &str,
) -> ReconcileAction {
    // --xmp file / newest are explicit "reconcile from the file" requests and
    // must bypass the skip, or the revert stops working.
    if matches!(prec, XmpPrecedence::File | XmpPrecedence::Newest) {
        return ReconcileAction::Full;
    }
    if media_changed {
        return ReconcileAction::Full;
    }
    match stored {
        None => ReconcileAction::Full,
        Some(s) if s == current => ReconcileAction::Skip,
        Some(_) => ReconcileAction::SidecarOnly,
    }
}

/// The sidecar's current state string for change detection: its rfc3339 mtime
/// when the sidecar exists, else `""` (absent). Canonicalized through
/// `mtime_iso`, the same form the DB stores, so equal states compare as equal
/// strings. A sidecar that exists but cannot be stat'd reads as `""`, so a
/// transient stat failure at worst triggers one extra sidecar-only read.
pub fn current_sidecar_state(path: &Path) -> String {
    let side = crate::xmp::write::sidecar_path(path);
    match std::fs::metadata(&side).and_then(|m| m.modified()) {
        Ok(t) => videre_core::db::mtime_iso(t),
        Err(_) => String::new(),
    }
}

/// Reconcile XMP for the library incrementally. A row is reconciled only when
/// its media changed this run (`changed` holds its path) or its sidecar changed
/// since the last reconcile; otherwise it is skipped without any read. `--xmp
/// file`/`newest` force a full reconcile of every row (the revert path). The
/// per-row decision is `decide_reconcile`; the current state is
/// `current_sidecar_state`. `xmp_sidecar_mtime` is updated to the current state
/// after any read, so the next run can skip an unchanged file.
///
/// This replaces the pre-incremental whole-library reconcile: that phase began
/// after the scan progress bar had reached 100% and re-read every file's XMP
/// (and, without a sidecar, its entire bytes) on every run, so a large library
/// looked frozen and an unchanged rescan paid the full cost. Now the phase
/// counts only the files that actually need a read, so an unchanged library
/// reads nothing and prints no metadata line at all.
pub fn reconcile_xmp_in(
    conn: &Connection,
    ctx: &videre_core::library::LibraryContext,
    prec: XmpPrecedence,
    changed: &std::collections::HashSet<String>,
    silent: bool,
) -> Result<()> {
    if matches!(prec, XmpPrecedence::Newest) && !silent {
        eprintln!("Warning: --xmp newest is not yet implemented; treating as db");
    }
    if !videre_core::db::table_exists(conn, "file_hashes")? {
        return Ok(());
    }
    let mut stmt = conn.prepare("SELECT path, hash, xmp_sidecar_mtime FROM file_hashes")?;
    let rows: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    // Decide first (a cheap stat per row), so the progress phase counts only the
    // files that actually need a read. On an unchanged library this is zero and
    // the whole phase is silent and instant.
    struct Work {
        path: String,
        hash: String,
        action: ReconcileAction,
        current: String,
    }
    let work: Vec<Work> = rows
        .into_iter()
        .map(|(path, hash, stored)| {
            let current = current_sidecar_state(Path::new(&path));
            let media_changed = changed.contains(&path);
            let action = decide_reconcile(prec, media_changed, stored.as_deref(), &current);
            Work {
                path,
                hash,
                action,
                current,
            }
        })
        .collect();

    let todo = work
        .iter()
        .filter(|w| w.action != ReconcileAction::Skip)
        .count();
    if !silent && todo > 0 {
        eprintln!("Reading metadata for {todo} file(s)");
    }
    let progress = videre_core::progress::Progress::new_counting(todo as u64, silent, "files");
    for w in &work {
        let data = match w.action {
            ReconcileAction::Skip => continue,
            ReconcileAction::Full => read::read_data_in(ctx, Path::new(&w.path)),
            ReconcileAction::SidecarOnly => read::read_sidecar_in(ctx, Path::new(&w.path)),
        };
        apply_xmp_data(conn, &w.hash, data, prec)?;
        conn.execute(
            "UPDATE file_hashes SET xmp_sidecar_mtime = ?1 WHERE path = ?2",
            rusqlite::params![w.current, w.path],
        )?;
        progress.tick();
    }
    progress.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use videre_core::marks::XmpPrecedence::{Db, File, Newest};

    #[test]
    fn decide_reconcile_covers_every_case() {
        // Explicit precedence always forces a full reconcile, whatever the state.
        assert_eq!(
            decide_reconcile(File, false, Some("t"), "t"),
            ReconcileAction::Full
        );
        assert_eq!(
            decide_reconcile(Newest, false, Some("t"), "t"),
            ReconcileAction::Full
        );

        // Db + media changed: full reconcile as part of processing the file.
        assert_eq!(
            decide_reconcile(Db, true, Some("t"), "t"),
            ReconcileAction::Full
        );

        // Db + never reconciled (NULL): one full reconcile (fresh or pre-upgrade).
        assert_eq!(decide_reconcile(Db, false, None, ""), ReconcileAction::Full);
        assert_eq!(
            decide_reconcile(Db, false, None, "t"),
            ReconcileAction::Full
        );

        // Db + unchanged: skip. Both "no sidecar, still none" and "same mtime".
        assert_eq!(
            decide_reconcile(Db, false, Some(""), ""),
            ReconcileAction::Skip
        );
        assert_eq!(
            decide_reconcile(Db, false, Some("t"), "t"),
            ReconcileAction::Skip
        );

        // Db + sidecar changed / appeared / removed: sidecar-only read.
        assert_eq!(
            decide_reconcile(Db, false, Some("t1"), "t2"),
            ReconcileAction::SidecarOnly
        );
        assert_eq!(
            decide_reconcile(Db, false, Some(""), "t"),
            ReconcileAction::SidecarOnly
        );
        assert_eq!(
            decide_reconcile(Db, false, Some("t"), ""),
            ReconcileAction::SidecarOnly
        );
    }

    #[test]
    fn current_sidecar_state_is_empty_when_absent_and_a_timestamp_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("IMG.jpg");

        // No sidecar yet.
        assert_eq!(current_sidecar_state(&photo), "");

        // Write the sidecar; state becomes a non-empty rfc3339 mtime that matches
        // mtime_iso of the file's modified time.
        let side = crate::xmp::write::sidecar_path(&photo);
        std::fs::write(&side, b"<x:xmpmeta/>").unwrap();
        let state = current_sidecar_state(&photo);
        assert!(
            !state.is_empty(),
            "a present sidecar must yield a timestamp"
        );
        let want =
            videre_core::db::mtime_iso(std::fs::metadata(&side).unwrap().modified().unwrap());
        assert_eq!(state, want);
    }
}
