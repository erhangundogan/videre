//! Facade over videre's faces-labeling read operations. Plain functions over
//! an open `rusqlite::Connection`, returning serde types and a shared
//! `Error`. Called by both the axum `--faces` server and the Tauri desktop
//! app.

use crate::error::{Error, Result};
use crate::types::*;
use rusqlite::Connection;
use std::collections::HashMap;

/// People / unassigned clusters / singletons for the labeling page.
pub fn faces_list(conn: &Connection) -> Result<FacesData> {
    let mut people: HashMap<String, PersonData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash, person_label FROM faces \
             WHERE confirmed = 1 AND person_label IS NOT NULL \
             ORDER BY person_label, is_primary DESC, id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (id, hash, label) = row?;
            let person = people.entry(label.clone()).or_insert(PersonData {
                label: label.clone(),
                face_ids: vec![],
                representative_id: id,
                hashes: vec![],
            });
            person.face_ids.push(id);
            if !person.hashes.contains(&hash) {
                person.hashes.push(hash);
            }
        }
    }

    let mut cluster_map: HashMap<i64, ClusterData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash, cluster_id FROM faces \
             WHERE cluster_id IS NOT NULL AND (confirmed = 0 OR person_label IS NULL) \
             ORDER BY cluster_id, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?;
        for row in rows {
            let (id, hash, cid) = row?;
            let cluster = cluster_map.entry(cid).or_insert(ClusterData {
                cluster_id: cid,
                face_ids: vec![],
                hashes: vec![],
            });
            cluster.face_ids.push(id);
            if !cluster.hashes.contains(&hash) {
                cluster.hashes.push(hash);
            }
        }
    }

    let mut singletons: Vec<SingletonData> = vec![];
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash FROM faces \
             WHERE cluster_id IS NULL AND (confirmed = 0 OR person_label IS NULL) \
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, hash) = row?;
            singletons.push(SingletonData { face_id: id, hash });
        }
    }

    Ok(FacesData {
        people: people.into_values().collect(),
        clusters: cluster_map.into_values().collect(),
        singletons,
    })
}

