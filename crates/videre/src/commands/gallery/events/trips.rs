use super::place_groups::{eligible_destination, Places};
use super::{MediaKind, Trip, TripRow};
use chrono::NaiveDateTime;
use std::collections::{BTreeMap, BTreeSet};
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
    let mut pos = 0;
    while pos < photos.len() {
        let when = rows[photos[pos]].capture.unwrap();
        let mut end = pos + 1;
        while end < photos.len() && rows[photos[end]].capture == Some(when) {
            end += 1;
        }
        // A home (or overlapping-home) photo at this exact wall-clock time
        // contradicts every away photo at that time. Resolve the whole second
        // before looking at hashes, so hash spelling cannot pick which side
        // of the home boundary an away anchor belongs to.
        if photos[pos..end]
            .iter()
            .any(|&i| !eligible_destination(places.by_row[i].unwrap(), places))
        {
            if let Some(episode) = current.take() {
                episodes.push(episode);
            }
            pos = end;
            continue;
        }
        for &i in &photos[pos..end] {
            let group_id = places.by_row[i].unwrap();
            let split = current.as_ref().is_some_and(|episode| {
                let prior = *episode.anchors.last().unwrap();
                let before = places.groups[places.by_row[prior].unwrap()].center;
                let after = places.groups[group_id].center;
                rows[i].capture.unwrap() - rows[prior].capture.unwrap()
                    > chrono::Duration::hours(72)
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
        pos = end;
    }
    if let Some(episode) = current {
        episodes.push(episode);
    }
    episodes
}

fn nearest_episode(
    when: NaiveDateTime,
    timed_anchors: &BTreeMap<NaiveDateTime, BTreeSet<usize>>,
) -> Option<usize> {
    let before = timed_anchors.range(..=when).next_back();
    let after = timed_anchors.range(when..).next();
    let selected = match (before, after) {
        (Some((bt, bg)), Some((at, ag))) => {
            let left = (when - *bt).num_milliseconds();
            let right = (*at - when).num_milliseconds();
            if left < right {
                bg
            } else if right < left {
                ag
            } else if bg == ag {
                bg
            } else {
                return None;
            }
        }
        (Some((_, bg)), None) => bg,
        (None, Some((_, ag))) => ag,
        (None, None) => return None,
    };
    (selected.len() == 1).then(|| *selected.first().unwrap())
}

fn single_stop(groups: &BTreeSet<usize>, episode: &Episode) -> bool {
    groups.len() == 1 && groups.iter().all(|id| episode.stop_groups.contains(id))
}

fn gpsless_agrees(
    when: NaiveDateTime,
    start: NaiveDateTime,
    end: NaiveDateTime,
    episode: &Episode,
    located: &BTreeMap<NaiveDateTime, BTreeSet<usize>>,
) -> bool {
    let before = located.range(..=when).next_back();
    let after = located.range(when..).next();
    if when < start {
        after.is_some_and(|(_, groups)| single_stop(groups, episode))
            && before.is_none_or(|(time, groups)| {
                *time < start - chrono::Duration::hours(3) || single_stop(groups, episode)
            })
    } else if when > end {
        before.is_some_and(|(_, groups)| single_stop(groups, episode))
            && after.is_none_or(|(time, groups)| {
                *time > end + chrono::Duration::hours(3) || single_stop(groups, episode)
            })
    } else {
        before.is_some_and(|(_, groups)| single_stop(groups, episode))
            && after.is_some_and(|(_, groups)| single_stop(groups, episode))
    }
}

fn inside_bounds(when: NaiveDateTime, start: NaiveDateTime, end: NaiveDateTime) -> bool {
    if when < start {
        when.date() == start.date() && start - when <= chrono::Duration::hours(3)
    } else if when > end {
        when.date() == end.date() && when - end <= chrono::Duration::hours(3)
    } else {
        true
    }
}

fn rolling_three_hours_has(rows: &[TripRow], members: &[usize], anchors: &BTreeSet<usize>) -> bool {
    let mut left = 0;
    let mut anchor_count = 0;
    for right in 0..members.len() {
        if anchors.contains(&members[right]) {
            anchor_count += 1;
        }
        let end = rows[members[right]].capture.unwrap();
        while left <= right {
            let start = rows[members[left]].capture.unwrap();
            if start.date() == end.date() && end - start <= chrono::Duration::hours(3) {
                break;
            }
            if anchors.contains(&members[left]) {
                anchor_count -= 1;
            }
            left += 1;
        }
        if right + 1 - left >= 10 && anchor_count >= 3 {
            return true;
        }
    }
    false
}

fn make_trip(rows: &[TripRow], places: &Places, anchors: &[usize], members: &[usize]) -> Trip {
    let first = anchors[0];
    let mut stop_counts = BTreeMap::<usize, (usize, usize)>::new();
    let anchor_set: BTreeSet<usize> = anchors.iter().copied().collect();
    for &i in members {
        if let Some(group) = places.by_row[i] {
            let counts = stop_counts.entry(group).or_default();
            counts.0 += 1;
            if anchor_set.contains(&i) {
                counts.1 += 1;
            }
        }
    }
    let dominant = stop_counts
        .into_iter()
        .max_by(|(aid, ac), (bid, bc)| {
            ac.cmp(bc).then_with(|| {
                let a = places.groups[*aid].center;
                let b = places.groups[*bid].center;
                b.0.total_cmp(&a.0).then_with(|| b.1.total_cmp(&a.1))
            })
        })
        .map(|(id, _)| places.groups[id].center)
        .unwrap_or(rows[first].gps.unwrap());
    Trip {
        start: rows[*members.first().unwrap()].capture.unwrap(),
        end: rows[*members.last().unwrap()].capture.unwrap(),
        sample: rows[first].clone(),
        members: members.iter().map(|&i| rows[i].hash.clone()).collect(),
        anchor_when: rows[first].capture.unwrap(),
        anchor_hash: rows[first].hash.clone(),
        dominant_stop: dominant,
    }
}

pub(super) fn qualify_trips(rows: &[TripRow], places: &Places, episodes: &[Episode]) -> Vec<Trip> {
    if episodes.is_empty() {
        return Vec::new();
    }
    let mut owner = vec![None; rows.len()];
    let mut timed_anchors = BTreeMap::<NaiveDateTime, BTreeSet<usize>>::new();
    for (eid, episode) in episodes.iter().enumerate() {
        for &i in &episode.anchors {
            owner[i] = Some(eid);
            timed_anchors
                .entry(rows[i].capture.unwrap())
                .or_default()
                .insert(eid);
        }
    }
    let mut located = BTreeMap::<NaiveDateTime, BTreeSet<usize>>::new();
    for (i, row) in rows.iter().enumerate() {
        if let (Some(when), Some(group)) = (row.capture, places.by_row[i]) {
            located.entry(when).or_default().insert(group);
        }
    }
    let bounds: Vec<_> = episodes
        .iter()
        .map(|episode| {
            (
                rows[episode.anchors[0]].capture.unwrap(),
                rows[*episode.anchors.last().unwrap()].capture.unwrap(),
            )
        })
        .collect();
    let mut assigned = vec![Vec::<usize>::new(); episodes.len()];
    for (i, row) in rows.iter().enumerate() {
        let Some(when) = row.capture else { continue };
        if let Some(eid) = owner[i] {
            assigned[eid].push(i);
            continue;
        }
        if row.media == MediaKind::Photo && places.by_row[i].is_some() {
            continue;
        }
        let Some(eid) = nearest_episode(when, &timed_anchors) else {
            continue;
        };
        let episode = &episodes[eid];
        let (start, end) = bounds[eid];
        if !inside_bounds(when, start, end) {
            continue;
        }
        let agrees = match places.by_row[i] {
            Some(group) => row.media == MediaKind::Video && episode.stop_groups.contains(&group),
            None => gpsless_agrees(when, start, end, episode, &located),
        };
        if agrees {
            assigned[eid].push(i);
        }
    }
    let mut trips = Vec::new();
    for (eid, episode) in episodes.iter().enumerate() {
        let members = &mut assigned[eid];
        members.sort_by(|&a, &b| {
            rows[a]
                .capture
                .cmp(&rows[b].capture)
                .then_with(|| rows[a].hash.cmp(&rows[b].hash))
        });
        let anchors: BTreeSet<usize> = episode.anchors.iter().copied().collect();
        let mut dates = BTreeMap::<_, usize>::new();
        for &i in &episode.anchors {
            *dates.entry(rows[i].capture.unwrap().date()).or_default() += 1;
        }
        let multidate = dates.len() > 1;
        if multidate
            && dates.values().filter(|&&count| count >= 2).count() >= 2
            && members.len() >= 10
        {
            trips.push(make_trip(rows, places, &episode.anchors, members));
        } else if multidate {
            for date in dates.keys() {
                let day_anchors: Vec<_> = episode
                    .anchors
                    .iter()
                    .copied()
                    .filter(|&i| rows[i].capture.unwrap().date() == *date)
                    .collect();
                let day_first = rows[day_anchors[0]].capture.unwrap();
                let day_last = rows[*day_anchors.last().unwrap()].capture.unwrap();
                let day_members: Vec<_> = members
                    .iter()
                    .copied()
                    .filter(|&i| {
                        let when = rows[i].capture.unwrap();
                        when.date() == *date && inside_bounds(when, day_first, day_last)
                    })
                    .collect();
                if rolling_three_hours_has(rows, &day_members, &anchors) {
                    trips.push(make_trip(rows, places, &day_anchors, &day_members));
                }
            }
        } else if rolling_three_hours_has(rows, members, &anchors) {
            trips.push(make_trip(rows, places, &episode.anchors, members));
        }
    }
    trips.sort_by(|a, b| {
        a.anchor_when
            .cmp(&b.anchor_when)
            .then_with(|| a.anchor_hash.cmp(&b.anchor_hash))
    });
    trips
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

    #[test]
    fn simultaneous_home_photo_vetoes_destination_anchor_regardless_of_hash_order() {
        fn anchor_sets(home_hash: &str) -> Vec<Vec<String>> {
            let rows = with_home(vec![
                row(
                    "away-10",
                    "2020-03-12T10:00:00",
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ),
                row(
                    "middle",
                    "2020-03-12T11:00:00",
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ),
                row(
                    home_hash,
                    "2020-03-12T11:00:00",
                    Some((52.52, 13.4)),
                    MediaKind::Photo,
                ),
                row(
                    "away-12",
                    "2020-03-12T12:00:00",
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ),
                row(
                    "away-13",
                    "2020-03-12T13:00:00",
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ),
            ]);
            let places = infer_places(&rows);
            destination_episodes(&rows, &places)
                .iter()
                .map(|episode| {
                    episode
                        .anchors
                        .iter()
                        .map(|&i| rows[i].hash.clone())
                        .collect()
                })
                .collect()
        }
        let expected = vec![
            vec!["away-10".to_owned()],
            vec!["away-12".to_owned(), "away-13".to_owned()],
        ];
        assert_eq!(anchor_sets("a-home"), expected);
        assert_eq!(anchor_sets("z-home"), expected);
    }
}
