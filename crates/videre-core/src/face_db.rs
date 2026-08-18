use half::f16;
use rusqlite::Connection;
use std::collections::HashMap;

pub struct FaceRow {
    pub hash: String,
    pub bbox: String,
    pub landmark: Option<String>,
    pub embedding: Vec<u8>, // 512 f16 values as little-endian bytes (1024 bytes)
    pub cluster_id: Option<i64>,
    pub person_label: Option<String>,
    pub confirmed: i64,
    pub is_primary: i64,
}

/// Creates the `people` table if it is missing.
///
/// Called from `db::open_wal`, so it runs on **every** open rather than only
/// when faces are written. That is the same reason `ensure_file_hashes_columns`
/// is there: readers query this table - the labeling UI lists people, and
/// `--person` resolves through it - and a library whose last `videre faces` run
/// predates this table would otherwise fail with "no such table" on commands
/// that never write faces.
pub fn ensure_people_table(conn: &Connection) {
    // One row per person: `name` is the identity form (see
    // `videre_core::person::normalize`) and `full_name` is what a reader sees.
    // `faces.person_label` holds the identity form and refers here.
    //
    // `name` is the primary key deliberately. It puts "two people cannot share
    // an identity" in the database rather than in whichever code path remembers
    // to check - `rename_person` checks by hand today and `assign` does not.
    //
    // No foreign key from `faces`: SQLite leaves `PRAGMA foreign_keys` off and
    // videre never sets it, so a `REFERENCES` clause here would be
    // documentation rather than a constraint, and code written to trust it
    // would be wrong. Tracked separately.
    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS people (
            name       TEXT PRIMARY KEY,
            full_name  TEXT NOT NULL
        );",
    );
}

pub fn create_faces_table(conn: &Connection) -> rusqlite::Result<()> {
    ensure_people_table(conn);
    // Writers migrate; readers do not. `open_wal` only creates the empty table,
    // because `stats` and `search` open databases they must not write to - a
    // read-only mount or another process holding the writer lock would turn a
    // report into a failure. This runs from the commands that already write
    // faces, and is a single COUNT once the migration has happened.
    let _ = migrate_person_labels(conn);
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS faces (
            id            INTEGER PRIMARY KEY,
            hash          TEXT NOT NULL,
            bbox          TEXT NOT NULL,
            landmark      TEXT,
            embedding     BLOB NOT NULL,
            cluster_id    INTEGER,
            person_label  TEXT,
            confirmed     INTEGER DEFAULT 0,
            is_primary    INTEGER DEFAULT 0
        );",
    )?;
    // Migration for existing tables without is_primary column; ignored if already exists.
    let _ = conn.execute_batch("ALTER TABLE faces ADD COLUMN is_primary INTEGER DEFAULT 0");

    // Records every hash whose faces have been scanned, INCLUDING images where
    // zero faces were detected (which leave no `faces` row). This is what makes
    // `videre faces` resumable: the skip set is "already scanned", not merely
    // "has a face", so a no-face image is never re-detected on a later run.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS faces_scanned (
            hash        TEXT PRIMARY KEY,
            scanned_at  TEXT DEFAULT (datetime('now'))
        );",
    )?;
    Ok(())
}

/// Marks a hash as face-scanned (idempotent). Call after detection runs for a
/// hash regardless of whether any faces were found.
pub fn mark_scanned(conn: &Connection, hash: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO faces_scanned (hash) VALUES (?1)",
        rusqlite::params![hash],
    )?;
    Ok(())
}

/// Every hash recorded as face-scanned.
pub fn scanned_hashes(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT hash FROM faces_scanned")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// From `(path, hash)` pairs, drop hashes in `skip`, keep one representative
/// path per remaining hash (first seen), preserving input order, and cap the
/// result at `limit` distinct hashes (`None` = no cap). Used to build the work
/// list for a resumable, optionally partial face-detection pass.
pub fn select_unscanned(
    all: &[(String, String)],
    skip: &std::collections::HashSet<String>,
    limit: Option<usize>,
) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (path, hash) in all {
        if skip.contains(hash) || !seen.insert(hash.clone()) {
            continue;
        }
        out.push((path.clone(), hash.clone()));
        if let Some(n) = limit {
            if out.len() >= n {
                break;
            }
        }
    }
    out
}

