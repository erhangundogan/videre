//! Collects the non-face owned labels for one photo hash (image dimensions,
//! resolved location name, zero-shot category) that a sidecar export needs.
//! Faces are gathered separately via `face_db::labeled_faces_by_hash`. Returns
//! plain data; the binary assembles the XMP model, because that type is
//! binary-only.

use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Default, PartialEq)]
pub struct Gathered {
    pub dims: Option<(u32, u32)>,
    pub location: Option<String>,
    pub category: Option<String>,
}

/// Best-effort: a missing row or column yields None fields, never an error, so an
/// export over a library that never ran classify or locations still writes faces
/// and marks.
pub fn gather_for_hash(conn: &Connection, hash: &str) -> rusqlite::Result<Gathered> {
    let dims = conn
        .query_row(
            "SELECT width, height FROM file_hashes
             WHERE hash = ?1 AND width IS NOT NULL AND height IS NOT NULL",
            [hash],
            |r| Ok((r.get::<_, u32>(0)?, r.get::<_, u32>(1)?)),
        )
        .optional()?;
    let location = conn
        .query_row(
            "SELECT lc.name FROM file_hashes fh
             JOIN location_clusters lc ON lc.id = fh.location_cluster_id
             WHERE fh.hash = ?1 AND lc.name IS NOT NULL",
            [hash],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    let category = conn
        .query_row(
            "SELECT category FROM classifications WHERE hash = ?1 LIMIT 1",
            [hash],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(Gathered {
        dims,
        location,
        category,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE file_hashes (path TEXT, hash TEXT, ext TEXT, mime TEXT,
                width INTEGER, height INTEGER, location_cluster_id INTEGER);
             CREATE TABLE location_clusters (id INTEGER PRIMARY KEY, centroid_lat REAL,
                centroid_lon REAL, name TEXT, photo_count INTEGER, radius_km REAL, created_at TEXT);
             CREATE TABLE classifications (model_id TEXT, hash TEXT, category TEXT,
                confidence REAL, classified_at TEXT, PRIMARY KEY (model_id, hash));
             INSERT INTO location_clusters VALUES (7, 41.0, 29.0, 'Kadıköy', 1, 0.5, '');
             INSERT INTO file_hashes VALUES ('/p.jpg','h1','jpg','image/jpeg',4000,3000,7);
             INSERT INTO classifications VALUES ('m','h1','photo',0.9,'');",
        )
        .unwrap();
        c
    }

    #[test]
    fn gathers_dims_location_and_category() {
        let c = setup();
        let g = gather_for_hash(&c, "h1").unwrap();
        assert_eq!(g.dims, Some((4000, 3000)));
        assert_eq!(g.location.as_deref(), Some("Kadıköy"));
        assert_eq!(g.category.as_deref(), Some("photo"));
    }

    #[test]
    fn missing_data_yields_none_not_error() {
        let c = setup();
        let g = gather_for_hash(&c, "absent").unwrap();
        assert_eq!(g.dims, None);
        assert_eq!(g.location, None);
        assert_eq!(g.category, None);
    }
}
