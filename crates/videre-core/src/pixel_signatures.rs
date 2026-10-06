//! Stored pixel signatures (`image_decode::pixel_signature`), one per content
//! hash, so the resized-copy check decodes each candidate once rather than on
//! every run. Only candidates are ever decoded and stored: pairs whose
//! fingerprints are a few bits apart at different pixel sizes, about 1% of a
//! library.
//!
//! The table is created on demand, like `decode_failures`, so a library from
//! before it needs no schema bump. A row describes pixels, so whatever changes
//! the pixels under an unchanged hash (a gallery rotation) must `forget` it,
//! and prune drops rows whose hash no longer has an indexed path.

use crate::image_decode::PixelSignature;
use rusqlite::Connection;
use std::collections::HashMap;

/// Create the table if it is absent. Idempotent; safe on every open.
pub fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pixel_signatures (
            hash   TEXT PRIMARY KEY,
            luma   BLOB NOT NULL,
            aspect REAL NOT NULL
        );",
    )
}

/// The stored signatures for `hashes`; a hash with none is absent from the
/// map. Empty when the table does not exist yet (a read-only open).
pub fn get_many(
    conn: &Connection,
    hashes: &[String],
) -> rusqlite::Result<HashMap<String, PixelSignature>> {
    let mut out = HashMap::new();
    if hashes.is_empty() || !crate::db::table_exists(conn, "pixel_signatures")? {
        return Ok(out);
    }
    let mut stmt = conn.prepare("SELECT luma, aspect FROM pixel_signatures WHERE hash = ?1")?;
    for hash in hashes {
        let row = stmt.query_row([hash], |r| {
            Ok(PixelSignature {
                luma: r.get(0)?,
                aspect: r.get::<_, f64>(1)? as f32,
            })
        });
        match row {
            Ok(sig) => {
                out.insert(hash.clone(), sig);
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Store (or replace) the signature of `hash`.
pub fn put(conn: &Connection, hash: &str, sig: &PixelSignature) -> rusqlite::Result<()> {
    ensure_table(conn)?;
    conn.execute(
        "INSERT INTO pixel_signatures (hash, luma, aspect) VALUES (?1, ?2, ?3)
         ON CONFLICT(hash) DO UPDATE SET luma = excluded.luma, aspect = excluded.aspect",
        rusqlite::params![hash, sig.luma, sig.aspect as f64],
    )?;
    Ok(())
}

/// Drop the signature of `hash`, if any: its pixels changed under the same key.
pub fn forget(conn: &Connection, hash: &str) -> rusqlite::Result<()> {
    if !crate::db::table_exists(conn, "pixel_signatures")? {
        return Ok(());
    }
    conn.execute("DELETE FROM pixel_signatures WHERE hash = ?1", [hash])?;
    Ok(())
}

/// Drop every signature whose hash has no `file_hashes` row, returning how
/// many went. `dry_run` only counts them.
pub fn prune_orphans(conn: &Connection, dry_run: bool) -> rusqlite::Result<usize> {
    if !crate::db::table_exists(conn, "pixel_signatures")? {
        return Ok(0);
    }
    const ORPHANS: &str = "FROM pixel_signatures WHERE hash NOT IN (SELECT hash FROM file_hashes)";
    if dry_run {
        let n: i64 = conn.query_row(&format!("SELECT COUNT(*) {ORPHANS}"), [], |r| r.get(0))?;
        Ok(n.max(0) as usize)
    } else {
        conn.execute(&format!("DELETE {ORPHANS}"), [])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (path TEXT, hash TEXT);")
            .unwrap();
        conn
    }

    fn sig(v: u8) -> PixelSignature {
        PixelSignature {
            luma: vec![v; 16],
            aspect: 1.5,
        }
    }

    fn hashes(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_is_stored_before_the_table_exists() {
        let conn = db();
        assert!(get_many(&conn, &hashes(&["h"])).unwrap().is_empty());
        forget(&conn, "h").unwrap();
        assert_eq!(prune_orphans(&conn, false).unwrap(), 0);
    }

    #[test]
    fn a_signature_round_trips() {
        let conn = db();
        put(&conn, "çiçek", &sig(7)).unwrap();
        put(&conn, "çiçek", &sig(9)).unwrap();
        let got = get_many(&conn, &hashes(&["çiçek", "deniz"])).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got["çiçek"].luma, vec![9; 16]);
        assert!((got["çiçek"].aspect - 1.5).abs() < 1e-6);
    }

    #[test]
    fn forget_drops_one_hash() {
        let conn = db();
        put(&conn, "çiçek", &sig(1)).unwrap();
        put(&conn, "deniz", &sig(2)).unwrap();
        forget(&conn, "çiçek").unwrap();
        let got = get_many(&conn, &hashes(&["çiçek", "deniz"])).unwrap();
        assert_eq!(got.keys().collect::<Vec<_>>(), vec!["deniz"]);
    }

    #[test]
    fn prune_drops_signatures_of_hashes_with_no_path() {
        let conn = db();
        conn.execute(
            "INSERT INTO file_hashes VALUES ('/kütüphane/deniz.jpg', 'deniz')",
            [],
        )
        .unwrap();
        put(&conn, "çiçek", &sig(1)).unwrap();
        put(&conn, "deniz", &sig(2)).unwrap();
        assert_eq!(prune_orphans(&conn, true).unwrap(), 1);
        assert_eq!(get_many(&conn, &hashes(&["çiçek"])).unwrap().len(), 1);
        assert_eq!(prune_orphans(&conn, false).unwrap(), 1);
        let got = get_many(&conn, &hashes(&["çiçek", "deniz"])).unwrap();
        assert_eq!(got.keys().collect::<Vec<_>>(), vec!["deniz"]);
    }
}
