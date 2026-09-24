//! `/events`: photos grouped into time-and-place sessions, computed on the fly.
//!
//! A new event starts after a long quiet stretch (a time gap) or when the
//! camera has clearly moved to a different place (a GPS jump). Rows are ordered
//! by time; each event is therefore a contiguous run. Nothing is persisted:
//! the whole grouping is recomputed from the library on each request, the same
//! shape as the `/date` buckets.

use chrono::NaiveDateTime;
use videre_core::location_cluster::haversine_km;

mod place_groups;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaKind {
    Photo,
    Video,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TripRow {
    pub hash: String,
    pub capture: Option<NaiveDateTime>,
    pub gps: Option<(f64, f64)>,
    pub media: MediaKind,
    pub path: String,
    pub ext: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// Only stored full capture timestamps can establish travel chronology.
pub(crate) fn parse_capture(value: &str) -> Option<NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|d| d.naive_local())
        .ok()
        .or_else(|| {
            ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"]
                .iter()
                .find_map(|fmt| NaiveDateTime::parse_from_str(value, fmt).ok())
        })
}

/// A new outing starts after this much quiet time.
const GAP_HOURS: i64 = 6;
/// ...or when the camera has moved at least this far between two located shots.
const DIST_KM: f64 = 5.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EventRow {
    pub hash: String,
    pub when: NaiveDateTime,
    pub gps: Option<(f64, f64)>,
    pub path: String,
    pub ext: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EventSample {
    pub path: String,
    pub hash: String,
    pub ext: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Event {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub centroid: Option<(f64, f64)>,
    pub sample: EventSample,
    pub members: Vec<String>,
}

impl Event {
    /// The URL key: the compact start time plus the first member's short
    /// hash. The time alone is not unique, since a location split can fall
    /// between two shots taken in the same second (two cameras with synced
    /// clocks), and both events must stay addressable.
    pub(crate) fn key(&self) -> String {
        let first: String = self
            .members
            .first()
            .map(|hash| hash.chars().take(8).collect())
            .unwrap_or_default();
        format!("{}-{first}", self.start.format("%Y%m%dT%H%M%S"))
    }
}

/// Parse the string `EFFECTIVE_DATE_SQL` yields: EXIF wall-clock, the RFC3339
/// `modified_at` fallback (its wall-clock part), or a bare date.
pub(crate) fn parse_effective(value: &str) -> Option<NaiveDateTime> {
    // Scan stores exif_date with a `T`; the space form is accepted too.
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(value, fmt) {
            return Some(dt);
        }
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(dt.naive_local());
    }
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
}

struct Accumulator {
    start: NaiveDateTime,
    last_when: NaiveDateTime,
    last_gps: Option<(f64, f64)>,
    gps_sum: (f64, f64),
    gps_count: usize,
    sample: EventSample,
    members: Vec<String>,
}

impl Accumulator {
    fn new(row: &EventRow) -> Self {
        let mut acc = Accumulator {
            start: row.when,
            last_when: row.when,
            last_gps: None,
            gps_sum: (0.0, 0.0),
            gps_count: 0,
            sample: EventSample {
                path: row.path.clone(),
                hash: row.hash.clone(),
                ext: row.ext.clone(),
                width: row.width,
                height: row.height,
            },
            members: Vec::new(),
        };
        acc.push(row);
        acc
    }

    fn push(&mut self, row: &EventRow) {
        self.last_when = row.when;
        if let Some(gps) = row.gps {
            self.last_gps = Some(gps);
            self.gps_sum.0 += gps.0;
            self.gps_sum.1 += gps.1;
            self.gps_count += 1;
        }
        self.members.push(row.hash.clone());
    }

    fn finish(self) -> Event {
        let centroid = (self.gps_count > 0).then(|| {
            (
                self.gps_sum.0 / self.gps_count as f64,
                self.gps_sum.1 / self.gps_count as f64,
            )
        });
        Event {
            start: self.start,
            end: self.last_when,
            centroid,
            sample: self.sample,
            members: self.members,
        }
    }
}

/// Group time-ordered rows into events. Sorts defensively by `(when, hash)` so
/// the result does not depend on the query's row order.
pub(crate) fn segment(rows: &[EventRow]) -> Vec<Event> {
    let gap = chrono::Duration::hours(GAP_HOURS);
    let mut rows = rows.to_vec();
    rows.sort_by(|a, b| a.when.cmp(&b.when).then_with(|| a.hash.cmp(&b.hash)));

    let mut events = Vec::new();
    let mut current: Option<Accumulator> = None;
    for row in &rows {
        let split = match &current {
            None => true,
            Some(acc) => {
                let time_gap = row.when - acc.last_when > gap;
                let location_jump = match (acc.last_gps, row.gps) {
                    (Some((plat, plon)), Some((lat, lon))) => {
                        haversine_km(plat, plon, lat, lon) > DIST_KM
                    }
                    _ => false,
                };
                time_gap || location_jump
            }
        };
        if split {
            if let Some(acc) = current.take() {
                events.push(acc.finish());
            }
            current = Some(Accumulator::new(row));
        } else if let Some(acc) = current.as_mut() {
            acc.push(row);
        }
    }
    if let Some(acc) = current {
        events.push(acc.finish());
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(s: &str) -> chrono::NaiveDateTime {
        parse_effective(s).unwrap()
    }

    #[test]
    fn parse_capture_requires_full_time_and_keeps_wall_clock() {
        assert_eq!(
            parse_capture("2020-03-12T09:30:00"),
            Some(dt("2020-03-12 09:30:00"))
        );
        assert_eq!(
            parse_capture("2020-03-12 09:30:00"),
            Some(dt("2020-03-12 09:30:00"))
        );
        assert_eq!(
            parse_capture("2020-03-12T09:30:00+02:00"),
            Some(dt("2020-03-12 09:30:00"))
        );
        assert!(parse_capture("2020-03-12").is_none());
        assert!(parse_capture("not a date").is_none());
    }

    #[test]
    fn events_starting_in_the_same_second_get_distinct_keys() {
        // Two cameras with synced clocks, far apart: a location split between
        // rows that share a second. Both events must stay addressable.
        let events = segment(&[
            row("aaaa1111", "2021-08-10 14:32:07", Some((52.52, 13.40))),
            row("bbbb2222", "2021-08-10 14:32:07", Some((41.01, 28.98))),
        ]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].key(), "20210810T143207-aaaa1111");
        assert_eq!(events[1].key(), "20210810T143207-bbbb2222");
    }

    fn row(hash: &str, when: &str, gps: Option<(f64, f64)>) -> EventRow {
        EventRow {
            hash: hash.to_owned(),
            when: dt(when),
            gps,
            path: format!("/p/{hash}.jpg"),
            ext: "jpg".to_owned(),
            width: Some(4000),
            height: Some(3000),
        }
    }

    #[test]
    fn parse_effective_handles_exif_rfc3339_and_date_only() {
        assert_eq!(
            parse_effective("2021-08-10 14:32:07"),
            NaiveDate::from_ymd_opt(2021, 8, 10)
                .unwrap()
                .and_hms_opt(14, 32, 7)
        );
        // exif_date as scan stores it: ISO 8601 with a `T`, local wall-clock,
        // no offset.
        assert_eq!(
            parse_effective("2021-08-10T19:34:03"),
            NaiveDate::from_ymd_opt(2021, 8, 10)
                .unwrap()
                .and_hms_opt(19, 34, 3)
        );
        // modified_at is RFC3339 with an offset; the wall-clock part is kept.
        assert_eq!(
            parse_effective("2021-01-01T09:00:00+00:00"),
            NaiveDate::from_ymd_opt(2021, 1, 1)
                .unwrap()
                .and_hms_opt(9, 0, 0)
        );
        assert_eq!(
            parse_effective("2021-08-10"),
            NaiveDate::from_ymd_opt(2021, 8, 10)
                .unwrap()
                .and_hms_opt(0, 0, 0)
        );
        assert_eq!(parse_effective("not a date"), None);
    }

    #[test]
    fn a_long_time_gap_starts_a_new_event() {
        let rows = vec![
            row("a", "2021-08-10 10:00:00", None),
            row("b", "2021-08-10 11:00:00", None), // 1h later: same event
            row("c", "2021-08-10 20:00:00", None), // 9h later: new event
        ];
        let events = segment(&rows);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].members, vec!["a", "b"]);
        assert_eq!(events[1].members, vec!["c"]);
        assert_eq!(events[0].start, dt("2021-08-10 10:00:00"));
        assert_eq!(events[0].end, dt("2021-08-10 11:00:00"));
    }

    #[test]
    fn a_small_time_gap_stays_in_one_event() {
        let rows = vec![
            row("a", "2021-08-10 10:00:00", None),
            row("b", "2021-08-10 15:59:00", None), // 5h59m: still one event
        ];
        assert_eq!(segment(&rows).len(), 1);
    }

    #[test]
    fn a_large_location_jump_splits_within_one_time_window() {
        // Berlin then Istanbul minutes apart: same time window, far apart.
        let rows = vec![
            row("a", "2021-08-10 10:00:00", Some((52.52, 13.40))),
            row("b", "2021-08-10 10:05:00", Some((41.01, 28.98))),
        ];
        let events = segment(&rows);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].members, vec!["a"]);
        assert_eq!(events[1].members, vec!["b"]);
    }

    #[test]
    fn a_small_location_move_does_not_split() {
        let rows = vec![
            row("a", "2021-08-10 10:00:00", Some((52.5200, 13.4000))),
            row("b", "2021-08-10 10:05:00", Some((52.5210, 13.4010))), // ~150m
        ];
        assert_eq!(segment(&rows).len(), 1);
    }

    #[test]
    fn missing_gps_rows_never_split_and_do_not_move_the_reference() {
        // A GPS-less row between two Berlin rows must not split the session,
        // and the location reference stays Berlin so the third row does not
        // spuriously jump.
        let rows = vec![
            row("a", "2021-08-10 10:00:00", Some((52.52, 13.40))),
            row("b", "2021-08-10 10:05:00", None),
            row("c", "2021-08-10 10:10:00", Some((52.521, 13.401))),
        ];
        let events = segment(&rows);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].members, vec!["a", "b", "c"]);
    }

    #[test]
    fn the_centroid_is_the_mean_of_present_gps_and_none_without_any() {
        // Two nearby located shots (a few hundred metres) stay in one event, so
        // the centroid is their mean; the GPS-less middle row does not count.
        let with_gps = vec![
            row("a", "2021-08-10 10:00:00", Some((52.5200, 13.4000))),
            row("b", "2021-08-10 10:05:00", None),
            row("c", "2021-08-10 10:10:00", Some((52.5210, 13.4010))),
        ];
        let events = segment(&with_gps);
        assert_eq!(events.len(), 1);
        let centroid = events[0].centroid.unwrap();
        assert!((centroid.0 - 52.5205).abs() < 1e-9 && (centroid.1 - 13.4005).abs() < 1e-9);

        let no_gps = vec![row("a", "2021-08-10 10:00:00", None)];
        assert_eq!(segment(&no_gps)[0].centroid, None);
    }

    #[test]
    fn segmentation_is_deterministic_regardless_of_input_order() {
        // Two photos share a timestamp; the (when, hash) sort makes the order
        // and the sample stable no matter how the rows arrive.
        let a = row("a", "2021-08-10 10:00:00", None);
        let b = row("b", "2021-08-10 10:00:00", None);
        let forward = segment(&[a.clone(), b.clone()]);
        let reversed = segment(&[b, a]);
        assert_eq!(forward, reversed);
        assert_eq!(forward[0].members, vec!["a", "b"]);
        assert_eq!(forward[0].sample.hash, "a");
    }

    #[test]
    fn an_empty_input_yields_no_events() {
        assert!(segment(&[]).is_empty());
    }
}
