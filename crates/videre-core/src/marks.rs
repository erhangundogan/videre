//! Per-photo marks: rating, pick, colour label, like. One row per photo, keyed
//! by content hash so a mark follows a photo across duplicates and moves.
//!
//! This module is the single implementation of set/get/query, and the only
//! writer of the `marks` table. The `videre mark` command and the gallery API
//! both call it; nothing else writes marks. Its predicates flow through
//! `videre_core::selection` so `search`, `gallery` and MCP get them for free.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;

/// A photo's four marks. Absent means unset.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Marks {
    /// Star rating 1..=5, or None when unrated.
    pub rating: Option<i64>,
    /// The culling decision, or None when undecided.
    pub pick: Option<Pick>,
    /// Colour label string, or None.
    pub label: Option<String>,
    /// Whether the photo is liked (a favourite).
    pub liked: bool,
}

/// The culling decision. Keep and Reject are the two set states; "undecided" is
/// `Option::None` at the field, so there is no third variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Keep,
    Reject,
}

impl Pick {
    pub fn as_bool(self) -> i64 {
        match self {
            Pick::Keep => 1,
            Pick::Reject => 0,
        }
    }
    pub fn from_bool(v: i64) -> Pick {
        if v == 0 {
            Pick::Reject
        } else {
            Pick::Keep
        }
    }
}

/// One field of a partial update. `Set` writes a value, `Clear` removes it.
#[derive(Debug, Clone)]
pub enum Field<T> {
    Set(T),
    Clear,
}

/// A partial update. A field that is `None` is left untouched; `Some(Set)`
/// writes it; `Some(Clear)` removes it. This is what lets
/// `videre mark --rating 5` change only the rating.
#[derive(Debug, Clone, Default)]
pub struct MarkChange {
    pub rating: Option<Field<i64>>,
    pub pick: Option<Field<Pick>>,
    pub label: Option<Field<String>>,
    pub liked: Option<bool>,
}

impl MarkChange {
    /// True if this change would touch at least one field. `videre mark` refuses
    /// a no-op invocation on the strength of this.
    pub fn any(&self) -> bool {
        self.rating.is_some() || self.pick.is_some() || self.label.is_some() || self.liked.is_some()
    }
}

/// Create the marks table if absent. Idempotent, safe on every open, called
/// from `db::open_wal` the same way the `faces`/`people` tables are ensured.
pub fn ensure_marks_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS marks (
            hash         TEXT PRIMARY KEY,
            rating       INTEGER,
            pick         INTEGER,
            label        TEXT,
            liked        INTEGER NOT NULL DEFAULT 0,
            updated_at   TEXT NOT NULL
        );",
    )?;
    Ok(())
}

/// Read one photo's marks. An unmarked photo is `Marks::default()`.
pub fn get(conn: &Connection, hash: &str) -> Result<Marks> {
    let row = conn
        .query_row(
            "SELECT rating, pick, label, liked FROM marks WHERE hash = ?1",
            [hash],
            |r| {
                Ok(Marks {
                    rating: r.get::<_, Option<i64>>(0)?,
                    pick: r.get::<_, Option<i64>>(1)?.map(Pick::from_bool),
                    label: r.get::<_, Option<String>>(2)?,
                    liked: r.get::<_, i64>(3)? != 0,
                })
            },
        )
        .ok();
    Ok(row.unwrap_or_default())
}

