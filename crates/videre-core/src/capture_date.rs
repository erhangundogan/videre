//! One capture date per file, and where it came from.
//!
//! Every date videre stores is a **local wall clock**, `YYYY-MM-DDTHH:MM:SS`,
//! the form EXIF writes: the time on the camera's clock where the photo was
//! taken, with no zone. A source that gives a UTC instant (a Takeout
//! sidecar, a video's `mvhd`, a file's mtime) becomes one through
//! [`wall_clock`], machine-local, which is the inverse of what fix-dates does
//! when it turns a wall clock back into an mtime. So a date that goes to the
//! mtime and back is unchanged, and two sources can never sit on two clocks
//! in one comparison.
//!
//! [`resolve`] picks one date per file by precedence; see [`DateSource`].

use chrono::{NaiveDateTime, TimeZone};

/// Where a file's capture date came from, best first: a source is used only
/// when every one above it is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSource {
    /// EXIF `DateTimeOriginal`, the camera's own wall clock.
    Exif,
    /// A video's `com.apple.quicktime.creationdate`, local time.
    Video,
    /// A Google Takeout sidecar's `photoTakenTime`.
    Sidecar,
    /// A video's `mvhd` creation time, a UTC instant with no zone.
    Mvhd,
    /// The file's modification time: what is left when nothing recorded the
    /// moment of capture.
    Mtime,
}

impl DateSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DateSource::Exif => "exif",
            DateSource::Video => "video",
            DateSource::Sidecar => "sidecar",
            DateSource::Mvhd => "mvhd",
            DateSource::Mtime => "mtime",
        }
    }

    pub fn parse(s: &str) -> Option<DateSource> {
        Some(match s {
            "exif" => DateSource::Exif,
            "video" => DateSource::Video,
            "sidecar" => DateSource::Sidecar,
            "mvhd" => DateSource::Mvhd,
            "mtime" => DateSource::Mtime,
            _ => return None,
        })
    }
}

const WALL: &str = "%Y-%m-%dT%H:%M:%S";

/// A stored or EXIF-derived wall clock (`YYYY-MM-DDTHH:MM:SS`), checked and
/// normalised, or `None` when it is not a date.
///
/// Hour 24 becomes 00 of the next day: some Android cameras write
/// `24:03:39` for three minutes past midnight, which a strict parse rejects
/// and a string comparison sorts after 23:59. An all-zero date (an unset
/// camera clock) is not a date.
pub fn normalize_exif(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.len() < 19 || raw.starts_with("0000") {
        return None;
    }
    let s = &raw[..19];
    if let Ok(t) = NaiveDateTime::parse_from_str(s, WALL) {
        return Some(t.format(WALL).to_string());
    }
    if &s[10..13] == "T24" {
        let at_midnight = format!("{}T00{}", &s[..10], &s[13..]);
        let t = NaiveDateTime::parse_from_str(&at_midnight, WALL).ok()?;
        return Some((t + chrono::Duration::days(1)).format(WALL).to_string());
    }
    None
}

/// The wall clock in `tz` at Unix second `unix`.
pub fn wall_clock_in<Tz: TimeZone>(unix: i64, tz: &Tz) -> Option<String> {
    let utc = chrono::DateTime::from_timestamp(unix, 0)?;
    Some(utc.with_timezone(tz).naive_local().format(WALL).to_string())
}

/// The machine-local wall clock at Unix second `unix`.
pub fn wall_clock(unix: i64) -> Option<String> {
    wall_clock_in(unix, &chrono::Local)
}

/// An rfc3339 timestamp (how `modified_at` is stored) as a wall clock in `tz`.
fn rfc3339_wall_clock_in<Tz: TimeZone>(rfc3339: &str, tz: &Tz) -> Option<String> {
    let t = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    wall_clock_in(t.timestamp(), tz)
}

/// Everything a file offers as a date. Each is optional; [`resolve`] takes
/// the best one present.
#[derive(Debug, Default, Clone, Copy)]
pub struct Inputs<'a> {
    /// EXIF `DateTimeOriginal`, as stored (`YYYY-MM-DDTHH:MM:SS`).
    pub exif: Option<&'a str>,
    /// A video's Apple creation date, already a wall clock.
    pub video_apple: Option<&'a str>,
    /// A Takeout sidecar's `photoTakenTime`, Unix seconds.
    pub sidecar_unix: Option<i64>,
    /// A video's `mvhd` creation time, Unix seconds.
    pub mvhd_unix: Option<i64>,
    /// The file's mtime, rfc3339 as stored in `modified_at`.
    pub mtime: Option<&'a str>,
}