/// Every face in one unassigned cluster (for the cluster detail page).
pub fn cluster_detail(conn: &Connection, cluster_id: i64) -> Result<ClusterDetail> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.hash, fh.path FROM faces f \
         JOIN file_hashes fh ON f.hash = fh.hash \
         WHERE f.cluster_id = ?1 ORDER BY f.id",
    )?;
    let faces = stmt
        .query_map([cluster_id], |r| {
            Ok(ClusterFaceData { face_id: r.get(0)?, hash: r.get(1)?, path: r.get(2)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ClusterDetail { cluster_id, faces })
}

/// Every confirmed face for one person, primary first and flagged.
pub fn person_detail(conn: &Connection, name: &str) -> Result<PersonDetail> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.hash, fh.path, f.is_primary FROM faces f \
         JOIN file_hashes fh ON f.hash = fh.hash \
         WHERE f.person_label = ?1 AND f.confirmed = 1 \
         ORDER BY f.is_primary DESC, f.id",
    )?;
    let faces = stmt
        .query_map([name], |r| {
            Ok(PersonFaceData {
                face_id: r.get(0)?,
                hash: r.get(1)?,
                path: r.get(2)?,
                is_primary: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(PersonDetail { label: name.to_string(), faces })
}

/// Image paths for confirmed faces of a person (prefix match), for the
/// person-name autocomplete. Delegates to the existing core search.
pub fn search_person(conn: &Connection, name: &str) -> Result<Vec<String>> {
    Ok(videre_core::person_search::search_by_person(conn, name, None)?)
}

/// Assign faces to an existing/new person: sets person_label + confirmed.
/// Rejects an empty label after sanitizing.
pub fn assign(conn: &Connection, face_ids: &[i64], person_label: &str) -> Result<()> {
    let label = crate::label::sanitize_person_label(person_label).ok_or(Error::Invalid)?;
    for id in face_ids {
        conn.execute(
            "UPDATE faces SET person_label = ?1, confirmed = 1 WHERE id = ?2",
            rusqlite::params![label, id],
        )?;
    }
    Ok(())
}

/// Create a person from faces. Same effect as `assign`; kept as a distinct
/// operation because callers treat "new person" and "assign to existing" as
/// separate user intents.
pub fn new_person(conn: &Connection, face_ids: &[i64], label: &str) -> Result<()> {
    assign(conn, face_ids, label)
}

/// Reset one face to fully unassigned (cluster, label, confirmed, primary).
pub fn remove_face(conn: &Connection, face_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE faces SET cluster_id = NULL, person_label = NULL, confirmed = 0, is_primary = 0 WHERE id = ?1",
        [face_id],
    )?;
    Ok(())
}

/// Ungroup a bad cluster: its faces become unassigned singletons (not deleted).
pub fn dissolve_cluster(conn: &Connection, cluster_id: i64) -> Result<()> {
    conn.execute("UPDATE faces SET cluster_id = NULL WHERE cluster_id = ?1", [cluster_id])?;
    Ok(())
}

/// Reset every face of a person back to unassigned. Deliberately does NOT touch
/// cluster_id, so a face rejoins its cluster's unassigned group rather than
/// scattering to singletons.
pub fn delete_person(conn: &Connection, label: &str) -> Result<()> {
    conn.execute(
        "UPDATE faces SET person_label = NULL, confirmed = 0, is_primary = 0 WHERE person_label = ?1",
        rusqlite::params![label],
    )?;
    Ok(())
}

/// Mark one face as the person's primary (their labeling-page thumbnail),
/// clearing any previous primary in the same transaction so exactly one
/// remains. The target update is guarded by person_label so it can't steal a
/// face from another person.
pub fn set_primary(conn: &Connection, face_id: i64, person_label: &str) -> Result<()> {
    conn.execute_batch("BEGIN")?;
    let result = (|| -> rusqlite::Result<()> {
        conn.execute(
            "UPDATE faces SET is_primary = 0 WHERE person_label = ?1",
            rusqlite::params![person_label],
        )?;
        conn.execute(
            "UPDATE faces SET is_primary = 1, confirmed = 1, person_label = ?1 WHERE id = ?2 AND person_label = ?1",
            rusqlite::params![person_label, face_id],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(Error::Db(e))
        }
    }
}

/// Rename a person. `NotFound` if `old_label` has no faces; `Conflict` if
/// `new_label` (after sanitizing) already belongs to a different person;
/// `Invalid` if the new label sanitizes to empty.
pub fn rename_person(conn: &Connection, old_label: &str, new_label: &str) -> Result<()> {
    let sanitized = crate::label::sanitize_person_label(new_label).ok_or(Error::Invalid)?;

    let old_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![old_label],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if old_count == 0 {
        return Err(Error::NotFound);
    }

    let collision_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![sanitized],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if collision_count > 0 && sanitized != old_label {
        return Err(Error::Conflict);
    }

    conn.execute(
        "UPDATE faces SET person_label = ?1 WHERE person_label = ?2",
        rusqlite::params![sanitized, old_label],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory db with the faces + file_hashes tables and a few rows:
    /// - face 1: person "Alice", confirmed, is_primary
    /// - face 2: person "Alice", confirmed
    /// - face 3: cluster 7 (unassigned)
    /// - face 4: cluster 7 (unassigned)
    /// - face 5: singleton (no cluster, unassigned)
    pub(super) fn seed() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);
             INSERT INTO file_hashes VALUES ('h1','/p/1.jpg'),('h2','/p/2.jpg'),
                ('h3','/p/3.jpg'),('h4','/p/4.jpg'),('h5','/p/5.jpg');
             INSERT INTO faces (id,hash,bbox,embedding,cluster_id,person_label,confirmed,is_primary) VALUES
                (1,'h1','0,0,9,9',X'0000',NULL,'Alice',1,1),
                (2,'h2','0,0,9,9',X'0000',NULL,'Alice',1,0),
                (3,'h3','0,0,9,9',X'0000',7,NULL,0,0),
                (4,'h4','0,0,9,9',X'0000',7,NULL,0,0),
                (5,'h5','0,0,9,9',X'0000',NULL,NULL,0,0);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn faces_list_splits_people_clusters_singletons() {
        let conn = seed();
        let d = faces_list(&conn).unwrap();
        assert_eq!(d.people.len(), 1);
        assert_eq!(d.people[0].label, "Alice");
        assert_eq!(d.people[0].representative_id, 1, "primary face is representative");
        assert_eq!(d.clusters.len(), 1);
        assert_eq!(d.clusters[0].cluster_id, 7);
        assert_eq!(d.clusters[0].face_ids, vec![3, 4]);
        assert_eq!(d.singletons.len(), 1);
        assert_eq!(d.singletons[0].face_id, 5);
    }

    #[test]
    fn person_detail_marks_primary() {
        let conn = seed();
        let p = person_detail(&conn, "Alice").unwrap();
        assert_eq!(p.faces.len(), 2);
        assert!(p.faces[0].is_primary, "primary sorts first and is flagged");
        assert!(!p.faces[1].is_primary);
    }

    #[test]
    fn cluster_detail_lists_faces() {
        let conn = seed();
        let c = cluster_detail(&conn, 7).unwrap();
        assert_eq!(c.cluster_id, 7);
        assert_eq!(c.faces.iter().map(|f| f.face_id).collect::<Vec<_>>(), vec![3, 4]);
    }

    #[test]
    fn assign_labels_and_confirms() {
        let conn = seed();
        assign(&conn, &[3, 4], "Bob").unwrap();
        let p = person_detail(&conn, "Bob").unwrap();
        assert_eq!(p.faces.len(), 2, "both faces now confirmed under Bob");
    }

    #[test]
    fn assign_rejects_empty_label() {
        let conn = seed();
        assert!(matches!(assign(&conn, &[3], "   "), Err(Error::Invalid)));
    }

    #[test]
    fn remove_face_unassigns_everything() {
        let conn = seed();
        remove_face(&conn, 1).unwrap();
        let (cid, label, confirmed, prim): (Option<i64>, Option<String>, i64, i64) = conn
            .query_row(
                "SELECT cluster_id, person_label, confirmed, is_primary FROM faces WHERE id=1",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        assert_eq!((cid, label, confirmed, prim), (None, None, 0, 0));
    }

    #[test]
    fn dissolve_cluster_nulls_cluster_id() {
        let conn = seed();
        dissolve_cluster(&conn, 7).unwrap();
        assert_eq!(faces_list(&conn).unwrap().clusters.len(), 0);
        assert_eq!(faces_list(&conn).unwrap().singletons.len(), 3, "3,4 join 5 as singletons");
    }

    #[test]
    fn delete_person_unassigns_without_touching_cluster() {
        let conn = seed();
        // Give one of Alice's faces a cluster_id so we can prove delete_person
        // leaves cluster_id intact (it must, so the face rejoins its cluster's
        // unassigned group rather than scattering to singletons).
        conn.execute("UPDATE faces SET cluster_id = 42 WHERE id = 1", []).unwrap();
        delete_person(&conn, "Alice").unwrap();
        assert_eq!(faces_list(&conn).unwrap().people.len(), 0, "Alice is gone");
        let (cid, label, confirmed): (Option<i64>, Option<String>, i64) = conn
            .query_row(
                "SELECT cluster_id, person_label, confirmed FROM faces WHERE id = 1",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(cid, Some(42), "cluster_id must be preserved");
        assert_eq!(label, None, "person_label cleared");
        assert_eq!(confirmed, 0, "confirmed cleared");
    }

    #[test]
    fn set_primary_is_exclusive_per_person() {
        let conn = seed();
        set_primary(&conn, 2, "Alice").unwrap();
        let primaries: Vec<i64> = {
            let mut s = conn.prepare("SELECT id FROM faces WHERE person_label='Alice' AND is_primary=1").unwrap();
            s.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(primaries, vec![2], "exactly one primary, now face 2");
    }

    #[test]
    fn rename_missing_person_is_not_found() {
        let conn = seed();
        assert!(matches!(rename_person(&conn, "Nobody", "X"), Err(Error::NotFound)));
    }

    #[test]
    fn rename_onto_existing_person_conflicts() {
        let conn = seed();
        assign(&conn, &[3], "Bob").unwrap(); // Bob now exists
        assert!(matches!(rename_person(&conn, "Alice", "Bob"), Err(Error::Conflict)));
    }

    #[test]
    fn rename_succeeds() {
        let conn = seed();
        rename_person(&conn, "Alice", "Alicia").unwrap();
        assert_eq!(person_detail(&conn, "Alicia").unwrap().faces.len(), 2);
        assert_eq!(person_detail(&conn, "Alice").unwrap().faces.len(), 0);
    }
}
