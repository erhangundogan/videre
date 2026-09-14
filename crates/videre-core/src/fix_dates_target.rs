//! The fix-dates target transform, shared by [`videre fix-dates`](the command
//! that applies it) and `videre status` (which reports how many rows would
//! change without applying anything).
//!
//! One definition of "would fix-dates change this row?": a row is outstanding
//! when its stored `modified_at` differs from
//! [`target_modified_at`] of its `exif_date`.

use chrono::TimeZone;

/// Given a stored `exif_date` ("YYYY-MM-DDTHH:MM:SS", camera-local, no
/// timezone), return the rfc3339 timestamp fix-dates would write to
/// `modified_at`, or `None` when it cannot be parsed or resolves to an
/// ambiguous local time.
pub fn target_modified_at(exif_date: &str) -> Option<String> {
    let ndt = chrono::NaiveDateTime::parse_from_str(exif_date, "%Y-%m-%dT%H:%M:%S").ok()?;
    let local_dt = chrono::Local.from_local_datetime(&ndt).single()?;
    Some(local_dt.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_is_none_for_garbage_and_some_for_a_valid_exif_date() {
        assert_eq!(target_modified_at("not-a-date"), None);
        assert_eq!(target_modified_at(""), None);
        let t = target_modified_at("2021-07-04T15:30:00");
        assert!(t.is_some() && t.unwrap().starts_with("2021-07-04T15:30:00"));
    }

    #[test]
    fn target_is_none_for_a_wrong_shape_that_would_otherwise_parse() {
        // fix-dates parses exactly one shape; a full rfc3339 exif_date is not
        // it and must read as "nothing to compute" rather than a parse error.
        assert_eq!(target_modified_at("2021-07-04T15:30:00+02:00"), None);
    }
}