pub fn replace_faces_for_hash(
    conn: &Connection,
    hash: &str,
    faces: &[FaceRow],
) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN")?;
    let result = (|| -> rusqlite::Result<()> {
        conn.execute("DELETE FROM faces WHERE hash = ?1", rusqlite::params![hash])?;
        for face in faces {
            conn.execute(
                "INSERT INTO faces (hash, bbox, landmark, embedding, cluster_id, person_label, confirmed, is_primary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    face.hash, face.bbox, face.landmark, face.embedding,
                    face.cluster_id, face.person_label, face.confirmed, face.is_primary
                ],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

pub fn load_face_embeddings(conn: &Connection) -> rusqlite::Result<Vec<(i64, Vec<f32>)>> {
    let mut stmt = conn.prepare("SELECT id, embedding FROM faces")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        Ok((id, blob))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, blob) = row?;
        let emb: Vec<f32> = blob
            .chunks_exact(2)
            .map(|b| f16::from_le_bytes([b[0], b[1]]).to_f32())
            .collect();
        out.push((id, emb));
    }
    Ok(out)
}

/// Like [`load_face_embeddings`] but also returns each face's smaller bbox
/// side in pixels (the shorter of width/height), parsed from the `"x,y,w,h"`
/// bbox string. Used as a quality signal: very small face crops embed into
/// near-degenerate ArcFace vectors that cluster together regardless of
/// identity, so callers gate them out of clustering. A bbox that fails to
/// parse yields a min-side of 0.0 (treated as lowest quality).
pub fn load_faces_for_clustering(conn: &Connection) -> rusqlite::Result<Vec<(i64, Vec<f32>, f32)>> {
    let mut stmt = conn.prepare("SELECT id, embedding, bbox FROM faces")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        let bbox: String = row.get(2)?;
        Ok((id, blob, bbox))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, blob, bbox) = row?;
        let emb: Vec<f32> = blob
            .chunks_exact(2)
            .map(|b| f16::from_le_bytes([b[0], b[1]]).to_f32())
            .collect();
        out.push((id, emb, bbox_min_side(&bbox)));
    }
    Ok(out)
}

/// Smaller side (min of width, height) of a `"x,y,w,h"` bbox string, or 0.0 if
/// it does not parse into at least four numeric fields.
fn bbox_min_side(bbox: &str) -> f32 {
    let nums: Vec<f32> = bbox
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if nums.len() >= 4 {
        nums[2].min(nums[3])
    } else {
        0.0
    }
}

pub fn update_cluster_assignments(
    conn: &Connection,
    assignments: &[(i64, Option<i64>)],
) -> rusqlite::Result<()> {
    for (face_id, cluster_id) in assignments {
        conn.execute(
            "UPDATE faces SET cluster_id = ?1 WHERE id = ?2",
            rusqlite::params![cluster_id, face_id],
        )?;
    }
    Ok(())
}