/// The capture date and its source, converting instants in `tz`.
pub fn resolve_in<Tz: TimeZone>(i: &Inputs, tz: &Tz) -> Option<(String, DateSource)> {
    if let Some(d) = i.exif.and_then(normalize_exif) {
        return Some((d, DateSource::Exif));
    }
    if let Some(d) = i.video_apple.and_then(normalize_exif) {
        return Some((d, DateSource::Video));
    }
    if let Some(d) = i.sidecar_unix.and_then(|u| wall_clock_in(u, tz)) {
        return Some((d, DateSource::Sidecar));
    }
    if let Some(d) = i.mvhd_unix.and_then(|u| wall_clock_in(u, tz)) {
        return Some((d, DateSource::Mvhd));
    }
    if let Some(d) = i.mtime.and_then(|m| rfc3339_wall_clock_in(m, tz)) {
        return Some((d, DateSource::Mtime));
    }
    None
}

/// The capture date and its source, machine-local.
pub fn resolve(i: &Inputs) -> Option<(String, DateSource)> {
    resolve_in(i, &chrono::Local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn plus2() -> FixedOffset {
        FixedOffset::east_opt(2 * 3600).unwrap()
    }

    #[test]
    fn hour_24_is_midnight_of_the_next_day() {
        assert_eq!(
            normalize_exif("2015-10-29T24:03:39").as_deref(),
            Some("2015-10-30T00:03:39")
        );
        assert_eq!(
            normalize_exif("2015-12-31T24:00:00").as_deref(),
            Some("2016-01-01T00:00:00")
        );
    }

    #[test]
    fn a_valid_date_is_kept_and_garbage_is_not_a_date() {
        assert_eq!(
            normalize_exif("2019-06-15T10:00:00").as_deref(),
            Some("2019-06-15T10:00:00")
        );
        for bad in ["0000-00-00T00:00:00", "2019-13-40T10:00:00", "dün", ""] {
            assert_eq!(normalize_exif(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_instant_becomes_the_wall_clock_of_the_zone() {
        // IMG-20160104-WA0000.jpg's photoTakenTime: 4 Jan 2016, 20:29:58 UTC.
        assert_eq!(
            wall_clock_in(1_451_939_398, &plus2()).as_deref(),
            Some("2016-01-04T22:29:58")
        );
    }

    /// An undated file written at 23:30 UTC was taken at 01:30 the next day
    /// in UTC+2; comparing the UTC string put it on the previous day.
    #[test]
    fn a_file_time_lands_on_its_local_day() {
        let i = Inputs {
            mtime: Some("2016-01-04T23:30:00+00:00"),
            ..Default::default()
        };
        assert_eq!(
            resolve_in(&i, &plus2()),
            Some(("2016-01-05T01:30:00".to_string(), DateSource::Mtime))
        );
    }

    #[test]
    fn each_source_wins_only_when_the_ones_above_are_absent() {
        let all = Inputs {
            exif: Some("2015-01-02T14:35:30"),
            video_apple: Some("2015-01-02T15:00:00"),
            sidecar_unix: Some(1_420_208_130),
            mvhd_unix: Some(1_420_200_000),
            mtime: Some("2026-09-18T13:30:04+00:00"),
        };
        let tz = plus2();
        let source = |i: &Inputs| resolve_in(i, &tz).map(|(_, s)| s);
        assert_eq!(source(&all), Some(DateSource::Exif));
        let i = Inputs { exif: None, ..all };
        assert_eq!(source(&i), Some(DateSource::Video));
        let i = Inputs {
            video_apple: None,
            ..i
        };
        assert_eq!(source(&i), Some(DateSource::Sidecar));
        let i = Inputs {
            sidecar_unix: None,
            ..i
        };
        assert_eq!(source(&i), Some(DateSource::Mvhd));
        let i = Inputs {
            mvhd_unix: None,
            ..i
        };
        assert_eq!(source(&i), Some(DateSource::Mtime));
        let i = Inputs { mtime: None, ..i };
        assert_eq!(source(&i), None);
    }

    /// A bad EXIF date does not block the sources below it.
    #[test]
    fn an_unusable_exif_date_falls_through() {
        let i = Inputs {
            exif: Some("0000-00-00T00:00:00"),
            sidecar_unix: Some(1_451_939_398),
            ..Default::default()
        };
        assert_eq!(
            resolve_in(&i, &plus2()),
            Some(("2016-01-04T22:29:58".to_string(), DateSource::Sidecar))
        );
    }

    #[test]
    fn sources_round_trip_through_their_names() {
        for s in [
            DateSource::Exif,
            DateSource::Video,
            DateSource::Sidecar,
            DateSource::Mvhd,
            DateSource::Mtime,
        ] {
            assert_eq!(DateSource::parse(s.as_str()), Some(s));
        }
    }
}
