use super::TripRow;
use chrono::{NaiveDate, NaiveDateTime};
use std::collections::{BTreeMap, BTreeSet};
use videre_core::location_cluster::haversine_km;

const MAX_RADIUS_KM: f64 = 20.0;

#[derive(Clone, Debug)]
pub(super) struct PlaceGroup {
    pub id: usize,
    pub center: (f64, f64),
    pub located_rows: Vec<usize>,
    pub observed_radius_km: f64,
}

#[derive(Clone, Debug)]
pub(super) struct Places {
    pub groups: Vec<PlaceGroup>,
    pub by_row: Vec<Option<usize>>,
    pub home: Option<usize>,
    #[cfg(test)]
    pub votes: Vec<usize>,
}

fn bucket(lat: f64, lon: f64) -> (i32, i32, i32) {
    let a = lat.to_radians();
    let o = lon.to_radians();
    let xyz = [
        6371.0 * a.cos() * o.cos(),
        6371.0 * a.cos() * o.sin(),
        6371.0 * a.sin(),
    ];
    (
        (xyz[0] / MAX_RADIUS_KM).floor() as i32,
        (xyz[1] / MAX_RADIUS_KM).floor() as i32,
        (xyz[2] / MAX_RADIUS_KM).floor() as i32,
    )
}

fn valid_gps(gps: Option<(f64, f64)>) -> Option<(f64, f64)> {
    gps.filter(|(lat, lon)| {
        lat.is_finite()
            && lon.is_finite()
            && (-90.0..=90.0).contains(lat)
            && (-180.0..=180.0).contains(lon)
    })
}

fn unambiguous_neighbor(
    timed: &BTreeMap<NaiveDateTime, BTreeSet<usize>>,
    when: NaiveDateTime,
) -> Option<usize> {
    let (before_time, before_groups) = timed.range(..=when).next_back()?;
    let (after_time, after_groups) = timed.range(when..).next()?;
    if when - *before_time > chrono::Duration::days(1)
        || *after_time - when > chrono::Duration::days(1)
        || before_groups.len() != 1
        || after_groups.len() != 1
    {
        return None;
    }
    let before = *before_groups.first()?;
    (after_groups.first() == Some(&before)).then_some(before)
}

pub(super) fn infer_places(rows: &[TripRow]) -> Places {
    let mut located: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, row)| valid_gps(row.gps).map(|_| i))
        .collect();
    located.sort_by(|&a, &b| {
        let ag = rows[a].gps.unwrap();
        let bg = rows[b].gps.unwrap();
        ag.0.total_cmp(&bg.0)
            .then_with(|| ag.1.total_cmp(&bg.1))
            .then_with(|| rows[a].hash.cmp(&rows[b].hash))
    });

    let mut groups: Vec<PlaceGroup> = Vec::new();
    let mut buckets: BTreeMap<(i32, i32, i32), Vec<usize>> = BTreeMap::new();
    let mut by_row = vec![None; rows.len()];
    for i in located {
        let gps = rows[i].gps.unwrap();
        let cell = bucket(gps.0, gps.1);
        let mut best: Option<(usize, f64)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(candidates) = buckets.get(&(cell.0 + dx, cell.1 + dy, cell.2 + dz))
                    {
                        for &id in candidates {
                            let seed = groups[id].center;
                            let km = haversine_km(seed.0, seed.1, gps.0, gps.1);
                            if km <= MAX_RADIUS_KM
                                && best.is_none_or(|(old, old_km)| {
                                    km.total_cmp(&old_km).is_lt()
                                        || (km.total_cmp(&old_km).is_eq()
                                            && seed_cmp(seed, groups[old].center).is_lt())
                                })
                            {
                                best = Some((id, km));
                            }
                        }
                    }
                }
            }
        }
        let id = if let Some((id, distance)) = best {
            let group = &mut groups[id];
            group.located_rows.push(i);
            group.observed_radius_km = group.observed_radius_km.max(distance);
            id
        } else {
            let id = groups.len();
            groups.push(PlaceGroup {
                id,
                center: gps,
                located_rows: vec![i],
                observed_radius_km: 0.0,
            });
            buckets.entry(cell).or_default().push(id);
            id
        };
        by_row[i] = Some(id);
    }

    let mut votes: Vec<usize> = groups.iter().map(|g| g.located_rows.len()).collect();
    let mut active_dates: Vec<BTreeSet<NaiveDate>> = vec![BTreeSet::new(); groups.len()];
    let mut timed = BTreeMap::<NaiveDateTime, BTreeSet<usize>>::new();
    for (i, row) in rows.iter().enumerate() {
        if let (Some(id), Some(when)) = (by_row[i], row.capture) {
            timed.entry(when).or_default().insert(id);
            active_dates[id].insert(when.date());
        }
    }
    for (i, row) in rows.iter().enumerate() {
        if by_row[i].is_none() {
            if let Some(id) = row
                .capture
                .and_then(|when| unambiguous_neighbor(&timed, when))
            {
                votes[id] += 1;
                active_dates[id].insert(row.capture.unwrap().date());
            }
        }
    }
    let home = (0..groups.len()).max_by(|&a, &b| {
        votes[a]
            .cmp(&votes[b])
            .then_with(|| {
                groups[a]
                    .located_rows
                    .len()
                    .cmp(&groups[b].located_rows.len())
            })
            .then_with(|| active_dates[a].len().cmp(&active_dates[b].len()))
            .then_with(|| seed_cmp(groups[b].center, groups[a].center))
    });
    Places {
        groups,
        by_row,
        home,
        #[cfg(test)]
        votes,
    }
}

