use rusqlite::Connection;

/// File paths containing confirmed faces for the given person label.
pub fn search_by_person(
    conn: &Connection,
    name: &str,
    limit: Option<usize>,
) -> rusqlite::Result<Vec<String>> {
    // Same normalization as `query::by_person`, for the same reason: the UI
    // passes what a human typed, the database holds the identity form.
    let name = crate::person::normalize(name).unwrap_or_else(|| name.to_string());
    let name = name.as_str();
    let limit_sql = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
    let sql = format!(
        "SELECT DISTINCT fh.path
         FROM faces f
         JOIN file_hashes fh ON fh.hash = f.hash
         WHERE f.person_label = ?1 AND f.confirmed = 1
         ORDER BY fh.path{limit_sql}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![name], |r| r.get(0))?;
    rows.collect()
}

/// All distinct person labels with at least one confirmed face.
pub fn list_persons(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        // Shows what a reader recognises. LEFT JOIN so a label with no people
        // row yet - one written before the table existed - still appears.
        "SELECT DISTINCT COALESCE(p.full_name, f.person_label) FROM faces f
         LEFT JOIN people p ON p.name = f.person_label
         WHERE f.person_label IS NOT NULL AND f.confirmed = 1
         ORDER BY 1",
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
             size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
             phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
             width INTEGER, height INTEGER);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0);
             CREATE TABLE IF NOT EXISTS people (name TEXT PRIMARY KEY, full_name TEXT NOT NULL);
             INSERT INTO people (name, full_name) VALUES ('alice','Alice'),('bob','Bob');
             INSERT INTO file_hashes VALUES ('/a.jpg','h1',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO file_hashes VALUES ('/b.jpg','h2',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO file_hashes VALUES ('/c.jpg','h3',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO faces VALUES (1,'h1','0,0,50,50',NULL,X'0000',0,'alice',1);
             INSERT INTO faces VALUES (2,'h2','0,0,50,50',NULL,X'0000',0,'alice',0);
             INSERT INTO faces VALUES (3,'h2','60,0,50,50',NULL,X'0000',1,'Bob',1);
             INSERT INTO faces VALUES (4,'h3','0,0,50,50',NULL,X'0000',NULL,NULL,0);"
        ).unwrap();
        crate::db::ensure_file_hashes_columns(conn);
    }

    #[test]
    fn returns_only_confirmed_paths_for_person() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        let paths = search_by_person(&conn, "Alice", None).unwrap();
        // h1 has confirmed=1 for Alice; h2 has confirmed=0, so skipped
        assert_eq!(paths, vec!["/a.jpg"]);
    }

    #[test]
    fn limit_is_respected() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
             size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
             phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0);
             CREATE TABLE IF NOT EXISTS people (name TEXT PRIMARY KEY, full_name TEXT NOT NULL);
             INSERT INTO people (name, full_name) VALUES ('alice','Alice'),('bob','Bob');
             INSERT INTO file_hashes VALUES ('/x.jpg','hx',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO file_hashes VALUES ('/y.jpg','hy',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO file_hashes VALUES ('/z.jpg','hz',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO faces VALUES (1,'hx','0,0,10,10',NULL,X'0000',0,'alice',1);
             INSERT INTO faces VALUES (2,'hy','0,0,10,10',NULL,X'0000',0,'alice',1);
             INSERT INTO faces VALUES (3,'hz','0,0,10,10',NULL,X'0000',0,'alice',1);"
        ).unwrap();
        crate::db::ensure_file_hashes_columns(&conn);
        let paths = search_by_person(&conn, "Alice", Some(2)).unwrap();
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn unknown_person_returns_empty() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        assert!(search_by_person(&conn, "Nobody", None).unwrap().is_empty());
    }

    #[test]
    fn list_persons_returns_confirmed_labels() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        let names = list_persons(&conn).unwrap();
        assert_eq!(names, vec!["Alice", "Bob"]);
    }
}
