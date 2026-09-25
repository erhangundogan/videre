use half::f16;
use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Clone)]
pub struct FaceRow {
    pub hash: String,
    pub bbox: String,
    pub landmark: Option<String>,
    pub embedding: Vec<u8>, // 512 f16 values as little-endian bytes (1024 bytes)
    pub cluster_id: Option<i64>,
    pub person_label: Option<String>,
    pub confirmed: i64,
    pub is_primary: i64,
    /// SCRFD's confidence for this detection, and how sharp the aligned crop is
    /// (variance of the Laplacian).
    ///
    /// :warning: **Both were computed and thrown away.** The detector has always
    /// produced a score and the crop has always been available; nothing read
    /// either, so the pipeline could not tell a confident, sharp face from a
    /// doubtful, blurry one, and embedded both. Stored so quality can be judged
    /// at detection time rather than guessed at cluster time.
    pub det_score: f32,
    pub blur: f32,
}

/// Creates the `people` table if it is missing.
///
/// Called from `db::open_wal`, so it runs on **every** open rather than only
/// when faces are written: readers query this table - the labeling UI lists
/// people, and `--person` resolves through it.
pub fn ensure_people_table(conn: &Connection) {
    // One row per person: `name` is the identity form (see
    // `videre_core::person::normalize`) and `full_name` is what a reader sees.
    // `faces.person_label` holds the identity form and refers here.
    //
    // `name` is the primary key deliberately. It puts "two people cannot share
    // an identity" in the database rather than in whichever code path remembers
    // to check.
    //
    // The version-2 faces table references this primary key, and videre
    // enables foreign-key enforcement on every connection it owns.
    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS people (
            name       TEXT PRIMARY KEY,
            full_name  TEXT NOT NULL
        );",
    );
}

/// The canonical `faces` DDL for a trusted static table name. It declares the
/// person relationship so SQLite rejects an unknown `person_label` instead of
/// storing an orphan.
pub fn faces_ddl_for(table: &str, if_not_exists: bool) -> String {
    let exists = if if_not_exists { "IF NOT EXISTS " } else { "" };
    format!(
        "CREATE TABLE {exists}{table} (
            id            INTEGER PRIMARY KEY,
            hash          TEXT NOT NULL,
            bbox          TEXT NOT NULL,
            landmark      TEXT,
            embedding     BLOB NOT NULL,
            cluster_id    INTEGER,
            person_label  TEXT,
            confirmed     INTEGER DEFAULT 0,
            is_primary    INTEGER DEFAULT 0,
            det_score     REAL,
            blur          REAL,
            oriented      INTEGER,
            FOREIGN KEY (person_label) REFERENCES people(name)
                ON DELETE RESTRICT ON UPDATE RESTRICT
        );"
    )
}

/// Create or migrate the faces and people tables without touching any data:
/// no person-label migration, no cluster detach, no row repairs. Safe inside
/// an outer transaction, which the versioned schema upgrade requires; the
/// data migrations belong to the callers that own data.
pub fn ensure_faces_schema(conn: &Connection) -> rusqlite::Result<()> {
    ensure_people_table(conn);
    conn.execute_batch(&faces_ddl_for("faces", true))?;
    Ok(())
}

pub fn create_faces_table(conn: &Connection) -> rusqlite::Result<()> {
    ensure_faces_schema(conn)?;
    crate::face_learning::ensure_profile_table(conn)?;
    crate::face_learning::ensure_learning_tables(conn)?;
    // Writers migrate; readers do not. `open_wal` only creates the empty table,
    // because `stats` and `search` open databases they must not write to - a
    // read-only mount or another process holding the writer lock would turn a
    // report into a failure. This runs from the commands that already write
    // faces, and is a single COUNT once the migration has happened.
    let _ = migrate_person_labels(conn);
    // Frozen faces: a labeled face's identity is its person label, never a
    // machine cluster id. Libraries written before the detach-on-assignment
    // rule carry stale cluster ids on confirmed faces; recluster renumbers
    // unlabeled clusters from 0 and collides with them, mixing named people
    // into unassigned cluster pages. Clear the tombstones on every writer
    // pass; steady state touches zero rows.
    let _ = conn.execute(
        "UPDATE faces SET cluster_id = NULL
         WHERE confirmed = 1 AND person_label IS NOT NULL AND cluster_id IS NOT NULL",
        [],
    );
    create_faces_scanned_table(conn)
}

