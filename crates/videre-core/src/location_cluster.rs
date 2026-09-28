//! GPS coordinate clustering: groups nearby `(lat, lon)` points by
//! haversine distance into named location clusters, persisted to
//! `location_clusters` + `file_hashes.location_cluster_id`.

use rusqlite::Connection;

const EARTH_RADIUS_KM: f64 = 6371.0;

/// Continents for the map view's world tier, as name plus approximate
/// south/west/north/east bounding boxes, checked in order.
pub const CONTINENTS: &[(&str, f64, f64, f64, f64)] = &[
    ("Antarctica", -90.0, -180.0, -60.0, 180.0),
    ("Oceania", -50.0, 110.0, 0.0, 180.0),
    ("Europe", 34.0, -25.0, 72.0, 45.0),
    ("Asia", -10.0, 25.0, 80.0, 180.0),
    ("Africa", -37.0, -20.0, 35.0, 52.0),
    ("North America", 7.0, -170.0, 85.0, -50.0),
    ("South America", -56.0, -82.0, 13.0, -34.0),
];

/// Returns the continent grouping used by the map view's world tier.
/// `Other` is the open-water fallback.
pub fn continent_of(lat: f64, lon: f64) -> &'static str {
    for (name, south, west, north, east) in CONTINENTS {
        if lat >= *south && lat <= *north && lon >= *west && lon <= *east {
            return name;
        }
    }
    "Other"
}

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

/// Registers the exact distance primitive for SQL queries that need to combine
/// indexed geographic bounds with a final great-circle check.
pub fn register_haversine_sql_function(conn: &Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "haversine_km",
        4,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(haversine_km(
                ctx.get(0)?,
                ctx.get(1)?,
                ctx.get(2)?,
                ctx.get(3)?,
            ))
        },
    )
}

use std::collections::{BinaryHeap, HashMap};

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

/// A cell's side as a share of the clustering radius. Coordinates in one
/// cell are one point to the clustering, so points up to a cell diagonal
/// apart always share a cluster: at the default 15km that is about 2.1km,
/// well below "which city" granularity. Measured on 25,000 synthetic
/// coordinates around three fully covered city hubs: radius/20 took 3.4s and
/// 630MB, radius/10 190ms and 48MB, for nearly the same clusters.
const CELL_DIVISOR: f64 = 10.0;

/// Average-linkage agglomerative clustering of `(lat, lon)` points by
/// haversine distance, same philosophy as `face_cluster.rs`'s
/// `agglomerate_average` (repeatedly merge the two closest clusters, where
/// cluster-to-cluster distance is the size-weighted average across every
/// member pair) but with no quality gate and no held-out singletons: every
/// point ends up in some cluster, since a GPS coordinate is always valid
/// data. Returns the member index-lists of every resulting cluster.
///
/// :warning: There is no n*n distance matrix, and there must not be one
/// again. A personal library concentrates around home cities: on one
/// measured library a fifth of all coordinate pairs were within the default
/// radius, and at 26,744 coordinates the old dense matrix claimed 5.3GB up
/// front and froze machines. So coordinates are first snapped into cells of
/// side `radius / CELL_DIVISOR` (5,644 coordinates became 273 cells), and the
/// cells are clustered as weighted points, keeping only the distances of
/// pairs within the radius.
pub fn cluster_by_distance(points: &[(f64, f64)], radius_km: f64) -> Vec<Vec<usize>> {
    if points.is_empty() {
        return Vec::new();
    }
    let cells = snap_cells(points, radius_km);
    let weights: Vec<f64> = cells.members.iter().map(|m| m.len() as f64).collect();
    agglomerate(&cells.centroids, &weights, radius_km)
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .flat_map(|cell| cells.members[cell].iter().copied())
                .collect()
        })
        .collect()
}

/// Coordinates snapped into cells: each cell's position is its members'
/// unweighted mean (as `centroid` computes it), and its members are indices
/// into the input.
struct Cells {
    centroids: Vec<(f64, f64)>,
    members: Vec<Vec<usize>>,
}

