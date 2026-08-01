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
}
