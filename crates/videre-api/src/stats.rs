//! Facade over videre-core's library-wide aggregate stats. Thin wrapper so
//! callers go through one shared `Error`/`Result` type like every other
//! videre-api operation. (`videre mcp` may adopt this later, once its own
//! JSON contract question is resolved - see the Pass A design doc.)

use crate::error::Result;
use rusqlite::Connection;
pub use videre_core::library_stats::LibraryStats;

pub fn library_stats(conn: &Connection) -> Result<LibraryStats> {
    Ok(videre_core::library_stats::compute(conn)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_stats_returns_totals_from_an_empty_db() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path        TEXT PRIMARY KEY,
                hash        TEXT NOT NULL,
                size_bytes  INTEGER,
                ext         TEXT
            );",
        )
        .unwrap();

        let stats = library_stats(&conn).unwrap();
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_size_bytes, 0);
        assert_eq!(stats.faces_detected, 0); // no faces table - guarded, not an error
    }
}
