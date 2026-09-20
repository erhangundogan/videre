//! GPS coordinate clustering: groups nearby `(lat, lon)` points by
//! haversine distance into named location clusters, persisted to
//! `location_clusters` + `file_hashes.location_cluster_id`. See
//! docs/superpowers/specs/2026-08-01-location-clustering-design.md.

use rusqlite::Connection;

const EARTH_RADIUS_KM: f64 = 6371.0;

/// Great-circle distance between two `(lat, lon)` points (in degrees), in km.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let lat1r = lat1.to_radians();
    let lat2r = lat2.to_radians();
    let a = (d_lat / 2.0).sin().powi(2) + lat1r.cos() * lat2r.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    EARTH_RADIUS_KM * c
}

use std::collections::BinaryHeap;

struct HeapEntry {
    dist: f64,
    i: usize,
    j: usize,
}
impl Eq for HeapEntry {}
impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist
    }
}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse so BinaryHeap (a max-heap) pops the smallest distance first.
        other.dist.total_cmp(&self.dist)
    }
}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Average-linkage agglomerative clustering of `(lat, lon)` points by
/// haversine distance, same philosophy as `face_cluster.rs`'s
/// `agglomerate_average` (repeatedly merge the two closest clusters, where
/// cluster-to-cluster distance is the size-weighted average across every
/// member pair) but with no quality gate and no held-out singletons: every
/// point ends up in some cluster, since a GPS coordinate is always valid
/// data. Returns the member index-lists of every resulting cluster.
pub fn cluster_by_distance(points: &[(f64, f64)], radius_km: f64) -> Vec<Vec<usize>> {
    cluster_by_distance_reporting(points, radius_km, |_, _| {})
}

/// `cluster_by_distance`, calling `on_row(done, total)` as the distance matrix
/// is filled.
///
/// The matrix build is the slow phase and it used to run silently. At 26,744
/// coordinates it is ~357 million haversine calls behind a
/// `vec![vec![0.0; n]; n]` allocation of about 5.7GB, and a caller that printed
/// "clustering..." and then said nothing for a minute was indistinguishable
/// from a hang - which is exactly how this was reported.
///
/// Reported per outer row rather than per pair: n callbacks instead of n^2/2,
/// so the reporting cannot itself become the cost.
pub fn cluster_by_distance_reporting<F>(
    points: &[(f64, f64)],
    radius_km: f64,
    mut on_row: F,
) -> Vec<Vec<usize>>
where
    F: FnMut(usize, usize),
{
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }

    let mut dist: Vec<Vec<f64>> = vec![vec![0.0; n]; n];
    let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();
    for i in 0..n {
        on_row(i, n);
        for j in (i + 1)..n {
            let d = haversine_km(points[i].0, points[i].1, points[j].0, points[j].1);
            dist[i][j] = d;
            dist[j][i] = d;
            if d <= radius_km {
                heap.push(HeapEntry { dist: d, i, j });
            }
        }
    }

    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut alive = vec![true; n];

    while let Some(HeapEntry { dist: d, i, j }) = heap.pop() {
        if !alive[i] || !alive[j] {
            continue;
        }
        if dist[i][j] != d {
            continue; // stale: superseded by a fresher push after i or j absorbed another cluster
        }
        if d > radius_km {
            break;
        }

        let size_i = members[i].len() as f64;
        let size_j = members[j].len() as f64;
        let moved = std::mem::take(&mut members[j]);
        members[i].extend(moved);
        alive[j] = false;
        for k in 0..n {
            if k == i || k == j || !alive[k] {
                continue;
            }
            let new_d = (size_i * dist[i][k] + size_j * dist[j][k]) / (size_i + size_j);
            if new_d != dist[i][k] {
                dist[i][k] = new_d;
                dist[k][i] = new_d;
                heap.push(HeapEntry {
                    dist: new_d,
                    i: i.min(k),
                    j: i.max(k),
                });
            }
        }
    }

    (0..n)
        .filter(|&r| alive[r])
        .map(|r| std::mem::take(&mut members[r]))
        .collect()
}

/// Unweighted mean of the given members' `(lat, lon)` coordinates, not
/// weighted by how many photos each coordinate has (see the spec's
/// disambiguation of `centroid_lat`/`centroid_lon` vs. `photo_count`).
pub fn centroid(points: &[(f64, f64)], member_idxs: &[usize]) -> (f64, f64) {
    let n = member_idxs.len() as f64;
    let sum_lat: f64 = member_idxs.iter().map(|&i| points[i].0).sum();
    let sum_lon: f64 = member_idxs.iter().map(|&i| points[i].1).sum();
    (sum_lat / n, sum_lon / n)
}

/// Idempotent: creates `location_clusters` if it doesn't already exist.
pub fn ensure_location_clusters_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS location_clusters (
            id            INTEGER PRIMARY KEY,
            centroid_lat  REAL NOT NULL,
            centroid_lon  REAL NOT NULL,
            name          TEXT,
            photo_count   INTEGER NOT NULL,
            radius_km     REAL NOT NULL,
            created_at    TEXT NOT NULL
        );",
    )
}

