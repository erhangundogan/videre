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
/// `modified_at`, or `None` when it cannot be parsed.
pub fn target_modified_at(exif_date: &str) -> Option<String> {
    let ndt = chrono::NaiveDateTime::parse_from_str(exif_date, "%Y-%m-%dT%H:%M:%S").ok()?;
    Some(local_instant(ndt)?.to_rfc3339())
}

/// A wall-clock time as an instant in the machine's zone, including the two
/// hours a year a clock change makes awkward. The hour the clocks go back
/// happens twice: its first pass is taken. The hour the clocks go forward
/// never happens: a camera that showed it had not been changed yet, so it is
/// read as the clock after the jump, one hour later. Refusing both left 30
/// photos of a real library failing on every fix-dates run.
fn local_instant(ndt: chrono::NaiveDateTime) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::LocalResult;
    let t = match chrono::Local.from_local_datetime(&ndt) {
        LocalResult::Single(t) => t,
        LocalResult::Ambiguous(a, b) => a.min(b),
        LocalResult::None => chrono::Local
            .from_local_datetime(&(ndt + chrono::Duration::hours(1)))
            .earliest()?,
    };
    // The platform's zone data can answer a repeated hour with its second pass
    // alone (macOS does): an hour earlier showing the same clock is the first.
    let earlier = t - chrono::Duration::hours(1);
    Some(if earlier.naive_local() == ndt {
        earlier
    } else {
        t
    })
}

/// Whether a row's stored `modified_at` already equals what fix-dates would
/// write for its `exif_date`, compared as instants so an equal time written
/// with a different offset still matches. `None` when the `exif_date` cannot be
/// parsed, which fix-dates reports as an error.
///
/// The one test of "would fix-dates change this row?": `videre status` counts
/// the rows where this is `Some(false)`, and fix-dates writes only those, so
/// the count status suggests is the count fix-dates asks about.
pub fn is_current(exif_date: &str, modified_at: Option<&str>) -> Option<bool> {
    let target = chrono::DateTime::parse_from_rfc3339(&target_modified_at(exif_date)?).ok()?;
    let current = modified_at.and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
    Some(current == Some(target))
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