/// Records every hash whose faces have been scanned, INCLUDING images where
/// zero faces were detected (which leave no `faces` row). This is what makes
/// `videre faces` resumable: the skip set is "already scanned", not merely
/// "has a face", so a no-face image is never re-detected on a later run.
pub fn create_faces_scanned_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS faces_scanned (
            hash        TEXT PRIMARY KEY,
            scanned_at  TEXT DEFAULT (datetime('now'))
        );",
    )
}

/// The highest `faces.id` covered by the last completed global recluster,
/// or 0 when none has run yet. The gate is "any face id above this", so an
/// absent watermark means the next reconcile reclusters everything.
pub fn recluster_watermark(conn: &Connection) -> anyhow::Result<i64> {
    Ok(
        crate::library_state::get(conn, crate::library_state::FACE_RECLUSTER_WATERMARK)?
            .unwrap_or(0),
    )
}

/// Record that a global recluster just completed over every face now in the
/// table. Called after each completed clustering pass, by `videre faces`
/// and by watch's recluster stage alike.
pub fn advance_recluster_watermark(conn: &Connection) -> anyhow::Result<()> {
    let max_id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM faces", [], |r| r.get(0))?;
    crate::library_state::set(conn, crate::library_state::FACE_RECLUSTER_WATERMARK, max_id)
}

/// Destroy all face state: face rows, people records, scanned markers,
/// faces decode-failure entries, and the recluster watermark. The
/// `videre faces --reset` escape hatch: after this, the next faces run
/// starts from absolute beginning. The caller owns consent.
/// What a face reset will delete, for the dry-run preview and the
/// confirmation prompt. Learning state is named so consent covers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FaceResetCounts {
    pub total_faces: usize,
    pub labeled_faces: usize,
    pub people: usize,
    pub learning_events: usize,
    pub questions: usize,
    pub profiles: usize,
}

/// Counts everything `reset_all` clears. Missing optional tables count as
/// zero, but a query error in an existing table must stop the reset.
pub fn face_reset_counts(conn: &Connection) -> anyhow::Result<FaceResetCounts> {
    let count = |table: &str, sql: &str| -> anyhow::Result<usize> {
        if !crate::db::table_exists(conn, table)? {
            return Ok(0);
        }
        let value: i64 = conn.query_row(sql, [], |row| row.get(0))?;
        Ok(value as usize)
    };
    Ok(FaceResetCounts {
        total_faces: count("faces", "SELECT COUNT(*) FROM faces")?,
        labeled_faces: count(
            "faces",
            "SELECT COUNT(*) FROM faces WHERE confirmed = 1 AND person_label IS NOT NULL",
        )?,
        people: count("people", "SELECT COUNT(*) FROM people")?,
        learning_events: count(
            "face_learning_events",
            "SELECT COUNT(*) FROM face_learning_events",
        )?,
        questions: count(
            "face_learning_questions",
            "SELECT COUNT(*) FROM face_learning_questions",
        )?,
        profiles: count(
            "face_learning_profiles",
            "SELECT COUNT(*) FROM face_learning_profiles",
        )?,
    })
}