/// Latitude bands of height `side`, and within a band longitude cells as
/// wide as `side` at the band's poleward edge, so no cell is wider than
/// `side` anywhere in it. A non-positive (or non-finite) radius snaps
/// nothing: each distinct coordinate is its own cell.
fn snap_cells(points: &[(f64, f64)], radius_km: f64) -> Cells {
    let side_km = radius_km / CELL_DIVISOR;
    let height = (side_km / EARTH_RADIUS_KM).to_degrees();
    let snap = side_km > 0.0 && height.is_finite();
    let mut index: HashMap<(i64, i64), usize> = HashMap::new();
    let mut members: Vec<Vec<usize>> = Vec::new();
    for (p, &(lat, lon)) in points.iter().enumerate() {
        let key = if snap {
            let band = (lat / height).floor();
            let edge = (band * height).abs().max(((band + 1.0) * height).abs());
            let cos = edge.min(90.0).to_radians().cos();
            let column = if cos < 1e-6 {
                0.0
            } else {
                (lon / (height / cos)).floor()
            };
            (band as i64, column as i64)
        } else {
            (lat.to_bits() as i64, lon.to_bits() as i64)
        };
        let cell = *index.entry(key).or_insert_with(|| {
            members.push(Vec::new());
            members.len() - 1
        });
        members[cell].push(p);
    }
    let centroids = members.iter().map(|m| centroid(points, m)).collect();
    Cells { centroids, members }
}

/// Every pair of `points` within `radius_km`, as `(lower index, higher
/// index, distance)`. Candidates come from a latitude-sorted sweep: a pair
/// within the radius differs in latitude by at most `r / R`, and since
/// `hav(d) >= cos(lat1) cos(lat2) hav(dlon)`, in longitude by at most
/// `2 asin(sin(r / 2R) / cos(max |lat|))` (the whole circle near the poles),
/// wrapping the antimeridian. Every candidate is confirmed with
/// `haversine_km`; the bounds can only let extra candidates through.
fn within_radius_pairs(points: &[(f64, f64)], radius_km: f64) -> Vec<(usize, usize, f64)> {
    const SLACK_DEG: f64 = 1e-9;
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by(|&a, &b| points[a].0.total_cmp(&points[b].0));
    let max_dlat = (radius_km / EARTH_RADIUS_KM).to_degrees() + SLACK_DEG;
    let whole_circle = radius_km >= std::f64::consts::PI * EARTH_RADIUS_KM;
    let half_angle_sin = (radius_km / (2.0 * EARTH_RADIUS_KM)).sin();
    let mut pairs = Vec::new();
    for (a, &i) in order.iter().enumerate() {
        let (lat_i, lon_i) = points[i];
        for &j in &order[a + 1..] {
            let (lat_j, lon_j) = points[j];
            if lat_j - lat_i > max_dlat {
                break;
            }
            let cos = lat_i.abs().max(lat_j.abs()).min(90.0).to_radians().cos();
            let max_dlon = if whole_circle || half_angle_sin >= cos {
                180.0
            } else {
                (2.0 * (half_angle_sin / cos).asin()).to_degrees() + SLACK_DEG
            };
            let mut dlon = (lon_j - lon_i).abs() % 360.0;
            if dlon > 180.0 {
                dlon = 360.0 - dlon;
            }
            if dlon > max_dlon {
                continue;
            }
            let d = haversine_km(lat_i, lon_i, lat_j, lon_j);
            if d <= radius_km {
                pairs.push((i.min(j), i.max(j), d));
            }
        }
    }
    pairs
}

/// The weighted average-linkage distance between two groups of points,
/// computed from the points: what the merge recurrence yields, for a pair
/// whose distance was never stored.
fn mean_distance(points: &[(f64, f64)], weights: &[f64], a: &[usize], b: &[usize]) -> f64 {
    let mut sum = 0.0;
    let mut weight = 0.0;
    for &p in a {
        for &q in b {
            let w = weights[p] * weights[q];
            sum += w * haversine_km(points[p].0, points[p].1, points[q].0, points[q].1);
            weight += w;
        }
    }
    sum / weight
}

