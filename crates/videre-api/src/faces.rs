//! Facade over videre's faces-labeling read operations. Plain functions over
//! an open `rusqlite::Connection`, returning serde types and a shared
//! `Error`. Called by the axum `--faces` server and any other embedder.

use crate::error::{Error, Result};
use crate::types::*;
use rusqlite::Connection;
use std::collections::HashMap;

/// People / unassigned clusters / singletons for the labeling page.
pub fn faces_list(conn: &Connection) -> Result<FacesData> {
    let mut people: HashMap<String, PersonData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            // LEFT JOIN, not JOIN: a face labelled before the people table
            // existed still has to appear, showing its raw label until the
            // migration gives it a row.
            "SELECT f.id, f.hash, f.person_label, COALESCE(p.full_name, f.person_label) \
             FROM faces f LEFT JOIN people p ON p.name = f.person_label \
             WHERE f.confirmed = 1 AND f.person_label IS NOT NULL \
             ORDER BY f.person_label, f.is_primary DESC, f.id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, hash, label, full_name) = row?;
            let person = people.entry(label.clone()).or_insert(PersonData {
                label: label.clone(),
                full_name,
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
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
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

    // Both maps are HashMaps, whose iteration order is arbitrary and differs
    // between instances, so collecting straight from them threw away the
    // ORDER BY the queries above establish. The labeling UI re-fetches this
    // list after every assignment, so the effect was that people and clusters
    // reshuffled on each drop: the cluster lined up next moved somewhere else,
    // and so did the person being dragged onto. `singletons` never had the
    // problem, and the difference is exactly that it is built as a Vec.
    //
    // Clusters are ordered largest first, which is the order people label in:
    // the big clusters are worth the most and are the easiest to recognise.
    // cluster_id breaks ties so the order is total, not merely sorted.
    let mut people: Vec<PersonData> = people.into_values().collect();
    people.sort_by(|a, b| a.full_name.to_lowercase().cmp(&b.full_name.to_lowercase()));
    let mut clusters: Vec<ClusterData> = cluster_map.into_values().collect();
    clusters.sort_by(|a, b| {
        b.face_ids
            .len()
            .cmp(&a.face_ids.len())
            .then(a.cluster_id.cmp(&b.cluster_id))
    });

    Ok(FacesData {
        people,
        clusters,
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
            Ok(ClusterFaceData {
                face_id: r.get(0)?,
                hash: r.get(1)?,
                path: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ClusterDetail { cluster_id, faces })
}

/// Every confirmed face for one person, primary first and flagged.
pub fn person_detail(conn: &Connection, name: &str) -> Result<PersonDetail> {
    // Reads normalize too, so `/person/Erhan`, `/person/erhan` and the original
    // spelling all reach the same person. That is what keeps existing links
    // working across the migration without a redirect table.
    let name = videre_core::person::normalize(name).unwrap_or_else(|| name.to_string());
    let name = name.as_str();
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
    Ok(PersonDetail {
        label: name.to_string(),
        faces,
    })
}

/// Image paths for confirmed faces of a person (prefix match), for the
/// person-name autocomplete. Delegates to the existing core search.
pub fn search_person(conn: &Connection, name: &str) -> Result<Vec<String>> {
    Ok(videre_core::person_search::search_by_person(
        conn, name, None,
    )?)
}

/// Assign faces to an existing/new person: sets person_label + confirmed.
/// Rejects an empty label after sanitizing.
pub fn assign(conn: &Connection, face_ids: &[i64], person_label: &str) -> Result<()> {
    // What was typed becomes the display name; its normalized form is the
    // identity written to every face row. Upserting keeps `people` complete
    // without a separate "create person" step.
    let display = crate::label::sanitize_person_label(person_label).ok_or(Error::Invalid)?;
    let label = videre_core::person::normalize(&display).ok_or(Error::Invalid)?;
    conn.execute(
        "INSERT INTO people (name, full_name) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING",
        rusqlite::params![&label, &display],
    )?;
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
    conn.execute(
        "UPDATE faces SET cluster_id = NULL WHERE cluster_id = ?1",
        [cluster_id],
    )?;
    Ok(())
}

/// Reset every face of a person back to unassigned. Deliberately does NOT touch
/// cluster_id, so a face rejoins its cluster's unassigned group rather than
/// scattering to singletons.
/// Change only what a person is shown as, never their identity.
///
/// A separate operation from `rename_person` on purpose. Intent cannot be
/// inferred from the new string: `Erhan` to `Erhan Gündoğan` is a display
/// correction, yet its normalized form changes too, so a single function would
/// have to guess which the caller meant. One row, no face touched, and the URL
/// keeps working - which is the whole reason identity and display are separate.
pub fn set_full_name(conn: &Connection, name: &str, full_name: &str) -> Result<()> {
    let display = crate::label::sanitize_person_label(full_name).ok_or(Error::Invalid)?;
    let name = videre_core::person::normalize(name).ok_or(Error::Invalid)?;
    let n = conn.execute(
        "UPDATE people SET full_name = ?1 WHERE name = ?2",
        rusqlite::params![display, name],
    )?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

pub fn delete_person(conn: &Connection, label: &str) -> Result<()> {
    let label = videre_core::person::normalize(label).unwrap_or_else(|| label.to_string());
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
    let person_label =
        videre_core::person::normalize(person_label).unwrap_or_else(|| person_label.to_string());
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
    // Both sides normalize, so renaming works whether the caller passes an
    // identity (`erhan`) or what a human sees (`Erhan Gündoğan`).
    let display = crate::label::sanitize_person_label(new_label).ok_or(Error::Invalid)?;
    let new_name = videre_core::person::normalize(&display).ok_or(Error::Invalid)?;
    let old_name = videre_core::person::normalize(old_label).ok_or(Error::Invalid)?;

    let old_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![&old_name],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if old_count == 0 {
        return Err(Error::NotFound);
    }

    // Changing only the spelling - `Erhan` to `Erhan Gündoğan` - leaves the
    // identity alone, so it is a one-row update and can never collide. That is
    // the common rename, and the whole reason display and identity are separate.
    if new_name == old_name {
        conn.execute(
            "INSERT INTO people (name, full_name) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET full_name = excluded.full_name",
            rusqlite::params![&new_name, &display],
        )?;
        return Ok(());
    }

    let collision_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![&new_name],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if collision_count > 0 {
        return Err(Error::Conflict);
    }

    // An identity rename moves both the people row and every face pointing at
    // it. Two statements in one transaction, which is what this did before the
    // people table existed too.
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO people (name, full_name) VALUES (?1, ?2) \
         ON CONFLICT(name) DO UPDATE SET full_name = excluded.full_name",
        rusqlite::params![&new_name, &display],
    )?;
    tx.execute(
        "UPDATE faces SET person_label = ?1 WHERE person_label = ?2",
        rusqlite::params![&new_name, &old_name],
    )?;
    tx.execute(
        "DELETE FROM people WHERE name = ?1",
        rusqlite::params![&old_name],
    )?;
    tx.commit()?;
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
             -- Labels are stored in identity form, as `assign` writes them and
             -- as the migration leaves them; `people` carries what a reader
             -- sees. Seeding raw 'Alice' would test a state the application no
             -- longer produces.
             INSERT INTO people (name, full_name) VALUES ('alice','Alice');
             INSERT INTO faces (id,hash,bbox,embedding,cluster_id,person_label,confirmed,is_primary) VALUES
                (1,'h1','0,0,9,9',X'0000',NULL,'alice',1,1),
                (2,'h2','0,0,9,9',X'0000',NULL,'alice',1,0),
                (3,'h3','0,0,9,9',X'0000',7,NULL,0,0),
                (4,'h4','0,0,9,9',X'0000',7,NULL,0,0),
                (5,'h5','0,0,9,9',X'0000',NULL,NULL,0,0);",
        )
        .unwrap();
        videre_core::db::ensure_file_hashes_columns(&conn);
        conn
    }

    #[test]
    fn the_list_comes_back_in_the_same_order_every_time() {
        // The labeling UI re-fetches after every assignment, so an unstable
        // order means the cluster lined up next moves, and so does the person
        // being dragged onto. Both lists were collected straight out of a
        // HashMap, which discarded the ORDER BY in the queries above.
        // `singletons` never had the bug, and the only difference is that it is
        // built as a Vec.
        let conn = seed();
        // The seed has one cluster and one person, which cannot show an
        // ordering problem. Add enough of both to have an order at all, with
        // sizes deliberately not matching id order.
        conn.execute_batch(
            // Columns named explicitly: `seed` runs ensure_file_hashes_columns,
            // so the table has more than the two it was created with.
            "INSERT INTO file_hashes (hash, path) VALUES ('h6','/p/6.jpg'),('h7','/p/7.jpg'),
                ('h8','/p/8.jpg'),('h9','/p/9.jpg'),('h10','/p/10.jpg');
             INSERT INTO faces (id,hash,bbox,embedding,cluster_id,person_label,confirmed,is_primary) VALUES
                (6,'h6','0,0,9,9',X'0000',9,NULL,0,0),
                (7,'h7','0,0,9,9',X'0000',9,NULL,0,0),
                (8,'h8','0,0,9,9',X'0000',9,NULL,0,0),
                (9,'h9','0,0,9,9',X'0000',3,NULL,0,0),
                (10,'h10','0,0,9,9',X'0000',NULL,'Bob',1,0);",
        )
        .unwrap();

        // Two calls on one connection: each builds fresh HashMaps, and Rust
        // seeds them differently, so an unstable order shows up here.
        let a = faces_list(&conn).unwrap();
        let b = faces_list(&conn).unwrap();

        let ids = |f: &FacesData| -> Vec<i64> { f.clusters.iter().map(|c| c.cluster_id).collect() };
        let names =
            |f: &FacesData| -> Vec<String> { f.people.iter().map(|p| p.label.clone()).collect() };
        assert!(ids(&a).len() >= 3, "fixture must have several clusters");
        assert_eq!(
            ids(&a),
            ids(&b),
            "cluster order must not change between calls"
        );
        assert_eq!(
            names(&a),
            names(&b),
            "people order must not change between calls"
        );

        // And the order is the useful one: biggest first, so the cluster worth
        // the most labelling effort is where it is expected.
        let sizes: Vec<usize> = a.clusters.iter().map(|c| c.face_ids.len()).collect();
        let mut want = sizes.clone();
        want.sort_unstable_by(|x, y| y.cmp(x));
        assert_eq!(
            sizes, want,
            "clusters must be ordered largest first, got {sizes:?}"
        );
    }

    #[test]
    fn faces_list_splits_people_clusters_singletons() {
        let conn = seed();
        let d = faces_list(&conn).unwrap();
        assert_eq!(d.people.len(), 1);
        // Identity is the normalized form; what a reader sees is separate.
        assert_eq!(d.people[0].label, "alice");
        assert_eq!(d.people[0].full_name, "Alice");
        assert_eq!(
            d.people[0].representative_id, 1,
            "primary face is representative"
        );
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
        assert_eq!(
            c.faces.iter().map(|f| f.face_id).collect::<Vec<_>>(),
            vec![3, 4]
        );
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
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((cid, label, confirmed, prim), (None, None, 0, 0));
    }

    #[test]
    fn dissolve_cluster_nulls_cluster_id() {
        let conn = seed();
        dissolve_cluster(&conn, 7).unwrap();
        assert_eq!(faces_list(&conn).unwrap().clusters.len(), 0);
        assert_eq!(
            faces_list(&conn).unwrap().singletons.len(),
            3,
            "3,4 join 5 as singletons"
        );
    }

    #[test]
    fn delete_person_unassigns_without_touching_cluster() {
        let conn = seed();
        // Give one of Alice's faces a cluster_id so we can prove delete_person
        // leaves cluster_id intact (it must, so the face rejoins its cluster's
        // unassigned group rather than scattering to singletons).
        conn.execute("UPDATE faces SET cluster_id = 42 WHERE id = 1", [])
            .unwrap();
        delete_person(&conn, "Alice").unwrap();
        assert_eq!(faces_list(&conn).unwrap().people.len(), 0, "Alice is gone");
        let (cid, label, confirmed): (Option<i64>, Option<String>, i64) = conn
            .query_row(
                "SELECT cluster_id, person_label, confirmed FROM faces WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
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
            let mut s = conn
                .prepare("SELECT id FROM faces WHERE person_label='alice' AND is_primary=1")
                .unwrap();
            s.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(primaries, vec![2], "exactly one primary, now face 2");
    }

    #[test]
    fn rename_missing_person_is_not_found() {
        let conn = seed();
        assert!(matches!(
            rename_person(&conn, "Nobody", "X"),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn rename_onto_existing_person_conflicts() {
        let conn = seed();
        assign(&conn, &[3], "Bob").unwrap(); // Bob now exists
        assert!(matches!(
            rename_person(&conn, "Alice", "Bob"),
            Err(Error::Conflict)
        ));
    }

    #[test]
    fn rename_succeeds() {
        let conn = seed();
        rename_person(&conn, "Alice", "Alicia").unwrap();
        assert_eq!(person_detail(&conn, "Alicia").unwrap().faces.len(), 2);
        assert_eq!(person_detail(&conn, "Alice").unwrap().faces.len(), 0);
        // The identity moved with it, so the old people row is gone.
        let names: Vec<String> = conn
            .prepare("SELECT name FROM people ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(names, vec!["alicia".to_string()]);
    }

    #[test]
    fn renaming_only_the_spelling_keeps_the_identity() {
        // The common rename: correcting or extending what is shown, which must
        // not change the URL or touch a single face row.
        let conn = seed();
        set_full_name(&conn, "alice", "Alice Smith").unwrap();
        let (name, full): (String, String) = conn
            .query_row("SELECT name, full_name FROM people", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(name, "alice", "identity is unchanged");
        assert_eq!(full, "Alice Smith", "only the display name moved");
        assert_eq!(person_detail(&conn, "alice").unwrap().faces.len(), 2);
    }

    #[test]
    fn rename_to_empty_label_is_invalid() {
        let conn = seed();
        assert!(matches!(
            rename_person(&conn, "Alice", "   "),
            Err(Error::Invalid)
        ));
    }
}