/// Idempotent: adds `file_hashes.location_cluster_id` if it doesn't already
/// exist. Mirrors `location::ensure_location_column`'s pattern, errors
/// (column already exists) are ignored.
pub fn ensure_location_cluster_id_column(conn: &Connection) {
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN location_cluster_id INTEGER");
}

/// Index the GPS columns, so assigning photos to a cluster is a lookup rather
/// than a table scan.
///
/// The recompute runs one UPDATE per distinct coordinate. Without this index
/// each of those scans the whole table: measured at 412s for 26,744 coordinates
/// against 70,601 rows.
///
/// The index is only usable because the UPDATE matches coordinates *exactly*.
/// It previously matched `ROUND(gps_lat, 6) = ROUND(?, 6)`, and a function call
/// on the column makes any index unusable no matter what is created here.
pub fn ensure_gps_index(conn: &Connection) {
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_file_hashes_gps ON file_hashes(gps_lat, gps_lon)",
    );
}

/// library_state keys for the location recluster gate: the GPS fingerprint the
/// last recompute covered, and the radius it ran at.
pub const LOCATIONS_GPS_FINGERPRINT: &str = "locations_gps_fingerprint";
pub const LOCATIONS_RADIUS: &str = "locations_radius";

/// The recompute radius the watcher runs at; a standalone run with a different
/// radius is a manual choice the watcher respects (see the watch stage) and
/// records instead of overriding.
pub const DEFAULT_CLUSTER_RADIUS_KM: f64 = 15.0;