/// Apply `change` to every hash. Fields not named are untouched; a `Clear`
/// removes just that field; a row left with no marks is deleted so `marks`
/// never fills with empty rows. Runs in one transaction. `hashes` is a user
/// selection, not the whole library, so the row count is bounded.
pub fn set(conn: &Connection, hashes: &[String], change: &MarkChange) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for h in hashes {
        let mut m = get(&tx, h)?;
        if let Some(f) = &change.rating {
            m.rating = match f {
                Field::Set(v) => Some((*v).clamp(0, 5)),
                Field::Clear => None,
            };
            if m.rating == Some(0) {
                m.rating = None; // 0 means unrated
            }
        }
        if let Some(f) = &change.pick {
            m.pick = match f {
                Field::Set(p) => Some(*p),
                Field::Clear => None,
            };
        }
        if let Some(f) = &change.label {
            m.label = match f {
                Field::Set(s) => Some(s.clone()),
                Field::Clear => None,
            };
        }
        if let Some(v) = change.liked {
            m.liked = v;
        }

        let empty = m.rating.is_none() && m.pick.is_none() && m.label.is_none() && !m.liked;
        if empty {
            tx.execute("DELETE FROM marks WHERE hash = ?1", [h])?;
        } else {
            tx.execute(
                "INSERT INTO marks (hash, rating, pick, label, liked, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
                 ON CONFLICT(hash) DO UPDATE SET
                   rating = ?2, pick = ?3, label = ?4, liked = ?5, updated_at = datetime('now')",
                rusqlite::params![
                    h,
                    m.rating,
                    m.pick.map(Pick::as_bool),
                    m.label,
                    m.liked as i64,
                ],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

// --- predicates, consumed through `RowSelection` -------------------------------

/// Hashes with rating >= `min` (the "4+ stars" semantics).
pub fn by_rating(conn: &Connection, min: i64) -> Result<HashSet<String>> {
    hashes(conn, "SELECT hash FROM marks WHERE rating >= ?1", [min])
}
/// Hashes with exactly this pick state.
pub fn by_pick(conn: &Connection, pick: Pick) -> Result<HashSet<String>> {
    hashes(
        conn,
        "SELECT hash FROM marks WHERE pick = ?1",
        [pick.as_bool()],
    )
}
/// Hashes with exactly this colour label.
pub fn by_label(conn: &Connection, label: &str) -> Result<HashSet<String>> {
    hashes(conn, "SELECT hash FROM marks WHERE label = ?1", [label])
}
/// Hashes that are liked.
pub fn by_liked(conn: &Connection) -> Result<HashSet<String>> {
    hashes(conn, "SELECT hash FROM marks WHERE liked = 1", [])
}

fn hashes<P: rusqlite::Params>(conn: &Connection, sql: &str, p: P) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(p, |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<HashSet<String>>>()?)
}

// --- XMP import ---------------------------------------------------------------

/// How a mark read from a file's XMP is reconciled with a mark already in the
/// db, chosen by `--xmp` on scan/watch/import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XmpPrecedence {
    /// The db wins; XMP only fills marks the db does not already have.
    #[default]
    Db,
    /// The file wins; XMP replaces the db mark.
    File,
    /// The more recently changed wins. Reserved (DEBT:27); callers treat it as
    /// `Db` with a warning until the timestamp signal is trustworthy.
    Newest,
}

impl XmpPrecedence {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "db" => Ok(Self::Db),
            "file" => Ok(Self::File),
            "newest" => Ok(Self::Newest),
            other => anyhow::bail!("unknown --xmp value {other:?}; expected db, file, or newest"),
        }
    }
}

