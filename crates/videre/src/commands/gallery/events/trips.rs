use super::place_groups::{eligible_destination, Places};
use super::{MediaKind, TripRow};
use std::collections::BTreeSet;
use videre_core::location_cluster::haversine_km;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Episode {
    pub anchors: Vec<usize>,
    pub stop_groups: BTreeSet<usize>,
}

pub(super) fn destination_episodes(rows: &[TripRow], places: &Places) -> Vec<Episode> {
    let mut photos: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| {
            (row.media == MediaKind::Photo && row.capture.is_some() && places.by_row[i].is_some())
                .then_some(i)
        })
        .collect();
    photos.sort_by(|&a, &b| {
        rows[a]
            .capture
            .cmp(&rows[b].capture)
            .then_with(|| rows[a].hash.cmp(&rows[b].hash))
    });

    let mut episodes = Vec::new();
    let mut current: Option<Episode> = None;
    for i in photos {
        let group_id = places.by_row[i].unwrap();
        if !eligible_destination(group_id, places) {
            if let Some(episode) = current.take() {
                episodes.push(episode);
            }
            continue;
        }
        let split = current.as_ref().is_some_and(|episode| {
            let prior = *episode.anchors.last().unwrap();
            let before = places.groups[places.by_row[prior].unwrap()].center;
            let after = places.groups[group_id].center;
            rows[i].capture.unwrap() - rows[prior].capture.unwrap() > chrono::Duration::hours(72)
                || haversine_km(before.0, before.1, after.0, after.1) > 40.0
        });
        if split {
            episodes.push(current.take().unwrap());
        }
        let episode = current.get_or_insert_with(|| Episode {
            anchors: Vec::new(),
            stop_groups: BTreeSet::new(),
        });
        episode.anchors.push(i);
        episode.stop_groups.insert(group_id);
    }
    if let Some(episode) = current {
        episodes.push(episode);
    }
    episodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::gallery::events::place_groups::infer_places;
    use crate::commands::gallery::events::{parse_capture, MediaKind};

    fn row(hash: &str, when: &str, gps: Option<(f64, f64)>, media: MediaKind) -> TripRow {
        TripRow {
            hash: hash.into(),
            capture: parse_capture(when),
            gps,
            media,
            path: format!("/p/{hash}.jpg"),
            ext: "jpg".into(),
            width: None,
            height: None,
        }
    }
    fn with_home(mut away: Vec<TripRow>) -> Vec<TripRow> {
        for i in 0..12 {
            away.push(row(
                &format!("home-{i}"),
                &format!("2020-01-01T08:{i:02}:00"),
                Some((52.52, 13.4)),
                MediaKind::Photo,
            ));
        }
        away
    }

    #[test]
    fn nearby_stops_across_four_days_form_one_episode() {
        let rows = with_home(
            (0..8)
                .map(|i| {
                    row(
                        &format!("away-{i}"),
                        &format!("2020-03-{:02}T10:00:00", 12 + i / 2),
                        Some(if i < 4 { (47.5, 19.0) } else { (47.5, 19.35) }),
                        MediaKind::Photo,
                    )
                })
                .collect(),
        );
        let places = infer_places(&rows);
        let episodes = destination_episodes(&rows, &places);
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].anchors.len(), 8);
        assert_eq!(episodes[0].stop_groups.len(), 2);
    }

    #[test]
    fn seventy_two_hours_continues_but_one_second_more_splits() {
        let rows = with_home(vec![
            row(
                "a",
                "2020-03-12T10:00:00",
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ),
            row(
                "b",
                "2020-03-15T10:00:00",
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ),
            row(
                "c",
                "2020-03-18T10:00:01",
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ),
        ]);
        let places = infer_places(&rows);
        let episodes = destination_episodes(&rows, &places);
        assert_eq!(
            episodes.iter().map(|e| e.anchors.len()).collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[test]
    fn home_photo_and_distant_jump_split_but_video_cannot_bridge() {
        let mut rows = with_home(vec![
            row(
                "a",
                "2020-03-12T10:00:00",
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ),
            row(
                "home-return",
                "2020-03-12T11:00:00",
                Some((52.52, 13.4)),
                MediaKind::Photo,
            ),
            row(
                "b",
                "2020-03-12T12:00:00",
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ),
            row(
                "c",
                "2020-03-12T13:00:00",
                Some((46.0, 20.0)),
                MediaKind::Photo,
            ),
            row(
                "video",
                "2020-03-20T10:00:00",
                Some((46.0, 20.0)),
                MediaKind::Video,
            ),
            row(
                "d",
                "2020-03-20T11:00:00",
                Some((46.0, 20.0)),
                MediaKind::Photo,
            ),
        ]);
        let places = infer_places(&rows);
        let episodes = destination_episodes(&rows, &places);
        assert_eq!(
            episodes.iter().map(|e| e.anchors.len()).collect::<Vec<_>>(),
            vec![1, 1, 1, 1]
        );
        rows.retain(|r| r.media == MediaKind::Video || r.hash.starts_with("home"));
        let places = infer_places(&rows);
        assert!(destination_episodes(&rows, &places).is_empty());
    }
}