/// A fingerprint over the GPS-bearing data: GPS-bearing row count, distinct
/// coordinate count, and coordinate sums accumulated in
/// `ORDER BY gps_lat, gps_lon` order (the GPS index serves this), so the same
/// data always produces the same fingerprint. The row count is the component
/// `photo_count` cares about: dedupe removing one copy of a pair changes it
/// while the coordinate set stays put.
///
/// Format: `v1:{rows}:{distinct}:{sum_lat}:{sum_lon}`.
pub fn gps_fingerprint(conn: &Connection) -> rusqlite::Result<String> {
    let rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_hashes
         WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT gps_lat, gps_lon FROM file_hashes
         WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL
         ORDER BY gps_lat, gps_lon",
    )?;
    let mut distinct = 0i64;
    let mut sum_lat = 0.0f64;
    let mut sum_lon = 0.0f64;
    let mut last: Option<(f64, f64)> = None;
    let mut iter = stmt.query_map([], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))?;
    while let Some((lat, lon)) = iter.next().transpose()? {
        if last != Some((lat, lon)) {
            distinct += 1;
        }
        sum_lat += lat;
        sum_lon += lon;
        last = Some((lat, lon));
    }
    Ok(format!("v1:{rows}:{distinct}:{sum_lat}:{sum_lon}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gps_fingerprint_is_stable_for_identical_data() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, gps_lat REAL, gps_lon REAL);
             INSERT INTO file_hashes VALUES ('a', 52.5, 13.4), ('b', 52.5, 13.4), ('c', 48.8, 2.35);",
        )
        .unwrap();
        let first = gps_fingerprint(&conn).unwrap();
        let second = gps_fingerprint(&conn).unwrap();
        assert_eq!(first, second, "identical data, identical fingerprint");
    }

    #[test]
    fn gps_fingerprint_changes_when_gps_data_changes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, gps_lat REAL, gps_lon REAL);
             INSERT INTO file_hashes VALUES ('a', 52.5, 13.4);",
        )
        .unwrap();
        let before = gps_fingerprint(&conn).unwrap();

        // A coordinate moves: the fingerprint must see it.
        conn.execute("UPDATE file_hashes SET gps_lon = 14.0", [])
            .unwrap();
        let moved = gps_fingerprint(&conn).unwrap();
        assert_ne!(before, moved, "a moved coordinate is a change");

        // A row is added with a NEW coordinate: seen.
        conn.execute("INSERT INTO file_hashes VALUES ('b', 48.8, 2.35)", [])
            .unwrap();
        let added = gps_fingerprint(&conn).unwrap();
        assert_ne!(moved, added, "a new coordinate is a change");

        // A row is REMOVED while its coordinate survives on another row: the
        // coordinate set is unchanged but the row count dropped, and
        // photo_count counts rows, so this must be visible too.
        conn.execute("DELETE FROM file_hashes WHERE path = 'b'", [])
            .unwrap();
        conn.execute("UPDATE file_hashes SET gps_lon = 13.4", [])
            .unwrap();
        let shrunk = gps_fingerprint(&conn).unwrap();
        assert_ne!(added, shrunk, "losing a row is a change");
    }

    #[test]
    fn gps_fingerprint_on_a_library_without_gps_is_a_known_value() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, gps_lat REAL, gps_lon REAL);",
        )
        .unwrap();
        let fp = gps_fingerprint(&conn).unwrap();
        assert_eq!(
            fp, "v1:0:0:0:0",
            "document the empty shape: rows:distinct:sumlat:sumlon"
        );
    }

    #[test]
    fn the_gps_index_exists_and_serves_an_exact_match() {
        // The whole performance fix rests on this: an exact predicate can use
        // the index, and the ROUND() form it replaced never could, whatever
        // index exists.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, gps_lat REAL, gps_lon REAL);",
        )
        .unwrap();
        ensure_gps_index(&conn);
        ensure_gps_index(&conn); // idempotent, like its neighbours

        let exact: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT 1 FROM file_hashes WHERE gps_lat = 1.0 AND gps_lon = 2.0",
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(
            exact.contains("idx_file_hashes_gps"),
            "an exact match must use the index, got: {exact}"
        );

        let rounded: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT 1 FROM file_hashes \
                 WHERE ROUND(gps_lat, 6) = ROUND(1.0, 6) AND ROUND(gps_lon, 6) = ROUND(2.0, 6)",
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(
            rounded.contains("SCAN"),
            "ROUND() on the column must still scan - this is why it was removed: {rounded}"
        );
    }

    #[test]
    fn two_coordinates_that_round_alike_stay_separate() {
        // BUGS.md item 7. These differ past the 6th decimal, so ROUND(x, 6)
        // makes them equal and each cluster's UPDATE claimed both rows - the
        // same photo counted twice across two clusters.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, gps_lat REAL, gps_lon REAL);
             INSERT INTO file_hashes VALUES
               ('/a.jpg','a', 52.55360000001, 13.43),
               ('/b.jpg','b', 52.55360000002, 13.43);",
        )
        .unwrap();

        let exact: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_hashes WHERE gps_lat = 52.55360000001 AND gps_lon = 13.43",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exact, 1, "exact matching claims only its own row");

        let rounded: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_hashes \
                 WHERE ROUND(gps_lat, 6) = ROUND(52.55360000001, 6) AND ROUND(gps_lon, 6) = ROUND(13.43, 6)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rounded, 2, "ROUND claims both - the double-count");
    }

    #[test]
    fn haversine_zero_distance_for_identical_points() {
        let d = haversine_km(48.8566, 2.3522, 48.8566, 2.3522);
        assert!(d.abs() < 1e-9, "expected ~0, got {d}");
    }

    #[test]
    fn haversine_one_degree_of_latitude_is_about_111_km() {
        let d = haversine_km(0.0, 0.0, 1.0, 0.0);
        assert!((d - 111.19).abs() < 0.5, "expected ~111.19km, got {d}");
    }

    #[test]
    fn haversine_paris_to_london_is_about_343_km() {
        let d = haversine_km(48.8566, 2.3522, 51.5074, -0.1278);
        assert!((d - 343.0).abs() < 5.0, "expected ~343km, got {d}");
    }

    #[test]
    fn cluster_by_distance_empty_input_returns_empty() {
        assert!(cluster_by_distance(&[], 15.0).is_empty());
    }

    #[test]
    fn cluster_by_distance_single_point_is_its_own_cluster() {
        let clusters = cluster_by_distance(&[(48.8566, 2.3522)], 15.0);
        assert_eq!(clusters, vec![vec![0]]);
    }

    #[test]
    fn cluster_by_distance_groups_nearby_points_and_isolates_far_ones() {
        let points = vec![
            (48.8566, 2.3522),  // Paris
            (48.8606, 2.3376),  // ~2km from Paris
            (48.8530, 2.3499),  // ~1km from Paris
            (51.5074, -0.1278), // London, ~343km from Paris
        ];
        let clusters = cluster_by_distance(&points, 50.0);
        assert_eq!(clusters.len(), 2, "expected 2 clusters, got {clusters:?}");
        let mut sizes: Vec<usize> = clusters.iter().map(|c| c.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![1, 3]);
    }

    #[test]
    fn cluster_by_distance_all_points_merge_when_radius_is_huge() {
        let points = vec![(48.8566, 2.3522), (51.5074, -0.1278)];
        let clusters = cluster_by_distance(&points, 10_000.0);
        assert_eq!(clusters.len(), 1);
    }

    #[test]
    fn centroid_is_unweighted_mean() {
        let points = vec![(0.0, 0.0), (2.0, 4.0)];
        let (lat, lon) = centroid(&points, &[0, 1]);
        assert!((lat - 1.0).abs() < 1e-9);
        assert!((lon - 2.0).abs() < 1e-9);
    }

    #[test]
    fn ensure_location_clusters_table_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_location_clusters_table(&conn).unwrap();
        ensure_location_clusters_table(&conn).unwrap(); // second call must not error
    }

    #[test]
    fn ensure_location_cluster_id_column_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL);")
            .unwrap();
        crate::db::ensure_file_hashes_columns(&conn);
        conn.execute(
            "INSERT INTO file_hashes (path, hash) VALUES ('x', 'h1')",
            [],
        )
        .unwrap();
        ensure_location_cluster_id_column(&conn);
        ensure_location_cluster_id_column(&conn); // second call must not error
        conn.execute(
            "UPDATE file_hashes SET location_cluster_id = 1 WHERE path = 'x'",
            [],
        )
        .unwrap();
    }
}
