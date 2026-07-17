use half::f16;
use rusqlite::Connection;
use std::collections::HashMap;

pub struct FaceRow {
    pub hash: String,
    pub bbox: String,
    pub landmark: Option<String>,
    pub embedding: Vec<u8>,      // 512 f16 values as little-endian bytes (1024 bytes)
    pub cluster_id: Option<i64>,
    pub person_label: Option<String>,
    pub confirmed: i64,
    pub is_primary: i64,
}

pub fn create_faces_table(conn: &Connection) -> rusqlite::Result<()> {
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
        );"
    )?;
    // Migration for existing tables without is_primary column; ignored if already exists.
    let _ = conn.execute_batch("ALTER TABLE faces ADD COLUMN is_primary INTEGER DEFAULT 0");
    Ok(())
}

pub fn replace_faces_for_hash(conn: &Connection, hash: &str, faces: &[FaceRow]) -> rusqlite::Result<()> {
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
        Ok(()) => { conn.execute_batch("COMMIT")?; Ok(()) }
        Err(e) => { let _ = conn.execute_batch("ROLLBACK"); Err(e) }
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

pub fn update_cluster_assignments(conn: &Connection, assignments: &[(i64, Option<i64>)]) -> rusqlite::Result<()> {
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
/// covering every hash, not one query per file - safe to call once per
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
    vals.iter().flat_map(|&v| f16::from_f32(v).to_le_bytes()).collect()
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
        replace_faces_for_hash(&conn, "habc", &[FaceRow {
            hash: "habc".into(), bbox: "0,0,50,50".into(), landmark: None,
            embedding: emb, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0,
        }]).unwrap();
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
        replace_faces_for_hash(&conn, "h1", &[
            FaceRow { hash: "h1".into(), bbox: "0,0,10,10".into(), landmark: None, embedding: emb.clone(), cluster_id: None, person_label: None, confirmed: 0, is_primary: 0 },
            FaceRow { hash: "h1".into(), bbox: "20,0,10,10".into(), landmark: None, embedding: emb.clone(), cluster_id: None, person_label: None, confirmed: 0, is_primary: 0 },
        ]).unwrap();
        replace_faces_for_hash(&conn, "h1", &[
            FaceRow { hash: "h1".into(), bbox: "99,0,10,10".into(), landmark: None, embedding: emb, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0 },
        ]).unwrap();
        let rows = load_face_embeddings(&conn).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn update_cluster_assignments_works() {
        let conn = open();
        let emb = make_embedding(&vec![0.0f32; 512]);
        replace_faces_for_hash(&conn, "h1", &[FaceRow { hash: "h1".into(), bbox: "0,0,10,10".into(), landmark: None, embedding: emb, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0 }]).unwrap();
        let rows = load_face_embeddings(&conn).unwrap();
        let id = rows[0].0;
        update_cluster_assignments(&conn, &[(id, Some(3))]).unwrap();
        let n: i64 = conn.query_row("SELECT cluster_id FROM faces WHERE id=?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn hashes_with_faces_returns_inserted_hash() {
        let conn = open();
        let emb = make_embedding(&vec![0.0f32; 512]);
        replace_faces_for_hash(&conn, "myhash", &[FaceRow { hash: "myhash".into(), bbox: "0,0,10,10".into(), landmark: None, embedding: emb, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0 }]).unwrap();
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
