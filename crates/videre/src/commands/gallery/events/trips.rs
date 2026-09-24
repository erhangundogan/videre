use super::place_groups::{eligible_destination, Places};
use super::{EventsConfig, MediaKind, Trip, TripRow};
use chrono::{Duration, NaiveDateTime};
use std::collections::{BTreeMap, BTreeSet};

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
        let mut at_time = BTreeMap::<usize, Vec<usize>>::new();
        for &i in &photos[pos..end] {
            at_time
                .entry(places.by_row[i].unwrap())
                .or_default()
                .push(i);
        }
        // A home photo at this second contradicts away anchors at the same
        // time. Resolve it before hashes can choose a side of that boundary.
        if at_time
            .keys()
            .any(|&group| !eligible_destination(group, places))
        {
            if let Some(episode) = current.take() {
                episodes.push(episode);
            }
            pos = end;
            continue;
        }
        // Multiple observed places at one timestamp have no known temporal
        // order, even when nearby. Isolate each as an episode; a dense burst
        // in one place stays together and costs only one group lookup per row.
        if at_time.len() > 1 {
            if let Some(episode) = current.take() {
                episodes.push(episode);
            }
            for (group, anchors) in at_time {
                episodes.push(Episode {
                    anchors,
                    stop_groups: BTreeSet::from([group]),
                });
            }
            pos = end;
            continue;
        }
        for &i in &photos[pos..end] {
            let group_id = places.by_row[i].unwrap();
            let split = current.as_ref().is_some_and(|episode| {
                let prior = *episode.anchors.last().unwrap();
                rows[i].capture.unwrap() - rows[prior].capture.unwrap()
                    > chrono::Duration::hours(72)
                    || places.by_row[prior] != Some(group_id)
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
    window: Duration,
) -> bool {
    let before = located.range(..=when).next_back();
    let after = located.range(when..).next();
    if when < start {
        after.is_some_and(|(_, groups)| single_stop(groups, episode))
            && before
                .is_none_or(|(time, groups)| *time < start - window || single_stop(groups, episode))
    } else if when > end {
        before.is_some_and(|(_, groups)| single_stop(groups, episode))
            && after
                .is_none_or(|(time, groups)| *time > end + window || single_stop(groups, episode))
    } else {
        before.is_some_and(|(_, groups)| single_stop(groups, episode))
            && after.is_some_and(|(_, groups)| single_stop(groups, episode))
    }
}

fn inside_bounds(
    when: NaiveDateTime,
    start: NaiveDateTime,
    end: NaiveDateTime,
    window: Duration,
) -> bool {
    if when < start {
        start - when <= window
    } else if when > end {
        when - end <= window
    } else {
        true
    }
}

fn qualifying_window<'a>(
    rows: &[TripRow],
    members: &'a [usize],
    anchors: &BTreeSet<usize>,
    config: &EventsConfig,
    must_cross_midnight: bool,
) -> Option<&'a [usize]> {
    let mut left = 0;
    let mut anchor_count = 0;
    for right in 0..members.len() {
        if anchors.contains(&members[right]) {
            anchor_count += 1;
        }
        let end = rows[members[right]].capture.unwrap();
        while left <= right {
            let start = rows[members[left]].capture.unwrap();
            if end - start <= config.time_window {
                break;
            }
            if anchors.contains(&members[left]) {
                anchor_count -= 1;
            }
            left += 1;
        }
        if right + 1 - left >= config.min_media_items
            && anchor_count >= 3
            && (!must_cross_midnight || rows[members[left]].capture.unwrap().date() != end.date())
        {
            return Some(&members[left..=right]);
        }
    }
    None
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

pub(super) fn qualify_trips(
    rows: &[TripRow],
    places: &Places,
    episodes: &[Episode],
    config: &EventsConfig,
) -> Vec<Trip> {
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
        let eid = nearest_episode(when, &timed_anchors).or_else(|| {
            // Distinct photo-anchored trips may share an exact timestamp.
            // A located video can join only if its place matches exactly one
            // of those episodes; a GPS-less item stays ambiguous.
            if row.media != MediaKind::Video {
                return None;
            }
            let group = places.by_row[i]?;
            let mut matching = timed_anchors
                .get(&when)?
                .iter()
                .copied()
                .filter(|&id| episodes[id].stop_groups.contains(&group));
            let selected = matching.next()?;
            matching.next().is_none().then_some(selected)
        });
        let Some(eid) = eid else {
            continue;
        };
        let episode = &episodes[eid];
        let (start, end) = bounds[eid];
        if !inside_bounds(when, start, end, config.time_window) {
            continue;
        }
        let agrees = match places.by_row[i] {
            Some(group) => row.media == MediaKind::Video && episode.stop_groups.contains(&group),
            None => gpsless_agrees(when, start, end, episode, &located, config.time_window),
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
            && members.len() >= config.min_media_items
        {
            trips.push(make_trip(rows, places, &episode.anchors, members));
        } else if multidate {
            let dates: Vec<_> = dates.keys().copied().collect();
            let mut anchors_by_day = Vec::with_capacity(dates.len());
            let mut members_by_day = Vec::with_capacity(dates.len());
            for date in &dates {
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
                        when.date() == *date
                            && inside_bounds(when, day_first, day_last, config.time_window)
                    })
                    .collect();
                anchors_by_day.push(day_anchors);
                members_by_day.push(day_members);
            }
            // A failed multi-day gate cannot be bypassed by chaining separate
            // midnight visits through a shared calendar date. Keep each short
            // cross-midnight visit to its qualifying window, with no reused row.
            let mut used = BTreeSet::new();
            for day in 0..dates.len() {
                if day + 1 < dates.len() {
                    let pair_members: Vec<_> = members_by_day[day]
                        .iter()
                        .chain(members_by_day[day + 1].iter())
                        .filter(|i| !used.contains(*i))
                        .copied()
                        .collect();
                    if let Some(window) =
                        qualifying_window(rows, &pair_members, &anchors, config, true)
                    {
                        let first = pair_members.iter().position(|&i| i == window[0]).unwrap();
                        let start = rows[window[0]].capture.unwrap();
                        let visit: Vec<_> = pair_members[first..]
                            .iter()
                            .copied()
                            .take_while(|&i| rows[i].capture.unwrap() - start <= config.time_window)
                            .collect();
                        let visit_anchors: Vec<_> = visit
                            .iter()
                            .copied()
                            .filter(|i| anchors.contains(i))
                            .collect();
                        trips.push(make_trip(rows, places, &visit_anchors, &visit));
                        used.extend(visit);
                    }
                }
                let day_members: Vec<_> = members_by_day[day]
                    .iter()
                    .filter(|i| !used.contains(*i))
                    .copied()
                    .collect();
                if qualifying_window(rows, &day_members, &anchors, config, false).is_some() {
                    let day_anchors: Vec<_> = anchors_by_day[day]
                        .iter()
                        .copied()
                        .filter(|i| !used.contains(i))
                        .collect();
                    trips.push(make_trip(rows, places, &day_anchors, &day_members));
                    used.extend(day_members);
                }
            }
        } else if qualifying_window(rows, members, &anchors, config, false).is_some() {
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
    fn stops_within_one_place_radius_across_four_days_form_one_episode() {
        let rows = with_home(
            (0..8)
                .map(|i| {
                    row(
                        &format!("away-{i}"),
                        &format!("2020-03-{:02}T10:00:00", 12 + i / 2),
                        Some(if i < 4 { (47.5, 19.0) } else { (47.5, 19.15) }),
                        MediaKind::Photo,
                    )
                })
                .collect(),
        );
        let places = infer_places(&rows, 20.0);
        let episodes = destination_episodes(&rows, &places);
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].anchors.len(), 8);
        assert_eq!(episodes[0].stop_groups.len(), 1);
    }

    #[test]
    fn a_second_place_beyond_twenty_km_starts_a_new_episode() {
        let rows = with_home(vec![
            row(
                "a-1",
                "2020-03-12T10:00:00",
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ),
            row(
                "a-2",
                "2020-03-12T11:00:00",
                Some((47.5, 19.0)),
                MediaKind::Photo,
            ),
            row(
                "b-1",
                "2020-03-12T12:00:00",
                Some((47.5, 19.32)),
                MediaKind::Photo,
            ),
            row(
                "b-2",
                "2020-03-12T13:00:00",
                Some((47.5, 19.32)),
                MediaKind::Photo,
            ),
        ]);
        let places = infer_places(&rows, 20.0);
        let episodes = destination_episodes(&rows, &places);
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0].anchors.len(), 2);
        assert_eq!(episodes[1].anchors.len(), 2);
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
        let places = infer_places(&rows, 20.0);
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
        let places = infer_places(&rows, 20.0);
        let episodes = destination_episodes(&rows, &places);
        assert_eq!(
            episodes.iter().map(|e| e.anchors.len()).collect::<Vec<_>>(),
            vec![1, 1, 1, 1]
        );
        rows.retain(|r| r.media == MediaKind::Video || r.hash.starts_with("home"));
        let places = infer_places(&rows, 20.0);
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
            let places = infer_places(&rows, 20.0);
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

    #[test]
    fn simultaneous_distant_destinations_form_separate_episodes_regardless_of_hash_order() {
        fn anchor_sets(a_hash: &str, b_hash: &str) -> Vec<Vec<String>> {
            let rows = with_home(vec![
                row(
                    "a-09",
                    "2020-03-12T09:00:00",
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ),
                row(
                    a_hash,
                    "2020-03-12T10:00:00",
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ),
                row(
                    b_hash,
                    "2020-03-12T10:00:00",
                    Some((48.2, 16.4)),
                    MediaKind::Photo,
                ),
                row(
                    "a-11",
                    "2020-03-12T11:00:00",
                    Some((47.5, 19.0)),
                    MediaKind::Photo,
                ),
            ]);
            let places = infer_places(&rows, 20.0);
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
        assert_eq!(
            anchor_sets("a-middle", "z-distant"),
            vec![
                vec!["a-09".to_owned()],
                vec!["a-middle".to_owned()],
                vec!["z-distant".to_owned()],
                vec!["a-11".to_owned()]
            ]
        );
        assert_eq!(
            anchor_sets("z-middle", "a-distant"),
            vec![
                vec!["a-09".to_owned()],
                vec!["z-middle".to_owned()],
                vec!["a-distant".to_owned()],
                vec!["a-11".to_owned()]
            ]
        );
    }

    #[test]
    fn simultaneous_nearby_stops_do_not_depend_on_hash_order() {
        fn anchor_sets(a_hash: &str, b_hash: &str) -> Vec<Vec<String>> {
            let rows = with_home(vec![
                row(
                    "c-09",
                    "2020-03-12T09:00:00",
                    Some((47.0, 19.0)),
                    MediaKind::Photo,
                ),
                row(
                    a_hash,
                    "2020-03-12T10:00:00",
                    Some((47.315, 19.0)),
                    MediaKind::Photo,
                ),
                row(
                    b_hash,
                    "2020-03-12T10:00:00",
                    Some((47.585, 19.0)),
                    MediaKind::Photo,
                ),
                row(
                    "c-11",
                    "2020-03-12T11:00:00",
                    Some((47.0, 19.0)),
                    MediaKind::Photo,
                ),
            ]);
            let places = infer_places(&rows, 20.0);
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
        assert_eq!(
            anchor_sets("a-middle", "z-middle"),
            vec![
                vec!["c-09".to_owned()],
                vec!["a-middle".to_owned()],
                vec!["z-middle".to_owned()],
                vec!["c-11".to_owned()]
            ]
        );
        assert_eq!(
            anchor_sets("z-middle", "a-middle"),
            vec![
                vec!["c-09".to_owned()],
                vec!["z-middle".to_owned()],
                vec!["a-middle".to_owned()],
                vec!["c-11".to_owned()]
            ]
        );
    }

    #[test]
    #[ignore = "manual dense same-second burst scaling measurement"]
    fn benchmark_dense_same_second_away_burst() {
        let rows = with_home(
            (0..70_000)
                .map(|i| {
                    row(
                        &format!("away-{i:05}"),
                        "2020-03-12T10:00:00",
                        Some((47.5, 19.0)),
                        MediaKind::Photo,
                    )
                })
                .collect(),
        );
        let mut places = infer_places(&rows, 20.0);
        let home_row = rows.iter().position(|r| r.hash == "home-0").unwrap();
        places.home = places.by_row[home_row];
        let started = std::time::Instant::now();
        let episodes = destination_episodes(&rows, &places);
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].anchors.len(), 70_000);
        eprintln!(
            "dense same-second episode detection: {:?}",
            started.elapsed()
        );
    }
}