pub fn reset_all(conn: &Connection) -> anyhow::Result<()> {
    // Ensures every table the deletes name exists, even on a library that
    // never ran faces before.
    create_faces_table(conn)?;
    crate::decode_failures::ensure_table(conn)?;
    crate::face_learning::ensure_learning_tables(conn)?;
    crate::face_learning::ensure_profile_table(conn)?;
    crate::face_learning::ensure_question_tables(conn)?;
    conn.execute_batch("BEGIN")?;
    let result = (|| -> anyhow::Result<()> {
        conn.execute("DELETE FROM faces", [])?;
        conn.execute("DELETE FROM people", [])?;
        conn.execute("DELETE FROM faces_scanned", [])?;
        // The learning tables carry immutability triggers, so a reset drops
        // and recreates them instead of deleting rows; the result is the same
        // clean slate inside the same transaction.
        conn.execute_batch(
            "DROP TABLE face_learning_event_faces;
             DROP TABLE face_learning_events;
             DROP TABLE face_learning_question_faces;
             DROP TABLE face_learning_questions;
             DROP TABLE face_learning_profiles;",
        )?;
        crate::face_learning::ensure_learning_tables(conn)?;
        crate::face_learning::ensure_profile_table(conn)?;
        crate::face_learning::ensure_question_tables(conn)?;
        // The state table itself survives the drop; restore its single row to
        // the absolute beginning.
        conn.execute(
            "UPDATE face_learning_state SET generation = 0, trained_generation = 0,
             status = 'current', training_generation = NULL, last_profile_id = NULL,
             last_error = NULL WHERE id = 1",
            [],
        )?;
        crate::decode_failures::clear_stage(conn, crate::decode_failures::STAGE_FACES)?;
        crate::library_state::set(conn, crate::library_state::FACE_RECLUSTER_WATERMARK, 0)?;
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

/// (labeled faces, people) for the reset confirmation prompt.
pub fn labeled_state_counts(conn: &Connection) -> anyhow::Result<(i64, i64)> {
    let labeled: i64 = conn.query_row(
        "SELECT COUNT(*) FROM faces WHERE confirmed = 1 AND person_label IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let people: i64 = conn.query_row("SELECT COUNT(*) FROM people", [], |r| r.get(0))?;
    Ok((labeled, people))
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
                "INSERT INTO faces (hash, bbox, landmark, embedding, cluster_id, person_label, confirmed, is_primary, det_score, blur, oriented)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)",
                rusqlite::params![
                    face.hash, face.bbox, face.landmark, face.embedding,
                    face.cluster_id, face.person_label, face.confirmed, face.is_primary,
                    face.det_score, face.blur
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

/// Load the complete, generic inputs used by face-learning feature extraction.
/// Requested ids are deduplicated and rows are returned in ascending id order.
/// Missing ids and malformed embedding blobs fail the whole load so a user
/// action can roll back rather than silently lose its learning evidence.
pub fn load_face_observations(
    conn: &Connection,
    face_ids: &[i64],
) -> rusqlite::Result<Vec<crate::face_learning::FaceObservation>> {
    use rusqlite::OptionalExtension;
    use std::collections::BTreeSet;

    let ids: BTreeSet<_> = face_ids.iter().copied().collect();
    let mut statement = conn.prepare(
        "SELECT embedding, bbox, landmark, blur, det_score, hash FROM faces WHERE id = ?1",
    )?;
    let mut observations = Vec::with_capacity(ids.len());
    for id in ids {
        let row = statement
            .query_row([id], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, Option<f64>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .optional()?
            .ok_or_else(|| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(format!(
                    "requested face {id} was not found"
                ))))
            })?;
        let (blob, bbox, landmark, blur, det_score, photo_hash) = row;
        if blob.len() % 2 != 0 {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                blob.len(),
                rusqlite::types::Type::Blob,
                Box::new(std::io::Error::other(format!(
                    "face {id} has a malformed embedding blob"
                ))),
            ));
        }
        let embedding = blob
            .chunks_exact(2)
            .map(|bytes| f16::from_le_bytes([bytes[0], bytes[1]]).to_f32())
            .collect();
        let landmark_residual = landmark
            .as_deref()
            .and_then(crate::face_learning::parse_landmarks)
            .map(|points| crate::face_learning::landmark_residual(&points) as f64);
        observations.push(crate::face_learning::FaceObservation {
            face_id: id,
            embedding,
            bbox_min_side: bbox_min_side_option(&bbox).map(f64::from),
            blur,
            det_score,
            landmark_residual,
            photo_hash,
        });
    }
    Ok(observations)
}

/// Loads confirmed person labels as evaluation truth without migrating or
/// modifying face state.
pub fn load_confirmed_face_labels(
    conn: &Connection,
) -> rusqlite::Result<Vec<crate::face_learning::LabeledFace>> {
    let mut stmt = conn.prepare(
        "SELECT id, person_label FROM faces
         WHERE confirmed = 1 AND person_label IS NOT NULL
         ORDER BY id",
    )?;
    let labels = stmt
        .query_map([], |row| {
            Ok(crate::face_learning::LabeledFace::new(
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
            ))
        })?
        .collect();
    labels
}

/// Like [`load_face_embeddings`] but also returns each face's smaller bbox
/// side in pixels (the shorter of width/height), parsed from the `"x,y,w,h"`
/// bbox string. Used as a quality signal: very small face crops embed into
/// near-degenerate ArcFace vectors that cluster together regardless of
/// identity, so callers gate them out of clustering. A bbox that fails to
/// parse yields a min-side of 0.0 (treated as lowest quality).
pub fn load_faces_for_clustering(
    conn: &Connection,
) -> rusqlite::Result<Vec<(i64, Vec<f32>, f32, Option<String>, Option<f32>)>> {
    // The landmark string travels as-is. What "a good landmark set" means is a
    // property of the ArcFace template, which lives in `videre-ml`; this crate
    // is below it in the dependency graph and must not learn it.
    let mut stmt = conn.prepare("SELECT id, embedding, bbox, landmark, blur FROM faces")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        let bbox: String = row.get(2)?;
        let landmark: Option<String> = row.get(3)?;
        let blur: Option<f32> = row.get(4)?;
        Ok((id, blob, bbox, landmark, blur))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, blob, bbox, landmark, blur) = row?;
        let emb: Vec<f32> = blob
            .chunks_exact(2)
            .map(|b| f16::from_le_bytes([b[0], b[1]]).to_f32())
            .collect();
        out.push((id, emb, bbox_min_side(&bbox), landmark, blur));
    }
    Ok(out)
}