pub fn hashes_with_faces(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT hash FROM faces ORDER BY hash")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// (face_id, person_label, bbox) for one labeled face.
pub type LabeledFace = (i64, String, String);

/// Maps a file hash to every labeled face on it, as returned by
/// `labeled_faces_by_hash`.
pub type LabeledFacesByHash = HashMap<String, Vec<LabeledFace>>;

/// Returns, for every hash that has at least one confirmed+labeled face, the
/// list of (face_id, person_label, bbox) for that hash. One batched query
/// covering every hash, not one query per file, safe to call once per
/// report generation without N+1 overhead.
pub fn labeled_faces_by_hash(conn: &Connection) -> rusqlite::Result<LabeledFacesByHash> {
    let mut stmt = conn.prepare(
        "SELECT hash, id, bbox, person_label FROM faces \
         WHERE confirmed = 1 AND person_label IS NOT NULL \
         ORDER BY hash, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut map: LabeledFacesByHash = HashMap::new();
    for row in rows {
        let (hash, id, bbox, label) = row?;
        map.entry(hash).or_default().push((id, label, bbox));
    }
    Ok(map)
}

#[cfg(test)]
fn make_embedding(vals: &[f32]) -> Vec<u8> {
    vals.iter()
        .flat_map(|&v| f16::from_f32(v).to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_faces_table(&conn).unwrap();
        conn
    }

    #[test]
    fn create_table_idempotent() {
        let conn = open();
        create_faces_table(&conn).unwrap();
    }

    #[test]
    fn insert_and_load_embedding() {
        let conn = open();
        let emb = make_embedding(&vec![0.5f32; 512]);
        replace_faces_for_hash(
            &conn,
            "habc",
            &[FaceRow {
                hash: "habc".into(),
                bbox: "0,0,50,50".into(),
                landmark: None,
                embedding: emb,
                cluster_id: None,
                person_label: None,
                confirmed: 0,
                is_primary: 0,
            }],
        )
        .unwrap();
        let rows = load_face_embeddings(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        let (id, emb_f32) = &rows[0];
        assert!(*id > 0);
        assert_eq!(emb_f32.len(), 512);
        assert!((emb_f32[0] - 0.5).abs() < 0.01);
    }

    #[test]
    fn replace_removes_old_rows_for_same_hash() {
        let conn = open();
        let emb = make_embedding(&vec![0.0f32; 512]);
        replace_faces_for_hash(
            &conn,
            "h1",
            &[
                FaceRow {
                    hash: "h1".into(),
                    bbox: "0,0,10,10".into(),
                    landmark: None,
                    embedding: emb.clone(),
                    cluster_id: None,
                    person_label: None,
                    confirmed: 0,
                    is_primary: 0,
                },
                FaceRow {
                    hash: "h1".into(),
                    bbox: "20,0,10,10".into(),
                    landmark: None,
                    embedding: emb.clone(),
                    cluster_id: None,
                    person_label: None,
                    confirmed: 0,
                    is_primary: 0,
                },
            ],
        )
        .unwrap();
        replace_faces_for_hash(
            &conn,
            "h1",
            &[FaceRow {
                hash: "h1".into(),
                bbox: "99,0,10,10".into(),
                landmark: None,
                embedding: emb,
                cluster_id: None,
                person_label: None,
                confirmed: 0,
                is_primary: 0,
            }],
        )
        .unwrap();
        let rows = load_face_embeddings(&conn).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn update_cluster_assignments_works() {
        let conn = open();
        let emb = make_embedding(&vec![0.0f32; 512]);
        replace_faces_for_hash(
            &conn,
            "h1",
            &[FaceRow {
                hash: "h1".into(),
                bbox: "0,0,10,10".into(),
                landmark: None,
                embedding: emb,
                cluster_id: None,
                person_label: None,
                confirmed: 0,
                is_primary: 0,
            }],
        )
        .unwrap();
        let rows = load_face_embeddings(&conn).unwrap();
        let id = rows[0].0;
        update_cluster_assignments(&conn, &[(id, Some(3))]).unwrap();
        let n: i64 = conn
            .query_row("SELECT cluster_id FROM faces WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn load_faces_for_clustering_returns_bbox_min_side() {
        let conn = open();
        let emb = make_embedding(&vec![0.25f32; 512]);
        // bbox "x,y,w,h": min side is min(w,h).
        replace_faces_for_hash(
            &conn,
            "h1",
            &[
                FaceRow {
                    hash: "h1".into(),
                    bbox: "10,10,200,300".into(),
                    landmark: None,
                    embedding: emb.clone(),
                    cluster_id: None,
                    person_label: None,
                    confirmed: 0,
                    is_primary: 0,
                },
                FaceRow {
                    hash: "h1".into(),
                    bbox: "0,0,40,25".into(),
                    landmark: None,
                    embedding: emb,
                    cluster_id: None,
                    person_label: None,
                    confirmed: 0,
                    is_primary: 0,
                },
            ],
        )
        .unwrap();
        let mut rows = load_faces_for_clustering(&conn).unwrap();
        rows.sort_by(|a, b| b.2.total_cmp(&a.2));
        assert_eq!(rows[0].2, 200.0, "min side of 200x300 bbox");
        assert_eq!(rows[1].2, 25.0, "min side of 40x25 bbox");
        assert_eq!(rows[0].1.len(), 512, "embedding still decoded");
    }

    #[test]
    fn mark_scanned_records_hash_even_with_zero_faces() {
        let conn = open();
        // A hash processed with no detected faces leaves no `faces` row, but
        // must still be recorded as scanned so it is not re-processed.
        mark_scanned(&conn, "noface").unwrap();
        assert_eq!(scanned_hashes(&conn).unwrap(), vec!["noface".to_string()]);
        // hashes_with_faces stays empty, the marker is independent of faces.
        assert!(hashes_with_faces(&conn).unwrap().is_empty());
    }

    #[test]
    fn mark_scanned_is_idempotent() {
        let conn = open();
        mark_scanned(&conn, "h").unwrap();
        mark_scanned(&conn, "h").unwrap();
        assert_eq!(scanned_hashes(&conn).unwrap().len(), 1);
    }

    #[test]
    fn select_unscanned_skips_dedups_and_limits() {
        // Two paths share hash "a"; "b" is skipped; "c","d","e" remain.
        let all = vec![
            ("/1.jpg".to_string(), "a".to_string()),
            ("/1copy.jpg".to_string(), "a".to_string()),
            ("/2.jpg".to_string(), "b".to_string()),
            ("/3.jpg".to_string(), "c".to_string()),
            ("/4.jpg".to_string(), "d".to_string()),
            ("/5.jpg".to_string(), "e".to_string()),
        ];
        let skip: std::collections::HashSet<String> = ["b".to_string()].into_iter().collect();
        // No limit: one path per unscanned hash (a,c,d,e), b excluded.
        let out = select_unscanned(&all, &skip, None);
        assert_eq!(
            out.iter().map(|(_, h)| h.clone()).collect::<Vec<_>>(),
            vec!["a", "c", "d", "e"]
        );
        // Limit 2: first two unscanned hashes only.
        let out2 = select_unscanned(&all, &skip, Some(2));
        assert_eq!(
            out2.iter().map(|(_, h)| h.clone()).collect::<Vec<_>>(),
            vec!["a", "c"]
        );
    }

    #[test]
    fn hashes_with_faces_returns_inserted_hash() {
        let conn = open();
        let emb = make_embedding(&vec![0.0f32; 512]);
        replace_faces_for_hash(
            &conn,
            "myhash",
            &[FaceRow {
                hash: "myhash".into(),
                bbox: "0,0,10,10".into(),
                landmark: None,
                embedding: emb,
                cluster_id: None,
                person_label: None,
                confirmed: 0,
                is_primary: 0,
            }],
        )
        .unwrap();
        let hashes = hashes_with_faces(&conn).unwrap();
        assert_eq!(hashes, vec!["myhash"]);
    }

    #[test]
    fn labeled_faces_by_hash_returns_only_confirmed_labeled() {
        let conn = Connection::open_in_memory().unwrap();
        create_faces_table(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
             VALUES ('h1', '0,0,10,10', X'0000', 'Alice', 1); \
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
             VALUES ('h1', '20,20,10,10', X'0000', NULL, 0); \
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
             VALUES ('h2', '0,0,10,10', X'0000', 'Bob', 1);",
        )
        .unwrap();

        let map = labeled_faces_by_hash(&conn).unwrap();
        assert_eq!(map.len(), 2, "expected two hashes with labeled faces");
        let h1 = &map["h1"];
        assert_eq!(h1.len(), 1, "unconfirmed/unlabeled face must be excluded");
        assert_eq!(h1[0].1, "Alice");
        assert_eq!(map["h2"][0].1, "Bob");
    }
}

/// One-off: give every existing `person_label` an identity and a display name.
///
/// Before this, a person was a single string on every face row, compared with
/// `=`. `alice` and `Alice` were two people. Afterwards `faces.person_label`
/// holds the identity form and `people` holds what to show.
///
/// Idempotent and guarded: it runs only when `people` is empty and labelled
/// faces exist, so a second call does nothing. It is the one irreversible step
/// in this change, so it reports what it did rather than working silently.
///
/// Returns `(people, merged)` - how many people exist afterwards, and how many
/// labels collapsed into an existing one.
pub fn migrate_person_labels(conn: &Connection) -> rusqlite::Result<(usize, usize)> {
    ensure_people_table(conn);

    let already: i64 = conn.query_row("SELECT COUNT(*) FROM people", [], |r| r.get(0))?;
    if already > 0 {
        return Ok((already as usize, 0));
    }

    // (label, face count), most-used first: when two labels collapse to one
    // identity, the more-used spelling wins the display name. A tie falls to
    // the one containing an uppercase letter, which is more likely to be the
    // proper noun someone typed deliberately.
    let mut labels: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT person_label, COUNT(*) FROM faces \
             WHERE person_label IS NOT NULL AND person_label <> '' \
             GROUP BY person_label",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if labels.is_empty() {
        return Ok((0, 0));
    }
    labels.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(
                b.0.chars()
                    .any(|c| c.is_uppercase())
                    .cmp(&a.0.chars().any(|c| c.is_uppercase())),
            )
            .then(a.0.cmp(&b.0))
    });

    let mut chosen: Vec<(String, String, String)> = Vec::new(); // (old label, name, full_name)
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut merged = 0usize;

    for (label, _) in &labels {
        let Some(name) = crate::person::normalize(label) else {
            // Nothing usable: leave the label alone rather than inventing an
            // identity, so a human can look at it.
            continue;
        };
        let full = crate::person::display_name(label).unwrap_or_else(|| label.clone());
        if seen.contains_key(&name) {
            merged += 1;
        } else {
            seen.insert(name.clone(), full.clone());
        }
        chosen.push((label.clone(), name, full));
    }

    let tx = conn.unchecked_transaction()?;
    for (name, full) in &seen {
        tx.execute(
            "INSERT INTO people (name, full_name) VALUES (?1, ?2) \
             ON CONFLICT(name) DO NOTHING",
            rusqlite::params![name, full],
        )?;
    }
    for (old, name, _) in &chosen {
        if old != name {
            tx.execute(
                "UPDATE faces SET person_label = ?1 WHERE person_label = ?2",
                rusqlite::params![name, old],
            )?;
        }
    }
    tx.commit()?;

    Ok((seen.len(), merged))
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        create_faces_table(&c).unwrap();
        c
    }

    fn label(c: &Connection, id: i64, label: &str) {
        c.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (?1, ?2, '0,0,9,9', X'0000', ?3, 1)",
            rusqlite::params![id, format!("h{id}"), label],
        )
        .unwrap();
    }

    #[test]
    fn labels_become_identities_and_display_names() {
        let c = db();
        label(&c, 1, "Işıl Özyeğin");
        label(&c, 2, "Erhan");
        let (people, merged) = migrate_person_labels(&c).unwrap();
        assert_eq!((people, merged), (2, 0));

        let rows: Vec<(String, String)> = c
            .prepare("SELECT name, full_name FROM people ORDER BY name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("erhan".to_string(), "Erhan".to_string()),
                ("isil_ozyegin".to_string(), "Işıl Özyeğin".to_string()),
            ]
        );

        let labels: Vec<String> = c
            .prepare("SELECT person_label FROM faces ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(labels, vec!["isil_ozyegin", "erhan"]);
    }

    #[test]
    fn case_variants_merge_keeping_the_more_used_spelling() {
        // The bug being fixed: these were two people. Afterwards they are one,
        // displayed with the spelling that appeared on more faces.
        let c = db();
        label(&c, 1, "alice");
        label(&c, 2, "Alice");
        label(&c, 3, "Alice");
        let (people, merged) = migrate_person_labels(&c).unwrap();
        assert_eq!((people, merged), (1, 1));

        let full: String = c
            .query_row("SELECT full_name FROM people", [], |r| r.get(0))
            .unwrap();
        assert_eq!(full, "Alice", "the spelling on more faces wins");
        let distinct: i64 = c
            .query_row("SELECT COUNT(DISTINCT person_label) FROM faces", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(distinct, 1, "one identity now");
    }

    #[test]
    fn running_it_twice_changes_nothing() {
        let c = db();
        label(&c, 1, "Erhan");
        let first = migrate_person_labels(&c).unwrap();
        let second = migrate_person_labels(&c).unwrap();
        assert_eq!(first, (1, 0));
        assert_eq!(second.0, 1, "second run is a no-op, not a re-migration");
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM people", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn an_empty_library_is_not_an_error() {
        let c = db();
        assert_eq!(migrate_person_labels(&c).unwrap(), (0, 0));
    }

    #[test]
    fn a_label_with_no_usable_identity_is_left_alone() {
        // "!!!" normalizes to nothing. Inventing an identity would be worse
        // than leaving it for a human to look at.
        let c = db();
        label(&c, 1, "!!!");
        label(&c, 2, "Erhan");
        let (people, _) = migrate_person_labels(&c).unwrap();
        assert_eq!(people, 1);
        let kept: String = c
            .query_row("SELECT person_label FROM faces WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kept, "!!!", "untouched rather than erased");
    }
}
