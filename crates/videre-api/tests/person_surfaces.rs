//! Every surface that resolves or displays a person name, exercised against
//! one person whose display name **diverges from their identity**.
//!
//! This file exists because the person-identity change shipped four defects in
//! a row, each on a different surface, each found by the user rather than by a
//! test. They were not four unrelated mistakes: they were one mistake, made
//! once per surface, because nothing asserted that the surfaces agree.
//!
//! :warning: **The divergence is the whole point.** While a person's display
//! name still normalizes back to their identity - `Özgür Demirtaş` to
//! `ozgur_demirtas` - the identity path satisfies every assertion on its own
//! and the display path is never exercised. Tests written that way passed
//! while two surfaces were broken. Divergence appears the moment someone
//! edits a display name, which is a feature this same change introduced.
//!
//! A new surface belongs here. If it cannot be added, that means it does not
//! go through the shared resolver, which is the bug this file is guarding.

use rusqlite::Connection;

/// Identity and display name deliberately do not normalize to each other.
const IDENTITY: &str = "ozgur_demirtas";
const DISPLAY: &str = "Özgür";

fn library() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
         bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
         cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
         is_primary INTEGER DEFAULT 0);
         CREATE TABLE people (name TEXT PRIMARY KEY, full_name TEXT NOT NULL);
         INSERT INTO people VALUES ('ozgur_demirtas','Özgür');
         INSERT INTO file_hashes (path, hash) VALUES ('/a.jpg','h1');
         INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed)
         VALUES (1,'h1','0,0,10,10',X'0000','ozgur_demirtas',1);",
    )
    .unwrap();
    conn
}

#[test]
fn every_lookup_surface_finds_a_person_by_either_name() {
    let conn = library();

    // `videre search --person`, and the selection layer and MCP tool behind it.
    for typed in [DISPLAY, "özgür", "ÖZGÜR", IDENTITY] {
        assert_eq!(
            videre_core::query::by_person(&conn, typed).unwrap().len(),
            1,
            "query::by_person did not resolve {typed:?}"
        );
    }

    // The labeling UI's person search.
    for typed in [DISPLAY, IDENTITY] {
        assert_eq!(
            videre_core::person_search::search_by_person(&conn, typed, None)
                .unwrap()
                .len(),
            1,
            "person_search::search_by_person did not resolve {typed:?}"
        );
    }

    // The person page, reached by identity because that is what the URL holds.
    let detail = videre_api::person_detail(&conn, IDENTITY).unwrap();
    assert_eq!(detail.label, IDENTITY);
    assert_eq!(detail.full_name, DISPLAY);
    assert_eq!(detail.faces.len(), 1);
}

#[test]
fn every_display_surface_shows_the_display_name() {
    let conn = library();

    // The people list in the labeling UI, and the MCP stats tool behind it.
    let listed = videre_core::person_search::list_persons(&conn).unwrap();
    assert_eq!(listed, vec![DISPLAY.to_string()]);

    // Face overlays in `report --show-faces`.
    let overlays = videre_core::face_db::labeled_faces_by_hash(&conn).unwrap();
    // `LabeledFace` is `(face_id, person_label, bbox)`, and that middle
    // field is a display name despite the tuple's name.
    let names: Vec<_> = overlays
        .values()
        .flatten()
        .map(|(_, name, _)| name.as_str())
        .collect();
    assert_eq!(names, vec![DISPLAY], "face overlays showed the identity");

    // The clusters list.
    let faces = videre_api::faces_list(&conn).unwrap();
    let person = faces
        .people
        .iter()
        .find(|p| p.label == IDENTITY)
        .expect("person missing from faces_list");
    assert_eq!(person.full_name, DISPLAY);
}

#[test]
fn a_display_name_shared_by_two_people_keeps_them_separate() {
    // They stay two people - the identity is the key - but the name they share
    // cannot tell them apart, so a search for it returns both. Pinned because
    // it is a real ambiguity, not a bug: the fix is to report which people
    // matched, not to merge or to pick one.
    let conn = library();
    conn.execute_batch(
        "INSERT INTO people VALUES ('ozgur_tamer','Özgür');
         INSERT INTO file_hashes (path, hash) VALUES ('/b.jpg','h2');
         INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed)
         VALUES (2,'h2','0,0,10,10',X'0000','ozgur_tamer',1);",
    )
    .unwrap();

    assert_eq!(
        videre_core::query::by_person(&conn, DISPLAY).unwrap().len(),
        2
    );
    assert_eq!(
        videre_core::query::by_person(&conn, IDENTITY)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        videre_core::query::by_person(&conn, "ozgur_tamer")
            .unwrap()
            .len(),
        1
    );
}