fn seed_cmp(a: (f64, f64), b: (f64, f64)) -> std::cmp::Ordering {
    a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1))
}

pub(super) fn eligible_destination(id: usize, places: &Places) -> bool {
    let Some(home) = places.home else {
        return false;
    };
    if id == home {
        return false;
    }
    let group = &places.groups[id];
    debug_assert_eq!(group.id, id);
    let home = &places.groups[home];
    haversine_km(home.center.0, home.center.1, group.center.0, group.center.1)
        > home.observed_radius_km + group.observed_radius_km
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::gallery::events::{parse_capture, MediaKind};

    fn row(hash: &str, when: Option<&str>, gps: Option<(f64, f64)>) -> TripRow {
        TripRow {
            hash: hash.into(),
            capture: when.and_then(parse_capture),
            gps,
            media: MediaKind::Photo,
            path: format!("/p/{hash}.jpg"),
            ext: "jpg".into(),
            width: None,
            height: None,
        }
    }

    #[test]
    fn home_counts_undated_located_and_only_bracketed_unlocated() {
        let mut rows = vec![
            row("b1", Some("2020-01-01T10:00:00"), Some((52.52, 13.4))),
            row("b2", None, Some((52.52, 13.4))),
            row("b3", Some("2020-01-01T12:00:00"), Some((52.52, 13.4))),
            row("bracketed", Some("2020-01-01T11:00:00"), None),
            row("away", Some("2020-02-01T10:00:00"), Some((47.5, 19.0))),
            row("unbracketed", Some("2020-02-01T11:00:00"), None),
        ];
        let places = infer_places(&rows);
        let home = places.home.unwrap();
        assert_eq!(places.groups[home].located_rows.len(), 3);
        assert_eq!(places.votes[home], 4);
        rows.reverse();
        let again = infer_places(&rows);
        assert_eq!(
            again.groups[again.home.unwrap()].center,
            places.groups[home].center
        );
    }

    #[test]
    fn same_second_conflict_and_long_bracket_do_not_vote() {
        let rows = vec![
            row("berlin1", Some("2020-01-01T10:00:00"), Some((52.52, 13.4))),
            row("budapest1", Some("2020-01-01T10:00:00"), Some((47.5, 19.0))),
            row("ambiguous", Some("2020-01-01T10:00:00"), None),
            row("berlin2", Some("2020-01-04T10:00:00"), Some((52.52, 13.4))),
            row("too-long", Some("2020-01-03T10:00:00"), None),
        ];
        let places = infer_places(&rows);
        assert_eq!(places.votes.iter().sum::<usize>(), 3);
    }

    #[test]
    fn home_ties_choose_direct_count_then_active_dates_then_center() {
        let direct = vec![
            row("a1", Some("2020-01-01T10:00:00"), Some((40.0, 0.0))),
            row("a2", Some("2020-01-01T12:00:00"), Some((40.0, 0.0))),
            row("b1", Some("2020-02-01T10:00:00"), Some((50.0, 0.0))),
            row("b2", Some("2020-02-01T10:00:00"), None),
        ];
        let places = infer_places(&direct);
        assert_eq!(places.groups[places.home.unwrap()].center, (40.0, 0.0));

        let dates = vec![
            row("a1", Some("2020-01-01T10:00:00"), Some((40.0, 0.0))),
            row("a2", Some("2020-01-02T10:00:00"), Some((40.0, 0.0))),
            row("b1", Some("2020-02-01T10:00:00"), Some((50.0, 0.0))),
            row("b2", Some("2020-02-01T11:00:00"), Some((50.0, 0.0))),
        ];
        let places = infer_places(&dates);
        assert_eq!(places.groups[places.home.unwrap()].center, (40.0, 0.0));

        let center = vec![
            row("a", None, Some((40.0, 0.0))),
            row("b", None, Some((50.0, 0.0))),
        ];
        let places = infer_places(&center);
        assert_eq!(places.groups[places.home.unwrap()].center, (40.0, 0.0));
    }

    #[test]
    fn fixed_seed_caps_members_and_crosses_earth_seams() {
        let rows = vec![
            row("chain-a", None, Some((0.0, 0.0))),
            row("chain-b", None, Some((0.0, 0.18))),
            row("chain-c", None, Some((0.0, 0.36))),
            row("date-line-a", None, Some((10.0, 179.99))),
            row("date-line-b", None, Some((10.0, -179.99))),
            row("pole-a", None, Some((89.99, 0.0))),
            row("pole-b", None, Some((89.99, 180.0))),
        ];
        let places = infer_places(&rows);
        assert_ne!(places.by_row[0], places.by_row[2]);
        assert_eq!(places.by_row[3], places.by_row[4]);
        assert_eq!(places.by_row[5], places.by_row[6]);
        assert!(places
            .groups
            .iter()
            .all(|group| group.located_rows.iter().all(|&i| {
                let gps = rows[i].gps.unwrap();
                haversine_km(group.center.0, group.center.1, gps.0, gps.1) <= 20.0
            })));
    }

    #[test]
    fn observed_footprint_vetoes_overlapping_home_split() {
        let rows = vec![
            row("h1", None, Some((52.52, 13.4))),
            row("h2", None, Some((52.52, 13.65))),
            row("h3", None, Some((52.52, 13.4))),
            row("near1", None, Some((52.52, 13.75))),
            row("near2", None, Some((52.52, 13.95))),
            row("far", None, Some((47.5, 19.0))),
        ];
        let places = infer_places(&rows);
        let near = places.by_row[3].unwrap();
        assert_ne!(near, places.home.unwrap());
        assert!(!eligible_destination(near, &places));
        assert!(eligible_destination(places.by_row[5].unwrap(), &places));
    }
}
