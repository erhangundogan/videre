//! Free-form tags, stored many-to-many by content hash so they follow a photo
//! across duplicates and moves, exactly like marks. A tag is one opaque string;
//! a `/`-separated path is stored verbatim with no hierarchy semantics (that is a
//! deferred follow-up). Mirrors the shape of `videre_core::marks`.

use rusqlite::{params, Connection, Result};
use std::collections::HashSet;

pub fn ensure_photo_tags_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS photo_tags (
            hash  TEXT NOT NULL,
            tag   TEXT NOT NULL,
            PRIMARY KEY (hash, tag)
        );
        CREATE INDEX IF NOT EXISTS idx_photo_tags_tag ON photo_tags(tag);",
    )
}

/// Add `tags` to each hash. Idempotent: the primary key makes a repeat a no-op.
/// Blank tags (after trimming) are skipped.
pub fn set_tags(conn: &Connection, hashes: &[String], tags: &[String]) -> Result<()> {
    ensure_photo_tags_table(conn)?;
    for h in hashes {
        for t in tags {
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            conn.execute(
                "INSERT OR IGNORE INTO photo_tags (hash, tag) VALUES (?1, ?2)",
                params![h, t],
            )?;
        }
    }
    Ok(())
}

/// Remove `tags` from each hash. Absent pairs are ignored.
pub fn remove_tags(conn: &Connection, hashes: &[String], tags: &[String]) -> Result<()> {
    for h in hashes {
        for t in tags {
            conn.execute(
                "DELETE FROM photo_tags WHERE hash = ?1 AND tag = ?2",
                params![h, t.trim()],
            )?;
        }
    }
    Ok(())
}

/// Hashes carrying `tag` (exact match).
pub fn by_tag(conn: &Connection, tag: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT hash FROM photo_tags WHERE tag = ?1")?;
    let rows = stmt.query_map([tag], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// One photo's tags, sorted for stable display and stable tests.
pub fn tags_for_hash(conn: &Connection, hash: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT tag FROM photo_tags WHERE hash = ?1 ORDER BY tag")?;
    let rows = stmt.query_map([hash], |r| r.get::<_, String>(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_photo_tags_table(&c).unwrap();
        c
    }

    #[test]
    fn set_and_query_tags() {
        let c = conn();
        set_tags(&c, &["h1".into()], &["beach".into(), "holiday".into()]).unwrap();
        set_tags(&c, &["h2".into()], &["beach".into()]).unwrap();
        let mut got: Vec<String> = by_tag(&c, "beach").unwrap().into_iter().collect();
        got.sort();
        assert_eq!(got, vec!["h1", "h2"]);
        assert_eq!(by_tag(&c, "holiday").unwrap().len(), 1);
    }

    #[test]
    fn set_is_idempotent_no_duplicate_rows() {
        let c = conn();
        set_tags(&c, &["h1".into()], &["beach".into()]).unwrap();
        set_tags(&c, &["h1".into()], &["beach".into()]).unwrap();
        assert_eq!(tags_for_hash(&c, "h1").unwrap(), vec!["beach".to_string()]);
    }

    #[test]
    fn remove_tags_drops_only_named() {
        let c = conn();
        set_tags(&c, &["h1".into()], &["beach".into(), "holiday".into()]).unwrap();
        remove_tags(&c, &["h1".into()], &["beach".into()]).unwrap();
        assert_eq!(
            tags_for_hash(&c, "h1").unwrap(),
            vec!["holiday".to_string()]
        );
    }

    #[test]
    fn blank_tags_are_skipped() {
        let c = conn();
        set_tags(&c, &["h1".into()], &["   ".into(), "ok".into()]).unwrap();
        assert_eq!(tags_for_hash(&c, "h1").unwrap(), vec!["ok".to_string()]);
    }
}