/// Smaller side (min of width, height) of a `"x,y,w,h"` bbox string, or 0.0 if
/// it does not parse into at least four numeric fields.
fn bbox_min_side(bbox: &str) -> f32 {
    bbox_min_side_option(bbox).unwrap_or(0.0)
}

fn bbox_min_side_option(bbox: &str) -> Option<f32> {
    let nums: Vec<f32> = bbox
        .split(',')
        .map(|s| s.trim().parse())
        .collect::<Result<_, _>>()
        .ok()?;
    if nums.len() >= 4 && nums[2].is_finite() && nums[3].is_finite() {
        Some(nums[2].min(nums[3]))
    } else {
        None
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
/// list of (face_id, person_label, bbox) for that hash. One
/// batched query covering every hash, not one query per file, safe to call
/// once per report generation without N+1 overhead.
pub fn labeled_faces_by_hash(conn: &Connection) -> rusqlite::Result<LabeledFacesByHash> {
    let mut stmt = conn.prepare(
        // The display name, not the identity: this feeds the face overlays in
        // the gallery, which a person reads. LEFT JOIN so a label written
        // before the people table existed still renders, as itself.
        "SELECT f.hash, f.id, f.bbox, COALESCE(p.full_name, f.person_label) \
         FROM faces f LEFT JOIN people p ON p.name = f.person_label \
         WHERE f.confirmed = 1 AND f.person_label IS NOT NULL \
         ORDER BY f.hash, f.id",
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
        // Enforced, so the learning tables' declared parent keys are checked
        // by every reset and lifecycle test.
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        create_faces_table(&conn).unwrap();
        conn
    }

    /// Seeds a labeled face without a people row: the pre-v2 legacy shape.
    /// Enforcement is lifted for the seed and restored, so the test body runs
    /// enforced.
    fn seed_legacy_label(conn: &Connection, hash: &str, label: &str, confirmed: i64) {
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding, cluster_id, confirmed, person_label)
             VALUES (?1, '0,0,50,50', X'0000', 0, ?2, ?3)",
            rusqlite::params![hash, confirmed, label],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    }

    /// The people parent a v2 labeled face requires.
    fn seed_person(conn: &Connection, name: &str) {
        conn.execute(
            "INSERT INTO people (name, full_name) VALUES (?1, ?1)",
            rusqlite::params![name],
        )
        .unwrap();
    }

    #[test]
    fn create_table_idempotent() {
        let conn = open();
        create_faces_table(&conn).unwrap();
    }

    #[test]
    fn face_writer_setup_creates_learning_events_and_state() {
        let conn = Connection::open_in_memory().unwrap();
        create_faces_table(&conn).unwrap();
        for table in [
            "face_learning_events",
            "face_learning_event_faces",
            "face_learning_state",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master
                     WHERE type = 'table' AND name = ?1)",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "{table} was not created");
        }
    }

    #[test]
    fn the_detach_migration_clears_tombstones_and_keeps_labels() {
        let conn = open();
        // Legacy shape: a labeled face still carrying a machine cluster id.
        seed_legacy_label(&conn, "h1", "elena", 1);
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding, cluster_id)
             VALUES ('h2', '0,0,50,50', X'0000', 0)",
            [],
        )
        .unwrap();

        create_faces_table(&conn).unwrap();

        let (cid, label): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT cluster_id, person_label FROM faces WHERE hash = 'h1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            cid, None,
            "a labeled face must not carry a machine cluster id"
        );
        assert_eq!(label.as_deref(), Some("elena"), "the label is untouched");
        let cid: Option<i64> = conn
            .query_row("SELECT cluster_id FROM faces WHERE hash = 'h2'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(Some(0), cid, "unlabeled faces keep their machine grouping");
    }

    #[test]
    fn the_detach_migration_is_idempotent() {
        let conn = open();
        seed_legacy_label(&conn, "h1", "elena", 1);
        conn.execute("UPDATE faces SET cluster_id = 7 WHERE hash = 'h1'", [])
            .unwrap();
        create_faces_table(&conn).unwrap();
        create_faces_table(&conn).unwrap();
        let cid: Option<i64> = conn
            .query_row("SELECT cluster_id FROM faces WHERE hash = 'h1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cid, None);
    }

    #[test]
    fn the_watermark_starts_absent_and_advance_records_the_highest_face_id() {
        let conn = open();
        assert_eq!(recluster_watermark(&conn).unwrap(), 0);
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding) VALUES ('h1', '0,0,10,10', X'0000')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding) VALUES ('h2', '0,0,10,10', X'0000')",
            [],
        )
        .unwrap();
        advance_recluster_watermark(&conn).unwrap();
        let max_id: i64 = conn
            .query_row("SELECT MAX(id) FROM faces", [], |r| r.get(0))
            .unwrap();
        assert_eq!(recluster_watermark(&conn).unwrap(), max_id);
    }

    #[test]
    fn advancing_over_an_empty_faces_table_records_zero() {
        let conn = open();
        advance_recluster_watermark(&conn).unwrap();
        assert_eq!(recluster_watermark(&conn).unwrap(), 0);
    }

    #[test]
    fn reset_all_returns_face_state_to_absolute_beginning() {
        let conn = open();
        seed_person(&conn, "elena");
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding, cluster_id, confirmed, person_label)
             VALUES ('h1', '0,0,50,50', X'0000', 0, 1, 'elena')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding, cluster_id)
             VALUES ('h2', '0,0,50,50', X'0000', 1)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO faces_scanned (hash) VALUES ('h1')", [])
            .unwrap();
        crate::library_state::set(&conn, crate::library_state::FACE_RECLUSTER_WATERMARK, 5000)
            .unwrap();

        reset_all(&conn).unwrap();

        for table in ["faces", "people", "faces_scanned"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} must be empty after reset");
        }
        let wm = recluster_watermark(&conn).unwrap();
        assert_eq!(
            wm, 0,
            "the recluster watermark must clear, or the gated recluster would never fire again"
        );
    }

    #[test]
    fn reset_all_removes_face_learning_profiles() {
        let conn = open();
        conn.execute(
            "INSERT INTO face_learning_profiles (
                artifact_version, embedding_model_id, feature_schema_version,
                model_kind, parameters, training_evidence_json,
                validation_report_json, stage, status
             ) VALUES (1, 'test', 1, 'test', X'00', '{}', '{}', 'shadow', 'candidate')",
            [],
        )
        .unwrap();

        reset_all(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM face_learning_profiles", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn reset_counts_and_clears_every_learning_table() {
        let conn = open();
        conn.execute_batch(
            "INSERT INTO people VALUES ('elena','Elena');
             INSERT INTO faces (hash, bbox, embedding, cluster_id, confirmed, person_label)
             VALUES ('h1', '0,0,50,50', X'3C00', NULL, 1, 'elena');
             INSERT INTO faces_scanned (hash) VALUES ('h1');",
        )
        .unwrap();
        crate::face_learning::ensure_learning_tables(&conn).unwrap();
        crate::face_learning::ensure_profile_table(&conn).unwrap();
        crate::face_learning::ensure_question_tables(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO face_learning_events (action_kind, decision_kind, outcome,
                embedding_model_id, feature_schema_version, feature_snapshot_json, support_count)
             VALUES ('assign_face','membership','positive','m/1',1,'{}',0);
             INSERT INTO face_learning_event_faces (event_id, face_id, role, ordinal)
             VALUES (1, 1, 'subject', 0);
             INSERT INTO face_learning_profiles (
                artifact_version, embedding_model_id, feature_schema_version, model_kind,
                parameters, training_evidence_json, validation_report_json, stage, status)
             VALUES (1,'m/1',1,'logistic',X'7B7D','{}','{}','suggestion','active');
             INSERT INTO face_learning_questions (
                status, target_identity, profile_id, model_kind, representative_face_id,
                cluster_id, evidence_revision, evidence_json)
             VALUES ('pending','elena',1,'logistic',1,1,'rev','{}');",
        )
        .unwrap();
        conn.execute(
            "UPDATE face_learning_state SET generation = 3, trained_generation = 2,
             last_profile_id = 1",
            [],
        )
        .unwrap();

        let counts = face_reset_counts(&conn).unwrap();
        assert_eq!(counts.total_faces, 1);
        assert_eq!(counts.labeled_faces, 1);
        assert_eq!(counts.people, 1);
        assert_eq!(counts.learning_events, 1);
        assert_eq!(counts.questions, 1);
        assert_eq!(counts.profiles, 1);

        reset_all(&conn).unwrap();
        assert_eq!(
            face_reset_counts(&conn).unwrap(),
            FaceResetCounts::default()
        );
        let state = crate::face_learning::learning_state(&conn).unwrap();
        assert_eq!(state.generation, 0);
        assert_eq!(state.trained_generation, 0);
        assert_eq!(state.status, crate::face_learning::LearningStatus::Current);
        assert_eq!(state.last_profile_id, None);

        // Second reset on empty learning state is a no-op, not an error.
        reset_all(&conn).unwrap();
        assert_eq!(
            face_reset_counts(&conn).unwrap(),
            FaceResetCounts::default()
        );
    }

    #[test]
    fn reset_counts_propagates_a_query_error_instead_of_reporting_zero() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            face_reset_counts(&conn).unwrap(),
            FaceResetCounts::default()
        );
        conn.execute_batch("CREATE TABLE faces (id INTEGER PRIMARY KEY)")
            .unwrap();
        assert!(face_reset_counts(&conn).is_err());
    }

    #[test]
    fn reset_is_all_or_nothing_under_an_aborting_trigger() {
        let conn = open();
        seed_person(&conn, "elena");
        conn.execute_batch(
            "INSERT INTO faces (hash, bbox, embedding, cluster_id, confirmed, person_label)
             VALUES ('h1', '0,0,50,50', X'3C00', NULL, 1, 'elena');",
        )
        .unwrap();
        crate::face_learning::ensure_learning_tables(&conn).unwrap();
        crate::face_learning::ensure_profile_table(&conn).unwrap();
        crate::face_learning::ensure_question_tables(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO face_learning_events (action_kind, decision_kind, outcome,
                embedding_model_id, feature_schema_version, feature_snapshot_json, support_count)
             VALUES ('assign_face','membership','positive','m/1',1,'{}',0);
             CREATE TRIGGER abort_face_wipe BEFORE DELETE ON faces
             BEGIN SELECT RAISE(ABORT, 'injected wipe failure'); END;",
        )
        .unwrap();
        assert!(reset_all(&conn).is_err());
        let counts = face_reset_counts(&conn).unwrap();
        assert_eq!(counts.total_faces, 1, "faces survive the failed reset");
        assert_eq!(counts.learning_events, 1, "evidence survives too");
    }

    #[test]
    fn labeled_state_counts_reports_what_the_reset_prompt_names() {
        let conn = open();
        assert_eq!(labeled_state_counts(&conn).unwrap(), (0, 0));
        // h2 carries a label with no people row: the pre-v2 legacy shape the
        // reset prompt must still count honestly.
        seed_legacy_label(&conn, "h2", "erhan", 1);
        seed_person(&conn, "elena");
        conn.execute(
            "INSERT INTO faces (hash, bbox, embedding, confirmed, person_label)
             VALUES ('h1', '0,0,50,50', X'0000', 1, 'elena')",
            [],
        )
        .unwrap();
        assert_eq!(labeled_state_counts(&conn).unwrap(), (2, 1));
    }

    #[test]
    fn replace_marks_rows_as_display_canvas() {
        // Every row is detected on the display canvas; `oriented` records
        // that for anything reading the database directly.
        let conn = open();
        let row = FaceRow {
            hash: "habc".into(),
            bbox: "0,0,50,50".into(),
            landmark: None,
            embedding: make_embedding(&vec![0.5f32; 512]),
            cluster_id: None,
            person_label: None,
            confirmed: 0,
            is_primary: 0,
            det_score: 0.9,
            blur: 1000.0,
        };
        replace_faces_for_hash(&conn, "habc", &[row]).unwrap();
        let oriented: i64 = conn
            .query_row("SELECT oriented FROM faces WHERE hash = 'habc'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(oriented, 1);
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
                det_score: 0.9,
                blur: 1000.0,
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
                    det_score: 0.9,
                    blur: 1000.0,
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
                    det_score: 0.9,
                    blur: 1000.0,
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
                det_score: 0.9,
                blur: 1000.0,
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
                det_score: 0.9,
                blur: 1000.0,
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
                    det_score: 0.9,
                    blur: 1000.0,
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
                    det_score: 0.9,
                    blur: 1000.0,
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
    fn load_face_observations_is_ordered_complete_and_preserves_missing_quality() {
        let conn = open();
        let emb = make_embedding(&[1.0, 0.0]);
        conn.execute(
            "INSERT INTO faces
             (id, hash, bbox, landmark, embedding, det_score, blur)
             VALUES (2, 'h2', '0,0,40,25', NULL, ?1, NULL, NULL),
                    (1, 'h1', '0,0,20,30',
                     '38.2946,51.6963,73.5318,51.5014,56.0252,71.7366,41.5493,92.3655,70.7299,92.2041',
                     ?1, 0.9, 200.0)",
            [emb],
        )
        .unwrap();

        let rows = load_face_observations(&conn, &[2, 1]).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.face_id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(rows[0].bbox_min_side, Some(20.0));
        assert!(rows[0].landmark_residual.unwrap() < 1e-3);
        assert_eq!(rows[1].bbox_min_side, Some(25.0));
        assert_eq!(rows[1].blur, None);
        assert_eq!(rows[1].det_score, None);
        assert_eq!(rows[1].landmark_residual, None);

        let error = load_face_observations(&conn, &[1, 3]).unwrap_err();
        assert!(error.to_string().contains("requested face 3 was not found"));
    }

    #[test]
    fn load_confirmed_face_labels_returns_only_ordered_truth_without_mutation() {
        let conn = open();
        seed_person(&conn, "zoe");
        seed_person(&conn, "İpek");
        // Face 4 is unconfirmed but still carries the stored label string
        // "unconfirmed", which the enforced key requires a parent for.
        seed_person(&conn, "unconfirmed");
        for (id, confirmed, label, cluster_id) in [
            (4, 0, Some("unconfirmed"), Some(40)),
            (3, 1, None, Some(30)),
            (2, 1, Some("zoe"), Some(20)),
            (1, 1, Some("İpek"), Some(10)),
            (5, 0, None, None),
        ] {
            conn.execute(
                "INSERT INTO faces
                 (id, hash, bbox, embedding, cluster_id, person_label, confirmed)
                 VALUES (?1, ?2, '0,0,10,10', X'0000', ?3, ?4, ?5)",
                rusqlite::params![id, format!("h{id}"), cluster_id, label, confirmed],
            )
            .unwrap();
        }
        let before: Vec<(i64, Option<i64>, Option<String>, i64)> = conn
            .prepare("SELECT id, cluster_id, person_label, confirmed FROM faces ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        let labels = load_confirmed_face_labels(&conn).unwrap();

        assert_eq!(
            labels,
            vec![
                crate::face_learning::LabeledFace::new(1, "İpek"),
                crate::face_learning::LabeledFace::new(2, "zoe"),
            ]
        );
        let after: Vec<(i64, Option<i64>, Option<String>, i64)> = conn
            .prepare("SELECT id, cluster_id, person_label, confirmed FROM faces ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(after, before);
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
                det_score: 0.9,
                blur: 1000.0,
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
            "INSERT INTO people VALUES ('alice','Alice'), ('bob','Bob');
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
             VALUES ('h1', '0,0,10,10', X'0000', 'alice', 1); \
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
             VALUES ('h1', '20,20,10,10', X'0000', NULL, 0); \
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) \
             VALUES ('h2', '0,0,10,10', X'0000', 'bob', 1);",
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

    // The fixture recreates the LEGACY library shape: labeled faces whose
    // person rows do not exist yet (the migration is what creates them).
    // Enforcement is lifted only for that seed and turned back on before the
    // migration runs, so the migration itself is exercised under enforcement.
    fn label(c: &Connection, id: i64, label: &str) {
        c.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        c.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (?1, ?2, '0,0,9,9', X'0000', ?3, 1)",
            rusqlite::params![id, format!("h{id}"), label],
        )
        .unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
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

#[cfg(test)]
mod people_table_tests {
    use super::*;

    fn label_legacy(c: &Connection, id: i64, label: &str) {
        c.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        c.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (?1, ?2, '0,0,9,9', X'0000', ?3, 1)",
            rusqlite::params![id, format!("h{id}"), label],
        )
        .unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    }

    #[test]
    fn ensure_people_table_is_idempotent_and_keeps_rows() {
        // It runs on every open, so a second call must not disturb what is
        // there.
        let c = Connection::open_in_memory().unwrap();
        ensure_people_table(&c);
        c.execute(
            "INSERT INTO people (name, full_name) VALUES ('erhan','Erhan')",
            [],
        )
        .unwrap();
        ensure_people_table(&c);
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM people", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "an existing row survives a second ensure");
    }

    #[test]
    fn two_people_cannot_share_an_identity() {
        // The primary key is what makes this a guarantee rather than something
        // each write path has to remember to check.
        let c = Connection::open_in_memory().unwrap();
        ensure_people_table(&c);
        c.execute(
            "INSERT INTO people (name, full_name) VALUES ('erhan','Erhan')",
            [],
        )
        .unwrap();
        let second = c.execute(
            "INSERT INTO people (name, full_name) VALUES ('erhan','Erhan Gündoğan')",
            [],
        );
        assert!(second.is_err(), "the database refuses, not the caller");
    }

    #[test]
    fn a_tie_on_face_count_prefers_the_capitalised_spelling() {
        // Two spellings, one face each: `Alice` is likelier to be the name
        // someone typed deliberately than `alice`.
        let c = Connection::open_in_memory().unwrap();
        create_faces_table(&c).unwrap();
        for (id, label) in [(1, "alice"), (2, "Alice")] {
            label_legacy(&c, id, label);
        }
        let (people, merged) = migrate_person_labels(&c).unwrap();
        assert_eq!((people, merged), (1, 1));
        let full: String = c
            .query_row("SELECT full_name FROM people", [], |r| r.get(0))
            .unwrap();
        assert_eq!(full, "Alice");
    }

    #[test]
    fn accented_and_unaccented_spellings_become_one_person() {
        // `Şefik` and `Sefik` fold to the same identity. That is the intent -
        // and the reason the display name is kept separately, so the accented
        // spelling is not lost.
        let c = Connection::open_in_memory().unwrap();
        create_faces_table(&c).unwrap();
        for (id, label) in [(1, "Şefik"), (2, "Şefik"), (3, "Sefik")] {
            label_legacy(&c, id, label);
        }
        let (people, merged) = migrate_person_labels(&c).unwrap();
        assert_eq!((people, merged), (1, 1));
        let (name, full): (String, String) = c
            .query_row("SELECT name, full_name FROM people", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(name, "sefik");
        assert_eq!(full, "Şefik", "the accented spelling had more faces");
    }

    #[test]
    fn every_migrated_face_points_at_a_person_row() {
        // The invariant worth asserting on a real library: no face left holding
        // a label that `people` does not know about.
        let c = Connection::open_in_memory().unwrap();
        create_faces_table(&c).unwrap();
        for (id, label) in [(1, "Işıl Özyeğin"), (2, "Ahmet Arı"), (3, "erhan")] {
            label_legacy(&c, id, label);
        }
        migrate_person_labels(&c).unwrap();
        let orphans: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM faces f LEFT JOIN people p ON p.name = f.person_label \
                 WHERE f.person_label IS NOT NULL AND p.name IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
    }
}

#[cfg(test)]
mod overlay_label_tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        create_faces_table(&c).unwrap();
        // `no_row_yet` pins that overlays read the stored label rather than
        // the join, so it deliberately has no people row: seed that one face
        // with enforcement lifted, the way a pre-v2 library looked.
        c.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        c.execute_batch(
            "INSERT INTO people (name, full_name) VALUES ('ozgur_demirtas','Özgür Demirtaş');
             INSERT INTO faces (id,hash,bbox,embedding,person_label,confirmed) VALUES
               (1,'h1','10,10,60,60',X'0000','ozgur_demirtas',1),
               (2,'h2','10,10,60,60',X'0000','no_row_yet',1),
               (3,'h3','10,10,60,60',X'0000','ozgur_demirtas',0);",
        )
        .unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        c
    }

    #[test]
    fn face_overlays_show_the_display_name() {
        // `report --show-faces` draws these on the photo, so they must read the
        // way a person wrote them - `Özgür Demirtaş`, not `ozgur_demirtas`.
        let m = labeled_faces_by_hash(&db()).unwrap();
        let (_, name, _) = &m.get("h1").unwrap()[0];
        assert_eq!(name, "Özgür Demirtaş");
    }

    #[test]
    fn a_label_with_no_people_row_still_renders_as_itself() {
        // Mid-migration, or written before the table existed: showing nothing
        // would be worse than showing the raw label.
        let m = labeled_faces_by_hash(&db()).unwrap();
        let (_, name, _) = &m.get("h2").unwrap()[0];
        assert_eq!(name, "no_row_yet");
    }

    #[test]
    fn unconfirmed_faces_are_not_labelled_on_photos() {
        // An unreviewed guess must not appear as a caption on someone's photo.
        let m = labeled_faces_by_hash(&db()).unwrap();
        assert!(!m.contains_key("h3"));
    }
}
