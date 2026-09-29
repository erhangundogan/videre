//! How long the last full place recompute and face regroup took, so `videre
//! watch` can choose per batch between redoing the whole partition and placing
//! only the new files.
//!
//! The cost of a full recompute depends on the library, not on the batch: one
//! new photo costs the same eight-minute place recompute on a 70k-file library
//! as a hundred do. So the switch is a time budget, measured, not a file count.

use anyhow::Result;
use rusqlite::Connection;
use std::time::Duration;

/// Milliseconds the last full `location_cluster::recompute_all` took.
pub const LOCATIONS: &str = "locations_recompute_ms";
/// Milliseconds the last full face regroup (`run_clustering`) took.
pub const FACES: &str = "faces_regroup_ms";

/// A full recompute that finished within this is cheap enough to run for every
/// watched batch.
pub const RECOMPUTE_BUDGET: Duration = Duration::from_secs(5);

/// Record how long a full recompute took.
pub fn record(conn: &Connection, key: &str, took: Duration) -> Result<()> {
    crate::library_state::set(conn, key, took.as_millis().min(i64::MAX as u128) as i64)
}

/// True only when a full run was recorded and took under the budget.
///
/// Unknown is not cheap: a library that never recomputed under this version
/// could be the 70k-file one, and watch's startup reconcile runs the full
/// recompute when the data changed, which records a time soon enough.
pub fn is_cheap(conn: &Connection, key: &str) -> Result<bool> {
    Ok(crate::library_state::get(conn, key)?
        .is_some_and(|ms| ms >= 0 && (ms as u128) < RECOMPUTE_BUDGET.as_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_not_cheap() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(!is_cheap(&conn, LOCATIONS).unwrap());
    }

    #[test]
    fn under_the_budget_is_cheap_and_over_is_not() {
        let conn = Connection::open_in_memory().unwrap();
        record(&conn, LOCATIONS, Duration::from_millis(4_000)).unwrap();
        assert!(is_cheap(&conn, LOCATIONS).unwrap());
        record(&conn, LOCATIONS, Duration::from_millis(6_000)).unwrap();
        assert!(
            !is_cheap(&conn, LOCATIONS).unwrap(),
            "a later record overwrites"
        );
    }

    #[test]
    fn the_two_keys_are_independent() {
        let conn = Connection::open_in_memory().unwrap();
        record(&conn, FACES, Duration::from_millis(100)).unwrap();
        assert!(is_cheap(&conn, FACES).unwrap());
        assert!(!is_cheap(&conn, LOCATIONS).unwrap());
    }
}