/// Average linkage over weighted points, each starting as a cluster of size
/// `weights[p]`. `rows[i]` holds `i`'s distance to every alive cluster within
/// the radius, and only those, symmetrically. That is enough: a pair absent
/// from both merging clusters' rows is farther than the radius from both, and
/// the merged distance, a weighted average of the two, stays farther.
fn agglomerate(points: &[(f64, f64)], weights: &[f64], radius_km: f64) -> Vec<Vec<usize>> {
    let n = points.len();
    let mut rows: Vec<Option<HashMap<usize, f64>>> = (0..n).map(|_| Some(HashMap::new())).collect();
    let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();
    for (i, j, d) in within_radius_pairs(points, radius_km) {
        rows[i].as_mut().expect("alive").insert(j, d);
        rows[j].as_mut().expect("alive").insert(i, d);
        heap.push(HeapEntry { dist: d, i, j });
    }
    let mut members: Vec<Vec<usize>> = (0..n).map(|p| vec![p]).collect();
    let mut sizes: Vec<f64> = weights.to_vec();

    while let Some(HeapEntry { dist: d, i, j }) = heap.pop() {
        // Dead (either side merged away) or stale (superseded by a later
        // distance for the same pair): the stored value no longer matches.
        if rows[i].as_ref().and_then(|row| row.get(&j)) != Some(&d) {
            continue;
        }
        let row_i = rows[i].take().expect("alive");
        let row_j = rows[j].take().expect("alive");
        let (size_i, size_j) = (sizes[i], sizes[j]);
        let mut others: Vec<usize> = row_i
            .keys()
            .chain(row_j.keys())
            .copied()
            .filter(|&k| k != i && k != j)
            .collect();
        others.sort_unstable();
        others.dedup();
        let mut merged: HashMap<usize, f64> = HashMap::new();
        for k in others {
            let d_ik = row_i
                .get(&k)
                .copied()
                .unwrap_or_else(|| mean_distance(points, weights, &members[i], &members[k]));
            let d_jk = row_j
                .get(&k)
                .copied()
                .unwrap_or_else(|| mean_distance(points, weights, &members[j], &members[k]));
            let new_d = (size_i * d_ik + size_j * d_jk) / (size_i + size_j);
            let row_k = rows[k].as_mut().expect("a neighbour is alive");
            row_k.remove(&i);
            row_k.remove(&j);
            if new_d <= radius_km {
                row_k.insert(i, new_d);
                merged.insert(k, new_d);
                heap.push(HeapEntry {
                    dist: new_d,
                    i: i.min(k),
                    j: i.max(k),
                });
            }
        }
        let moved = std::mem::take(&mut members[j]);
        members[i].extend(moved);
        sizes[i] = size_i + size_j;
        rows[i] = Some(merged);
    }

    (0..n)
        .filter(|&r| rows[r].is_some())
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
/// The radius the last recompute ran at, the watcher's gate input (it skips a
/// manual, non-default radius). Distinct from `location_clusters.radius_km`,
/// which records the radius per produced cluster: this is the gate's datum, that
/// is the recompute's per-cluster output. Do not collapse one into the other.
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

/// One location cluster produced by [`recompute_all`]: the same five fields
/// the standalone command serializes.
pub struct RecomputedCluster {
    pub id: i64,
    pub name: Option<String>,
    pub centroid_lat: f64,
    pub centroid_lon: f64,
    pub photo_count: i64,
}

/// The full location recompute: wipes `location_clusters` and every
/// `location_cluster_id`, then reclusters over every distinct GPS coordinate
/// and names each cluster from its centroid. One transaction. `quiet`
/// suppresses the progress reporting. Cluster ids are not stable across runs,
/// by design (see the location clustering design spec, section 1).
///
/// Deliberately takes no selection: the recompute is global. It drops every
/// cluster and clears every `location_cluster_id` first, so clustering a
/// scoped subset would not narrow the work, it would leave every file outside
/// the scope permanently unclustered. A partial recompute of a global
/// partition is data loss, not a filter.
pub fn recompute_all(
    conn: &Connection,
    cache: &crate::library::CachePaths,
    radius_km: f64,
    quiet: bool,
) -> anyhow::Result<Vec<RecomputedCluster>> {
    let tx = conn.unchecked_transaction()?;

    ensure_location_clusters_table(&tx)?;
    ensure_gps_index(&tx);

    let coords: Vec<(f64, f64)> = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT gps_lat, gps_lon FROM file_hashes \
             WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    // Children first: under enforced foreign keys a parent delete with
    // referencing rows would fail, so the references are cleared before the
    // clusters are removed.
    tx.execute(
        "UPDATE file_hashes SET location_cluster_id = NULL WHERE location_cluster_id IS NOT NULL",
        [],
    )?;
    tx.execute("DELETE FROM location_clusters", [])?;

    if coords.is_empty() {
        tx.commit()?;
        store_recompute_state(conn, radius_km)?;
        return Ok(Vec::new());
    }

    // The clustering maths is sub-second (see `cluster_by_distance`); the cost
    // is the per-coordinate UPDATE below, run once per distinct coordinate,
    // which has its own progress.
    if !quiet {
        tracing::info!(
            "Clustering {} distinct coordinate(s) at radius {}km...",
            coords.len(),
            radius_km
        );
    }
    let member_groups = cluster_by_distance(&coords, radius_km);

    if !quiet {
        tracing::info!(
            "{} cluster(s); naming them and assigning photos",
            member_groups.len()
        );
    }
    let progress =
        crate::progress::Progress::new_counting(coords.len() as u64, quiet, "coordinates");

    let mut clusters = Vec::with_capacity(member_groups.len());
    for members in &member_groups {
        let (centroid_lat, centroid_lon) = centroid(&coords, members);
        // Cache-aware and fail-loud: a place-name lookup that cannot materialize
        // its dataset propagates rather than silently producing a different name.
        let name = crate::location::location_name_in(cache, centroid_lat, centroid_lon)?;

        tx.execute(
            "INSERT INTO location_clusters \
             (centroid_lat, centroid_lon, name, photo_count, radius_km, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            rusqlite::params![centroid_lat, centroid_lon, name, 0i64, radius_km],
        )?;
        let id = tx.last_insert_rowid();

        let mut photo_count = 0i64;
        for &idx in members {
            let (lat, lon) = coords[idx];
            // Exact equality, not ROUND(): `coords` came from SELECT DISTINCT
            // gps_lat, gps_lon, so matching them back exactly returns exactly the
            // rows they came from (ROUND double-counted coordinates differing past
            // the 6th decimal), and a function on the column would make the index
            // unusable, so this stays a single indexed lookup per coordinate.
            let affected = tx.execute(
                "UPDATE file_hashes SET location_cluster_id = ?1 \
                 WHERE gps_lat = ?2 AND gps_lon = ?3",
                rusqlite::params![id, lat, lon],
            )?;
            photo_count += affected as i64;
            progress.tick();
        }

        tx.execute(
            "UPDATE location_clusters SET photo_count = ?1 WHERE id = ?2",
            rusqlite::params![photo_count, id],
        )?;

        clusters.push(RecomputedCluster {
            id,
            name,
            centroid_lat,
            centroid_lon,
            photo_count,
        });
    }
    progress.finish();

    clusters.sort_by_key(|c| std::cmp::Reverse(c.photo_count));
    tx.commit()?;
    store_recompute_state(conn, radius_km)?;
    Ok(clusters)
}

/// Record what a recompute covered, so the watcher's fingerprint gate and
/// `videre status` can tell current clusters from stale ones. The fingerprint
/// is over the GPS data, which the recompute does not change, so reading it
/// back now yields the library's current fingerprint.
fn store_recompute_state(conn: &Connection, radius_km: f64) -> anyhow::Result<()> {
    let fingerprint = gps_fingerprint(conn)?;
    crate::library_state::set_string(conn, LOCATIONS_GPS_FINGERPRINT, &fingerprint)?;
    crate::library_state::set_string(conn, LOCATIONS_RADIUS, &format!("{radius_km}"))?;
    Ok(())
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

    fn temp_cache() -> crate::library::CachePaths {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        // The cities dataset is materialized under `geo` during the test; keep
        // the directory alive by leaking the guard (a test-only temp dir).
        std::mem::forget(dir);
        crate::library::CachePaths {
            thumbnails: base.join("thumbnails"),
            geo: base.join("geo"),
            base,
        }
    }

    #[test]
    fn recompute_all_clusters_assigns_and_counts() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, gps_lat REAL, gps_lon REAL,
                location_cluster_id INTEGER);
             INSERT INTO file_hashes (path, gps_lat, gps_lon) VALUES
               ('a', 52.5, 13.4), ('b', 52.5001, 13.4001), ('c', -33.87, 151.21);",
        )
        .unwrap();
        let cache = temp_cache();
        let clusters = recompute_all(&conn, &cache, 15.0, true).unwrap();
        // Two Berlin-area coordinates cluster; Sydney stands alone; every row
        // gets an assignment and a count.
        assert_eq!(clusters.len(), 2, "{:?}", clusters.len());
        let total: i64 = clusters.iter().map(|c| c.photo_count).sum();
        assert_eq!(total, 3, "every photo lands in exactly one cluster");
        let assigned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_hashes WHERE location_cluster_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(assigned, 3);
    }

    #[test]
    fn recompute_all_on_empty_gps_wipes_cleanly() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, gps_lat REAL, gps_lon REAL,
                location_cluster_id INTEGER);",
        )
        .unwrap();
        let cache = temp_cache();
        // Seed a stale cluster (the table must exist first), then recompute over
        // no GPS rows: the wipe must clear it.
        ensure_location_clusters_table(&conn).unwrap();
        conn.execute(
            "INSERT INTO location_clusters \
             (centroid_lat, centroid_lon, name, photo_count, radius_km, created_at) \
             VALUES (52.5, 13.4, 'ghost', 1, 15.0, datetime('now'))",
            [],
        )
        .unwrap();
        recompute_all(&conn, &cache, 15.0, true).unwrap();
        let clusters: i64 = conn
            .query_row("SELECT COUNT(*) FROM location_clusters", [], |r| r.get(0))
            .unwrap();
        assert_eq!(clusters, 0, "the stale cluster must be wiped");
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
    fn registered_haversine_sql_function_matches_rust_distance() {
        let conn = Connection::open_in_memory().unwrap();
        register_haversine_sql_function(&conn).unwrap();
        let sql_distance: f64 = conn
            .query_row(
                "SELECT haversine_km(?1, ?2, ?3, ?4)",
                rusqlite::params![52.52, 13.405, 48.8566, 2.3522],
                |row| row.get(0),
            )
            .unwrap();
        let rust_distance = haversine_km(52.52, 13.405, 48.8566, 2.3522);
        assert!((sql_distance - rust_distance).abs() < 1e-9);
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

    /// The algorithm this module used before cells and sparse rows: a dense
    /// n*n matrix, kept as the oracle `agglomerate` must agree with.
    fn dense_reference(points: &[(f64, f64)], radius_km: f64) -> Vec<Vec<usize>> {
        let n = points.len();
        let mut dist: Vec<Vec<f64>> = vec![vec![0.0; n]; n];
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::new();
        for i in 0..n {
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
                continue;
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

    /// Order-free form of a partition, so two can be compared.
    fn canonical(mut partition: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        for group in &mut partition {
            group.sort_unstable();
        }
        partition.sort();
        partition
    }

    /// Deterministic noise in `[-1, 1)`.
    fn noise(state: &mut u64) -> f64 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*state >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }

    /// GPS-shaped fixtures: city hubs with scatter (Istanbul, Berlin,
    /// Tromsø at 69.6N), a chain of points 9km apart that average linkage
    /// must not string into one cluster at 15km, a pair either side of the
    /// antimeridian, and high-latitude pairs where a degree of longitude is
    /// short (84N, and 89.9N across the pole).
    fn gps_fixture(seed: u64) -> Vec<(f64, f64)> {
        let mut state = seed;
        let mut points = Vec::new();
        for (lat, lon, count) in [(41.01, 28.98, 60), (52.52, 13.405, 40), (69.65, 18.96, 20)] {
            for _ in 0..count {
                points.push((lat + noise(&mut state) * 0.2, lon + noise(&mut state) * 0.3));
            }
        }
        for step in 0..12 {
            points.push((38.0 + noise(&mut state) * 0.001, 27.0 + step as f64 * 0.103));
        }
        points.push((-16.5, 179.99));
        points.push((-16.5, -179.99));
        points.extend([(84.0, 10.0), (84.0, 11.5), (89.9, 0.0), (89.9, 180.0)]);
        points
    }

    #[test]
    fn sparse_linkage_matches_the_dense_matrix() {
        for seed in [1, 2, 3] {
            let points = gps_fixture(seed);
            let weights = vec![1.0; points.len()];
            for radius in [1.0, 15.0, 50.0, 10_000.0] {
                assert_eq!(
                    canonical(agglomerate(&points, &weights, radius)),
                    canonical(dense_reference(&points, radius)),
                    "seed {seed}, radius {radius}km"
                );
            }
        }
    }

    #[test]
    fn a_chain_is_not_strung_into_one_cluster() {
        let chain: Vec<(f64, f64)> = (0..12).map(|s| (38.0, 27.0 + s as f64 * 0.103)).collect();
        assert!(cluster_by_distance(&chain, 15.0).len() > 1);
    }

    #[test]
    fn within_radius_pairs_finds_exactly_the_brute_force_pairs() {
        let points = gps_fixture(7);
        for radius in [1.0, 15.0, 500.0] {
            let mut expected = Vec::new();
            for i in 0..points.len() {
                for j in (i + 1)..points.len() {
                    let d = haversine_km(points[i].0, points[i].1, points[j].0, points[j].1);
                    if d <= radius {
                        expected.push((i, j));
                    }
                }
            }
            let mut found: Vec<(usize, usize)> = within_radius_pairs(&points, radius)
                .into_iter()
                .map(|(i, j, _)| (i, j))
                .collect();
            found.sort_unstable();
            assert_eq!(found, expected, "radius {radius}km");
        }
    }

    #[test]
    fn nearby_coordinates_share_a_cell_and_distant_ones_do_not() {
        // ~10m apart, then ~2km apart, at radius 15 (1.5km cells).
        let cells = snap_cells(&[(41.0, 29.0), (41.00009, 29.0), (41.018, 29.0)], 15.0);
        let cell_of = |p: usize| cells.members.iter().position(|m| m.contains(&p));
        assert_eq!(cell_of(0), cell_of(1));
        assert_ne!(cell_of(0), cell_of(2));
    }

    #[test]
    fn a_cell_stays_within_about_its_side() {
        for lat in [0.0, 45.0, 70.0] {
            let points: Vec<(f64, f64)> = (0..200)
                .map(|k| {
                    (
                        lat + (k % 20) as f64 * 0.0007,
                        10.0 + (k / 20) as f64 * 0.0011,
                    )
                })
                .collect();
            let cells = snap_cells(&points, 15.0);
            for (cell, members) in cells.members.iter().enumerate() {
                let (clat, clon) = cells.centroids[cell];
                for &p in members {
                    let d = haversine_km(points[p].0, points[p].1, clat, clon);
                    let side = 15.0 / CELL_DIVISOR;
                    assert!(d <= side * 1.5, "{d}km from its cell at latitude {lat}");
                }
            }
        }
    }

    #[test]
    fn a_zero_radius_snaps_nothing() {
        let cells = snap_cells(&[(1.0, 1.0), (1.0, 1.0), (1.0, 1.0000001)], 0.0);
        assert_eq!(cells.members, vec![vec![0, 1], vec![2]]);
        assert_eq!(
            cluster_by_distance(&[(1.0, 1.0), (1.0, 1.0000001)], 0.0).len(),
            2
        );
    }

    /// Scale check, run by hand: `cargo test -p videre-core --release --lib
    /// clusters_a_large_library_quickly -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn clusters_a_large_library_quickly() {
        let mut state = 42;
        let mut points = Vec::new();
        for (lat, lon) in [(41.01, 28.98), (52.52, 13.405), (40.71, -74.0)] {
            for _ in 0..5_833 {
                points.push((
                    lat + noise(&mut state) * 0.18,
                    lon + noise(&mut state) * 0.24,
                ));
            }
        }
        for _ in 0..7_500 {
            points.push((noise(&mut state) * 70.0, noise(&mut state) * 180.0));
        }
        let started = std::time::Instant::now();
        let clusters = cluster_by_distance(&points, DEFAULT_CLUSTER_RADIUS_KM);
        eprintln!(
            "{} coordinates -> {} clusters in {:?}",
            points.len(),
            clusters.len(),
            started.elapsed()
        );
    }

    #[test]
    fn centroid_is_unweighted_mean() {
        let points = vec![(0.0, 0.0), (2.0, 4.0)];
        let (lat, lon) = centroid(&points, &[0, 1]);
        assert!((lat - 1.0).abs() < 1e-9);
        assert!((lon - 2.0).abs() < 1e-9);
    }

    #[test]
    fn continent_of_assigns_representative_points() {
        let cases = [
            (52.52, 13.405, "Europe"),
            (35.68, 139.69, "Asia"),
            (-33.87, 151.21, "Oceania"),
            (40.71, -74.01, "North America"),
            (-23.55, -46.63, "South America"),
            (6.45, 3.39, "Africa"),
            (-75.0, 0.0, "Antarctica"),
            (55.75, 37.62, "Europe"),
            (-41.29, 174.78, "Oceania"),
        ];
        for (lat, lon, expected) in cases {
            assert_eq!(continent_of(lat, lon), expected, "at {lat},{lon}");
        }
    }

    #[test]
    fn continent_of_names_the_ocean_fallback_for_unmatched_points() {
        assert_eq!(continent_of(0.0, -30.0), "Other");
    }

    #[test]
    fn ensure_location_clusters_table_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_location_clusters_table(&conn).unwrap();
        ensure_location_clusters_table(&conn).unwrap(); // second call must not error
    }

    #[test]
    fn recompute_clears_foreign_key_children_first() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        ensure_location_clusters_table(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, gps_lat REAL, gps_lon REAL,
                location_cluster_id INTEGER REFERENCES location_clusters(id)
                    ON DELETE RESTRICT ON UPDATE RESTRICT
            );
            INSERT INTO location_clusters (id, centroid_lat, centroid_lon, name, photo_count, radius_km, created_at)
            VALUES (1, 52.52, 13.40, 'berlin', 1, 25.0, datetime('now'));
            INSERT INTO file_hashes (path, hash, gps_lat, gps_lon, location_cluster_id)
            VALUES ('/p/x.jpg', 'x', 52.51, 13.39, 1);",
        )
        .unwrap();

        let cache = temp_cache();
        recompute_all(&conn, &cache, 15.0, true)
            .expect("recompute must clear child references before deleting the parent clusters");

        let clusters: i64 = conn
            .query_row("SELECT COUNT(*) FROM location_clusters", [], |r| r.get(0))
            .unwrap();
        let refs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_hashes WHERE location_cluster_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(clusters, 1, "the located shot regains a fresh cluster");
        assert_eq!(refs, 1, "its reference points at the fresh cluster");
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }
}
