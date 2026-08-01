//! GPS coordinate clustering: groups nearby `(lat, lon)` points by
//! haversine distance into named location clusters, persisted to
//! `location_clusters` + `file_hashes.location_cluster_id`. See
//! docs/superpowers/specs/2026-08-01-location-clustering-design.md.

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
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }

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
                heap.push(HeapEntry { dist: new_d, i: i.min(k), j: i.max(k) });
            }
        }
    }

    (0..n).filter(|&r| alive[r]).map(|r| std::mem::take(&mut members[r])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