/// Given the marks already in the db and the rating/label read from XMP, produce
/// the change to apply under `prec`, or None to leave the db untouched. Only
/// rating and label are portable; pick and like have no XMP standard.
pub fn import_change(
    existing: &Marks,
    xmp_rating: Option<i64>,
    xmp_label: Option<String>,
    prec: XmpPrecedence,
) -> Option<MarkChange> {
    // `Newest` is treated as `Db` here; the caller warns once. See DEBT:27.
    let file_wins = matches!(prec, XmpPrecedence::File);
    let want = |db_has: bool| file_wins || !db_has;

    let mut c = MarkChange::default();
    if let Some(r) = xmp_rating {
        if want(existing.rating.is_some()) {
            c.rating = Some(Field::Set(r));
        }
    }
    if let Some(l) = xmp_label {
        if want(existing.label.is_some()) {
            c.label = Some(Field::Set(l));
        }
    }
    if c.any() {
        Some(c)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL);")
            .unwrap();
        ensure_marks_table(&c).unwrap();
        c
    }

    #[test]
    fn empty_change_touches_nothing() {
        assert!(!MarkChange::default().any());
    }

    #[test]
    fn a_rating_change_is_a_change() {
        let c = MarkChange {
            rating: Some(Field::Set(4)),
            ..Default::default()
        };
        assert!(c.any());
    }

    #[test]
    fn ensure_marks_table_is_idempotent() {
        let c = mem();
        ensure_marks_table(&c).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM marks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn set_then_get_roundtrips_each_field() {
        let c = mem();
        set(
            &c,
            &["abc".into()],
            &MarkChange {
                rating: Some(Field::Set(4)),
                pick: Some(Field::Set(Pick::Keep)),
                label: Some(Field::Set("red".into())),
                liked: Some(true),
            },
        )
        .unwrap();
        assert_eq!(
            get(&c, "abc").unwrap(),
            Marks {
                rating: Some(4),
                pick: Some(Pick::Keep),
                label: Some("red".into()),
                liked: true
            }
        );
    }

    #[test]
    fn clearing_only_touches_named_fields() {
        let c = mem();
        set(
            &c,
            &["abc".into()],
            &MarkChange {
                rating: Some(Field::Set(5)),
                liked: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        set(
            &c,
            &["abc".into()],
            &MarkChange {
                rating: Some(Field::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        let m = get(&c, "abc").unwrap();
        assert_eq!(m.rating, None);
        assert!(m.liked);
    }

    #[test]
    fn a_row_with_no_marks_left_is_deleted() {
        let c = mem();
        set(
            &c,
            &["abc".into()],
            &MarkChange {
                rating: Some(Field::Set(3)),
                ..Default::default()
            },
        )
        .unwrap();
        set(
            &c,
            &["abc".into()],
            &MarkChange {
                rating: Some(Field::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM marks WHERE hash='abc'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0, "an all-clear row must be removed");
    }

    #[test]
    fn get_of_unmarked_is_default() {
        let c = mem();
        assert_eq!(get(&c, "nope").unwrap(), Marks::default());
    }

    #[test]
    fn by_rating_is_at_least() {
        let c = mem();
        set(
            &c,
            &["a".into()],
            &MarkChange {
                rating: Some(Field::Set(5)),
                ..Default::default()
            },
        )
        .unwrap();
        set(
            &c,
            &["b".into()],
            &MarkChange {
                rating: Some(Field::Set(3)),
                ..Default::default()
            },
        )
        .unwrap();
        let hit = by_rating(&c, 4).unwrap();
        assert!(
            hit.contains("a") && !hit.contains("b"),
            "--rating 4 means >= 4"
        );
    }

    #[test]
    fn by_pick_and_liked_are_exact() {
        let c = mem();
        set(
            &c,
            &["k".into()],
            &MarkChange {
                pick: Some(Field::Set(Pick::Keep)),
                ..Default::default()
            },
        )
        .unwrap();
        set(
            &c,
            &["r".into()],
            &MarkChange {
                pick: Some(Field::Set(Pick::Reject)),
                ..Default::default()
            },
        )
        .unwrap();
        set(
            &c,
            &["l".into()],
            &MarkChange {
                liked: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            by_pick(&c, Pick::Reject)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["r"]
        );
        assert_eq!(
            by_liked(&c).unwrap().into_iter().collect::<Vec<_>>(),
            vec!["l"]
        );
    }

    #[test]
    fn import_db_precedence_fills_gaps_only() {
        // db already has a rating: db wins, so no change for rating; label is a gap, so filled.
        let existing = Marks {
            rating: Some(5),
            ..Default::default()
        };
        let c = import_change(&existing, Some(2), Some("Red".into()), XmpPrecedence::Db).unwrap();
        assert!(c.rating.is_none(), "db rating kept");
        assert!(matches!(c.label, Some(Field::Set(ref s)) if s == "Red"));
    }

    #[test]
    fn import_file_precedence_overwrites() {
        let existing = Marks {
            rating: Some(5),
            ..Default::default()
        };
        let c = import_change(&existing, Some(2), None, XmpPrecedence::File).unwrap();
        assert!(matches!(c.rating, Some(Field::Set(2))));
    }

    #[test]
    fn import_nothing_to_do_is_none() {
        let existing = Marks {
            rating: Some(5),
            label: Some("Red".into()),
            ..Default::default()
        };
        assert!(
            import_change(&existing, Some(2), Some("Blue".into()), XmpPrecedence::Db).is_none()
        );
    }
}
