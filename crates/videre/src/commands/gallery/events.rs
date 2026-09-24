//! Read-only travel trips inferred from capture dates and photo locations.
//! Home, bounded places and exact membership are derived on demand.

use chrono::{Duration, NaiveDateTime};

mod place_groups;
mod trips;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EventsConfig {
    pub min_media_items: usize,
    pub time_window: Duration,
    pub place_radius_km: f64,
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            min_media_items: 10,
            time_window: Duration::hours(3),
            place_radius_km: 20.0,
        }
    }
}

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Trip {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub sample: TripRow,
    pub members: Vec<String>,
    pub anchor_when: NaiveDateTime,
    pub anchor_hash: String,
    pub dominant_stop: (f64, f64),
}

impl Trip {
    pub(crate) fn key(&self) -> String {
        format!(
            "{}-{}",
            self.anchor_when.format("%Y%m%dT%H%M%S"),
            self.anchor_hash
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmptyReason {
    NoMedia,
    NoCaptureDates,
    InsufficientLocationEvidence,
    NoQualifyingTrips,
}

impl EmptyReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoMedia => "no_media",
            Self::NoCaptureDates => "no_capture_dates",
            Self::InsufficientLocationEvidence => "insufficient_location_evidence",
            Self::NoQualifyingTrips => "no_qualifying_trips",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Detection {
    pub events: Vec<Trip>,
    pub empty_reason: Option<EmptyReason>,
}

pub(crate) fn detect(rows: &[TripRow]) -> Detection {
    detect_with_config(rows, &EventsConfig::default())
}

pub(crate) fn detect_with_config(rows: &[TripRow], config: &EventsConfig) -> Detection {
    use std::collections::BTreeMap;
    let mut by_hash = BTreeMap::<&str, &TripRow>::new();
    for row in rows {
        let prior = by_hash.get(row.hash.as_str()).copied();
        let better = prior.is_none_or(|old| {
            let quality = (row.capture.is_some(), row.gps.is_some());
            let old_quality = (old.capture.is_some(), old.gps.is_some());
            quality > old_quality || (quality == old_quality && row.path < old.path)
        });
        if better {
            by_hash.insert(&row.hash, row);
        }
    }
    let rows: Vec<TripRow> = by_hash.into_values().cloned().collect();
    let reason = if rows.is_empty() {
        Some(EmptyReason::NoMedia)
    } else if rows.iter().all(|row| row.capture.is_none()) {
        Some(EmptyReason::NoCaptureDates)
    } else {
        None
    };
    if let Some(empty_reason) = reason {
        return Detection {
            events: Vec::new(),
            empty_reason: Some(empty_reason),
        };
    }
    let places = place_groups::infer_places(&rows, config.place_radius_km);
    let episodes = trips::destination_episodes(&rows, &places);
    let events = trips::qualify_trips(&rows, &places, &episodes, config);
    let empty_reason = if events.is_empty() {
        Some(if places.groups.len() < 2 || episodes.is_empty() {
            EmptyReason::InsufficientLocationEvidence
        } else {
            EmptyReason::NoQualifyingTrips
        })
    } else {
        None
    };
    Detection {
        events,
        empty_reason,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    fn dt(s: &str) -> chrono::NaiveDateTime {
        parse_capture(s).unwrap()
    }

    fn trip_row(
        hash: &str,
        when: Option<&str>,
        gps: Option<(f64, f64)>,
        media: MediaKind,
    ) -> TripRow {
        TripRow {
            hash: hash.into(),
            capture: when.and_then(parse_capture),
            gps,
            media,
            path: format!("/p/{hash}.jpg"),
            ext: if media == MediaKind::Video {
                "mp4"
            } else {
                "jpg"
            }
            .into(),
            width: None,
            height: None,
        }
    }

    fn travel_fixture() -> Vec<TripRow> {
        let mut rows: Vec<_> = (0..13)
            .map(|i| {
                trip_row(
                    &format!("home-{i}"),
                    Some(&format!("2020-01-01T08:{i:02}:00")),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for (i, time) in ["10:00:00", "11:00:00", "12:00:00"].iter().enumerate() {
            rows.push(trip_row(
                &format!("anchor-{i}"),
                Some(&format!("2020-03-12T{time}")),
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ));
        }
        for i in 0..8 {
            rows.push(trip_row(
                &format!("plain-{i}"),
                Some(&format!("2020-03-12T10:{:02}:00", 10 + 5 * i)),
                None,
                MediaKind::Photo,
            ));
        }
        rows.push(trip_row(
            "video",
            Some("2020-03-12T11:40:00"),
            None,
            MediaKind::Video,
        ));
        rows.push(trip_row("undated", None, None, MediaKind::Photo));
        rows
    }

    #[test]
    fn travel_trip_has_exact_members_and_stable_full_hash_key() {
        let mut rows = travel_fixture();
        rows[13].hash = "aabbccdd".repeat(8);
        let result = detect(&rows);
        assert_eq!(result.events.len(), 1);
        let trip = &result.events[0];
        assert_eq!(trip.members.len(), 12);
        assert_eq!(
            trip.key(),
            format!("20200312T100000-{}", "aabbccdd".repeat(8))
        );
        assert!(trip.members.contains(&"video".to_owned()));
        assert!(!trip.members.contains(&"undated".to_owned()));
        assert!(!trip.members.iter().any(|hash| hash.starts_with("home")));
        assert_eq!(
            trip.members
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            trip.members.len()
        );
        let before = trip.key();
        rows.push(trip_row(
            "edge",
            Some("2020-03-12T09:30:00"),
            None,
            MediaKind::Photo,
        ));
        assert_eq!(detect(&rows).events[0].key(), before);
        rows.reverse();
        assert_eq!(detect(&rows).events[0].key(), before);
    }

    #[test]
    fn empty_reasons_distinguish_missing_time_location_and_volume() {
        assert_eq!(detect(&[]).empty_reason, Some(EmptyReason::NoMedia));
        assert_eq!(
            detect(&[trip_row(
                "undated",
                None,
                Some((52.52, 13.4)),
                MediaKind::Photo
            )])
            .empty_reason,
            Some(EmptyReason::NoCaptureDates)
        );
        assert_eq!(
            detect(&[trip_row(
                "plain",
                Some("2020-03-12T10:00:00"),
                None,
                MediaKind::Photo
            )])
            .empty_reason,
            Some(EmptyReason::InsufficientLocationEvidence)
        );
        let mut rows = travel_fixture();
        rows.retain(|r| !r.hash.starts_with("plain") && r.hash != "video");
        assert_eq!(
            detect(&rows).empty_reason,
            Some(EmptyReason::NoQualifyingTrips)
        );
    }

    #[test]
    fn video_location_and_gpsless_edge_membership_are_conservative() {
        let mut rows = travel_fixture();
        rows.push(trip_row(
            "matching-video",
            Some("2020-03-12T11:20:00"),
            Some((47.5, 19.0)),
            MediaKind::Video,
        ));
        rows.push(trip_row(
            "conflicting-video",
            Some("2020-03-12T11:21:00"),
            Some((41.0, 29.0)),
            MediaKind::Video,
        ));
        rows.push(trip_row(
            "ambiguous",
            Some("2020-03-12T11:20:30"),
            None,
            MediaKind::Photo,
        ));
        rows.push(trip_row(
            "before-edge",
            Some("2020-03-12T09:30:00"),
            None,
            MediaKind::Photo,
        ));
        rows.push(trip_row(
            "after-edge",
            Some("2020-03-12T12:30:00"),
            None,
            MediaKind::Video,
        ));
        rows.push(trip_row(
            "too-early",
            Some("2020-03-12T06:59:59"),
            None,
            MediaKind::Photo,
        ));
        rows.push(trip_row(
            "next-date",
            Some("2020-03-13T00:01:00"),
            None,
            MediaKind::Photo,
        ));
        let trip = detect(&rows).events.remove(0);
        for hash in ["matching-video", "before-edge", "after-edge"] {
            assert!(trip.members.contains(&hash.to_owned()), "missing {hash}");
        }
        for hash in [
            "conflicting-video",
            "ambiguous",
            "too-early",
            "next-date",
            "undated",
        ] {
            assert!(!trip.members.contains(&hash.to_owned()), "included {hash}");
        }
        assert_eq!(trip.start, dt("2020-03-12 09:30:00"));
        assert_eq!(trip.end, dt("2020-03-12 12:30:00"));
    }

    #[test]
    fn same_day_gate_requires_ten_items_and_three_anchors_in_three_hours() {
        let mut rows: Vec<_> = (0..13)
            .map(|i| {
                trip_row(
                    &format!("home-{i}"),
                    Some(&format!("2020-01-01T08:{i:02}:00")),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for (i, time) in ["10:00:00", "11:00:00", "13:00:00"].iter().enumerate() {
            rows.push(trip_row(
                &format!("anchor-{i}"),
                Some(&format!("2020-03-12T{time}")),
                Some((43.08, -79.07)),
                MediaKind::Photo,
            ));
        }
        for i in 0..7 {
            rows.push(trip_row(
                &format!("plain-{i}"),
                Some(&format!("2020-03-12T11:{i:02}:00")),
                None,
                MediaKind::Photo,
            ));
        }
        assert_eq!(detect(&rows).events.len(), 1);
        rows.pop();
        assert!(detect(&rows).events.is_empty());
        rows.push(trip_row(
            "plain-6",
            Some("2020-03-12T11:06:00"),
            None,
            MediaKind::Photo,
        ));
        rows.iter_mut()
            .find(|r| r.hash == "anchor-2")
            .unwrap()
            .capture = parse_capture("2020-03-12T13:00:01");
        assert!(detect(&rows).events.is_empty());
    }

    #[test]
    fn a_qualifying_three_hour_burst_can_cross_midnight() {
        let mut rows: Vec<_> = (0..13)
            .map(|i| {
                trip_row(
                    &format!("home-{i}"),
                    Some(&format!("2020-01-01T08:{i:02}:00")),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for (i, when) in [
            "2020-03-12T23:00:00",
            "2020-03-13T00:00:00",
            "2020-03-13T01:30:00",
        ]
        .iter()
        .enumerate()
        {
            rows.push(trip_row(
                &format!("anchor-{i}"),
                Some(when),
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ));
        }
        for (i, when) in [
            "2020-03-12T23:10:00",
            "2020-03-12T23:20:00",
            "2020-03-12T23:30:00",
            "2020-03-13T00:10:00",
            "2020-03-13T00:20:00",
            "2020-03-13T00:30:00",
            "2020-03-13T01:00:00",
        ]
        .iter()
        .enumerate()
        {
            rows.push(trip_row(
                &format!("plain-{i}"),
                Some(when),
                None,
                MediaKind::Photo,
            ));
        }

        let result = detect(&rows);
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].members.len(), 10);
        assert_eq!(result.events[0].start, dt("2020-03-12 23:00:00"));
        assert_eq!(result.events[0].end, dt("2020-03-13 01:30:00"));

        let mut with_extra = rows.clone();
        with_extra.push(trip_row(
            "plain-7",
            Some("2020-03-13T01:40:00"),
            None,
            MediaKind::Photo,
        ));
        let extended = detect(&with_extra);
        assert_eq!(extended.events.len(), 1);
        assert_eq!(extended.events[0].members.len(), 11);
        assert!(extended.events[0].members.contains(&"plain-7".to_owned()));

        let fewer: Vec<_> = rows
            .iter()
            .filter(|row| row.hash != "plain-6")
            .cloned()
            .collect();
        assert!(detect(&fewer).events.is_empty());
        rows.iter_mut()
            .find(|row| row.hash == "anchor-2")
            .unwrap()
            .capture = parse_capture("2020-03-13T02:00:01");
        assert!(detect(&rows).events.is_empty());
    }

    #[test]
    fn midnight_edge_files_join_within_the_configured_window() {
        let home: Vec<_> = (0..13)
            .map(|i| {
                trip_row(
                    &format!("home-{i}"),
                    Some(&format!("2020-01-01T08:{i:02}:00")),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        let cases = [
            (
                [
                    "2020-03-12T22:30:00",
                    "2020-03-12T23:00:00",
                    "2020-03-12T23:30:00",
                ],
                [
                    "2020-03-12T23:35:00",
                    "2020-03-12T23:40:00",
                    "2020-03-13T00:00:00",
                    "2020-03-13T00:10:00",
                    "2020-03-13T00:20:00",
                    "2020-03-13T00:30:00",
                    "2020-03-13T00:40:00",
                ],
            ),
            (
                [
                    "2020-03-13T00:30:00",
                    "2020-03-13T01:00:00",
                    "2020-03-13T01:30:00",
                ],
                [
                    "2020-03-12T23:20:00",
                    "2020-03-12T23:30:00",
                    "2020-03-12T23:40:00",
                    "2020-03-12T23:50:00",
                    "2020-03-13T00:05:00",
                    "2020-03-13T00:10:00",
                    "2020-03-13T00:20:00",
                ],
            ),
        ];
        for (anchors, plain) in cases {
            let mut rows = home.clone();
            for (i, when) in anchors.iter().enumerate() {
                rows.push(trip_row(
                    &format!("anchor-{i}"),
                    Some(when),
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ));
            }
            for (i, when) in plain.into_iter().enumerate() {
                rows.push(trip_row(
                    &format!("plain-{i}"),
                    Some(when),
                    None,
                    MediaKind::Photo,
                ));
            }
            let result = detect(&rows);
            assert_eq!(result.events.len(), 1, "anchors: {anchors:?}");
            assert_eq!(result.events[0].members.len(), 10);
        }
    }

    #[test]
    fn separate_midnight_bursts_do_not_merge_without_multi_day_evidence() {
        let mut rows: Vec<_> = (0..30)
            .map(|i| {
                trip_row(
                    &format!("home-{i}"),
                    Some(&format!("2020-01-01T08:{i:02}:00")),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for (i, when) in [
            "2020-03-12T23:00:00",
            "2020-03-13T00:00:00",
            "2020-03-13T00:30:00",
            "2020-03-13T23:00:00",
            "2020-03-13T23:30:00",
            "2020-03-14T00:30:00",
        ]
        .iter()
        .enumerate()
        {
            rows.push(trip_row(
                &format!("anchor-{i}"),
                Some(when),
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ));
        }
        for (i, when) in [
            "2020-03-12T23:05:00",
            "2020-03-12T23:10:00",
            "2020-03-12T23:15:00",
            "2020-03-12T23:20:00",
            "2020-03-13T00:05:00",
            "2020-03-13T00:10:00",
            "2020-03-13T00:15:00",
            "2020-03-13T23:05:00",
            "2020-03-13T23:10:00",
            "2020-03-13T23:15:00",
            "2020-03-13T23:20:00",
            "2020-03-14T00:05:00",
            "2020-03-14T00:10:00",
            "2020-03-14T00:15:00",
        ]
        .iter()
        .enumerate()
        {
            rows.push(trip_row(
                &format!("plain-{i}"),
                Some(when),
                None,
                MediaKind::Photo,
            ));
        }
        let events = detect(&rows).events;
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].members.len(), 10);
        assert_eq!(events[1].members.len(), 10);
        assert!(events
            .iter()
            .all(|trip| trip.end - trip.start <= Duration::hours(3)));
    }

    #[test]
    fn configured_item_minimum_controls_trip_qualification() {
        let rows = travel_fixture();
        assert_eq!(detect(&rows).events.len(), 1);
        let config = EventsConfig {
            min_media_items: 13,
            ..EventsConfig::default()
        };
        assert!(detect_with_config(&rows, &config).events.is_empty());
    }

    #[test]
    fn configured_time_window_controls_trip_qualification() {
        let rows = travel_fixture();
        assert_eq!(detect(&rows).events.len(), 1);
        let config = EventsConfig {
            time_window: Duration::minutes(30),
            ..EventsConfig::default()
        };
        assert!(detect_with_config(&rows, &config).events.is_empty());
    }

    #[test]
    fn configured_place_radius_controls_trip_boundaries() {
        let mut rows = travel_fixture();
        rows.iter_mut()
            .find(|row| row.hash == "anchor-2")
            .unwrap()
            .gps = Some((47.5, 19.30));
        assert!(detect(&rows).events.is_empty());
        let config = EventsConfig {
            place_radius_km: 30.0,
            ..EventsConfig::default()
        };
        assert_eq!(detect_with_config(&rows, &config).events.len(), 1);
    }

    #[test]
    fn distinct_away_places_must_each_qualify_as_a_trip() {
        let mut rows: Vec<_> = (0..13)
            .map(|i| {
                trip_row(
                    &format!("home-{i}"),
                    Some(&format!("2020-01-01T08:{i:02}:00")),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for (place, lon, hour) in [("first", 19.0, 10), ("second", 19.32, 15)] {
            for i in 0..3 {
                rows.push(trip_row(
                    &format!("{place}-anchor-{i}"),
                    Some(&format!("2020-03-12T{:02}:00:00", hour + i)),
                    Some((47.5, lon)),
                    MediaKind::Photo,
                ));
            }
            for i in 0..7 {
                rows.push(trip_row(
                    &format!("{place}-plain-{i}"),
                    Some(&format!("2020-03-12T{hour:02}:{:02}:00", 5 + i * 5)),
                    None,
                    MediaKind::Photo,
                ));
            }
        }
        let result = detect(&rows);
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0].members.len(), 10);
        assert_eq!(result.events[1].members.len(), 10);

        rows.retain(|row| !row.hash.starts_with("second-plain"));
        let result = detect(&rows);
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].members.len(), 10);
    }

    #[test]
    fn two_day_trip_qualifies_without_same_day_burst() {
        let mut rows: Vec<_> = (0..13)
            .map(|i| {
                trip_row(
                    &format!("home-{i}"),
                    Some(&format!("2020-01-01T08:{i:02}:00")),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for day in [12, 13] {
            for (j, hour) in [10, 16].iter().enumerate() {
                rows.push(trip_row(
                    &format!("anchor-{day}-{j}"),
                    Some(&format!("2020-03-{day}T{hour:02}:00:00")),
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ));
            }
            for i in 0..3 {
                rows.push(trip_row(
                    &format!("plain-{day}-{i}"),
                    Some(&format!("2020-03-{day}T11:{i:02}:00")),
                    None,
                    MediaKind::Photo,
                ));
            }
        }
        let result = detect(&rows);
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].members.len(), 10);
    }

    #[test]
    fn same_second_trips_have_distinct_keys_and_duplicate_hashes_do_not_count_twice() {
        let mut rows: Vec<_> = (0..30)
            .map(|i| {
                trip_row(
                    &format!("home-{i:02}"),
                    Some("2020-01-01T08:00:00"),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for i in 0..10 {
            rows.push(trip_row(
                &format!("a{i:02}"),
                Some("2020-03-12T10:00:00"),
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ));
            rows.push(trip_row(
                &format!("b{i:02}"),
                Some("2020-03-12T10:00:00"),
                Some((41.0, 29.0)),
                MediaKind::Photo,
            ));
        }
        let result = detect(&rows);
        assert_eq!(result.events.len(), 2);
        assert_ne!(result.events[0].key(), result.events[1].key());
        let before = result.events.clone();
        rows.push(trip_row(
            "a00",
            Some("2020-03-12T10:00:00"),
            Some((47.5, 19.0)),
            MediaKind::Photo,
        ));
        rows.reverse();
        assert_eq!(detect(&rows).events, before);
    }

    #[test]
    fn simultaneous_trips_claim_only_their_matching_located_videos() {
        let mut rows: Vec<_> = (0..30)
            .map(|i| {
                trip_row(
                    &format!("home-{i:02}"),
                    Some("2020-01-01T08:00:00"),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for i in 0..10 {
            for (prefix, gps) in [("a", (47.5, 19.0)), ("b", (41.0, 29.0))] {
                rows.push(trip_row(
                    &format!("{prefix}{i:02}"),
                    Some("2020-03-12T10:00:00"),
                    Some(gps),
                    MediaKind::Photo,
                ));
            }
        }
        for (hash, gps) in [
            ("video-a", Some((47.5, 19.0))),
            ("video-b", Some((41.0, 29.0))),
            ("video-unknown", None),
        ] {
            rows.push(trip_row(
                hash,
                Some("2020-03-12T10:00:00"),
                gps,
                MediaKind::Video,
            ));
        }
        let events = detect(&rows).events;
        assert_eq!(events.len(), 2);
        for (anchor, own_video, other_video) in
            [("a00", "video-a", "video-b"), ("b00", "video-b", "video-a")]
        {
            let trip = events
                .iter()
                .find(|trip| trip.members.contains(&anchor.to_owned()))
                .unwrap();
            assert!(trip.members.contains(&own_video.to_owned()));
            assert!(!trip.members.contains(&other_video.to_owned()));
            assert!(!trip.members.contains(&"video-unknown".to_owned()));
        }
    }

    #[test]
    fn equidistant_edge_file_is_not_claimed_by_two_trips() {
        let mut rows: Vec<_> = (0..30)
            .map(|i| {
                trip_row(
                    &format!("home-{i:02}"),
                    Some("2020-01-01T08:00:00"),
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                )
            })
            .collect();
        for (prefix, gps, hours) in [
            ("a", (47.5, 19.0), [10, 11, 12]),
            ("b", (41.0, 29.0), [14, 15, 16]),
        ] {
            for (j, hour) in hours.iter().enumerate() {
                rows.push(trip_row(
                    &format!("{prefix}-anchor-{j}"),
                    Some(&format!("2020-03-12T{hour:02}:00:00")),
                    Some(gps),
                    MediaKind::Photo,
                ));
            }
            for i in 0..7 {
                rows.push(trip_row(
                    &format!("{prefix}-plain-{i}"),
                    Some(&format!("2020-03-12T{:02}:{i:02}:00", hours[0])),
                    None,
                    MediaKind::Photo,
                ));
            }
        }
        rows.push(trip_row(
            "middle",
            Some("2020-03-12T13:00:00"),
            None,
            MediaKind::Photo,
        ));
        let result = detect(&rows);
        assert_eq!(result.events.len(), 2);
        assert!(result
            .events
            .iter()
            .all(|t| !t.members.contains(&"middle".to_owned())));
        assert_eq!(
            result
                .events
                .iter()
                .flat_map(|t| &t.members)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            result.events.iter().map(|t| t.members.len()).sum::<usize>()
        );
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

    fn synthetic_library(n: usize) -> Vec<TripRow> {
        let home = n * 2 / 7;
        let away = n / 14;
        let trip_unlocated = n / 7;
        let jan = chrono::NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(8, 0, 0)
            .unwrap();
        let mar = chrono::NaiveDate::from_ymd_opt(2020, 3, 12)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let apr = chrono::NaiveDate::from_ymd_opt(2020, 4, 1)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        (0..n)
            .map(|i| {
                let at_home = i < home;
                let video = i % 10 == 0;
                let gps = if at_home {
                    Some((52.52, 13.405))
                } else if i < home + away {
                    Some((47.4979, 19.0402))
                } else {
                    None
                };
                let capture = if at_home {
                    jan
                } else if i < home + away + trip_unlocated {
                    mar
                } else {
                    apr
                } + chrono::Duration::seconds((i % 7200) as i64);
                TripRow {
                    hash: format!("{i:064x}"),
                    capture: Some(capture),
                    gps,
                    media: if video {
                        MediaKind::Video
                    } else {
                        MediaKind::Photo
                    },
                    path: format!("/synthetic/{i}.{}", if video { "mp4" } else { "jpg" }),
                    ext: if video { "mp4" } else { "jpg" }.into(),
                    width: Some(4000),
                    height: Some(3000),
                }
            })
            .collect()
    }

    fn measure(n: usize) {
        let rows = synthetic_library(n);
        let start = std::time::Instant::now();
        let result = detect(&rows);
        eprintln!(
            "events rows={} elapsed_ms={}",
            rows.len(),
            start.elapsed().as_millis()
        );
        assert!(!result.events.is_empty());
        for trip in &result.events {
            assert_eq!(
                trip.members
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                trip.members.len()
            );
        }
    }

    #[test]
    #[ignore = "manual event measurement"]
    fn event_measurement_20k() {
        measure(20_000);
    }

    #[test]
    #[ignore = "manual event measurement"]
    fn event_measurement_70k() {
        measure(70_000);
    }
}
