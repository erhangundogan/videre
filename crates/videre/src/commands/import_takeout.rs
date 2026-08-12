//! Google Takeout: matching a media file to its JSON sidecar.
//!
//! The matcher is the feature. Takeout puts every capture date in a sidecar
//! and then truncates the sidecar's *whole filename* at around 46 to 51
//! characters, so `.supplemental-metadata.json` arrives as `.suppl.json`,
//! `.s.json`, and everything between. Naive matching silently misses a large
//! fraction of a library, and a library whose dates are silently wrong looks
//! exactly like one whose dates are right.
//!
//! Its own module rather than inlined in the command, so a Flickr or XMP
//! source can reuse the truncation and `(N)` handling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// JSON that Takeout writes for its own bookkeeping rather than for a photo.
/// Excluded by name before matching, so a prefix rule can never reach them.
const NOT_SIDECARS: &[&str] = &[
    "metadata.json",
    "print-subscriptions.json",
    "shared_album_comments.json",
    "user-generated-memory-titles.json",
];

const CANONICAL_SUFFIX: &str = ".supplemental-metadata.json";

/// Every candidate sidecar in **one** directory.
///
/// Per directory because Takeout always places a sidecar beside its media
/// file, so the whole search space for one file is its own folder.
pub(crate) struct SidecarIndex {
    by_name: HashMap<String, PathBuf>,
    names: Vec<String>,
}

impl SidecarIndex {
    pub(crate) fn for_dir(dir: &Path) -> Self {
        let mut by_name = HashMap::new();
        let mut names = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            // An unreadable directory yields an empty index rather than an
            // error: the files in it are then simply reported unmatched.
            return Self { by_name, names };
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().ends_with(".json") {
                continue;
            }
            if NOT_SIDECARS.contains(&name.as_str()) {
                continue;
            }
            names.push(name.clone());
            by_name.insert(name, entry.path());
        }
        Self { by_name, names }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    fn exact(&self, name: &str) -> Option<PathBuf> {
        self.by_name.get(name).cloned()
    }

    /// Every sidecar whose name starts with `prefix`, for truncation matching.
    fn starting_with(&self, prefix: &str) -> Vec<PathBuf> {
        self.names
            .iter()
            .filter(|n| n.starts_with(prefix))
            .filter_map(|n| self.by_name.get(n).cloned())
            .collect()
    }
}

/// One media file and the sidecar that belongs to it.
pub(crate) struct Matched {
    pub file: PathBuf,
    pub sidecar: PathBuf,
}

/// The result of matching a whole export.
#[derive(Default)]
pub(crate) struct Survey {
    pub matched: Vec<Matched>,
    pub unmatched: usize,
    /// Counted apart from `unmatched`: ambiguity means the rules missed a
    /// naming variant, and a run whose ambiguous count is not near zero should
    /// be inspected rather than trusted.
    pub ambiguous: usize,
    pub folders: usize,
}

/// Matches every file against the sidecars in its own directory.
///
/// One index per directory, built once and reused for every file in it, since
/// indexing is the only part that touches the filesystem.
pub(crate) fn survey(files: &[PathBuf]) -> Survey {
    let mut by_folder: HashMap<PathBuf, Vec<&PathBuf>> = HashMap::new();
    for f in files {
        by_folder
            .entry(f.parent().unwrap_or(Path::new(".")).to_path_buf())
            .or_default()
            .push(f);
    }

    let mut out = Survey {
        folders: by_folder.len(),
        ..Default::default()
    };
    for (dir, files) in by_folder {
        let index = SidecarIndex::for_dir(&dir);
        if index.is_empty() {
            // A folder with no sidecars at all (an album of shortcuts, or a
            // tree that is not a Takeout export) needs no per-file work.
            out.unmatched += files.len();
            continue;
        }
        for file in files {
            let name = file.file_name().unwrap_or_default().to_string_lossy();
            match match_sidecar_detailed(&index, &name) {
                SidecarMatch::Found(sidecar) => out.matched.push(Matched {
                    file: file.clone(),
                    sidecar,
                }),
                SidecarMatch::Ambiguous => out.ambiguous += 1,
                SidecarMatch::Missing => out.unmatched += 1,
            }
        }
    }
    out
}

/// What one sidecar says about its photo.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SidecarMeta {
    /// Unix seconds from `photoTakenTime`. `None` when the field is absent or
    /// unparseable, which leaves the file untouched.
    pub taken_unix: Option<i64>,
    /// Latitude and longitude, absent at exactly `0.0, 0.0`.
    pub gps: Option<(f64, f64)>,
}

/// Only the fields that are read. Everything else in a sidecar (title,
/// descriptions, people, view counts) is ignored rather than modelled.
#[derive(serde::Deserialize)]
struct RawSidecar {
    #[serde(rename = "photoTakenTime")]
    photo_taken_time: Option<RawStamp>,
    #[serde(rename = "geoData")]
    geo_data: Option<RawGeo>,
}

#[derive(serde::Deserialize)]
struct RawStamp {
    /// Unix seconds **as a string**, which is how Google writes it.
    timestamp: Option<String>,
}

#[derive(serde::Deserialize)]
struct RawGeo {
    latitude: Option<f64>,
    longitude: Option<f64>,
}

/// Reads a sidecar's JSON.
///
/// **`photoTakenTime`, never `creationTime`.** `creationTime` is when the file
/// was uploaded to Google Photos, which for a migrated library is years after
/// the photo was taken, so it is not read at all rather than used as a
/// fallback: an import that silently applies upload dates looks exactly like
/// one that worked.
pub(crate) fn parse_sidecar(json: &str) -> anyhow::Result<SidecarMeta> {
    let raw: RawSidecar = serde_json::from_str(json)?;
    let taken_unix = raw
        .photo_taken_time
        .and_then(|s| s.timestamp)
        .and_then(|t| t.trim().parse::<i64>().ok());
    let gps = match raw.geo_data {
        Some(g) => match (g.latitude, g.longitude) {
            // Exactly zero is Takeout's way of saying "no location". Taken
            // literally it is Null Island, and importing it would put a
            // spurious cluster there for every photo without GPS.
            (Some(lat), Some(lon)) if lat != 0.0 || lon != 0.0 => Some((lat, lon)),
            _ => None,
        },
        None => None,
    };
    Ok(SidecarMeta { taken_unix, gps })
}

/// Deliberately three-valued. Ambiguous is not the same as missing: it means
/// the rules found more than one plausible answer, which is a signal that the
/// export names something in a way videre does not yet handle, and the summary
/// counts it separately so that stays visible.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SidecarMatch {
    Found(PathBuf),
    Ambiguous,
    Missing,
}

/// The sidecar for `file_name`, or `None` when there is none **or when more
/// than one matches**. Guessing between two candidates would produce a wrong
/// date, which is worse than no date at all.
///
/// The plain two-valued contract, which is what the matching rules are
/// specified in terms of. Test-only because the command needs the three-valued
/// form to count ambiguity apart from absence.
#[cfg(test)]
pub(crate) fn match_sidecar(index: &SidecarIndex, file_name: &str) -> Option<PathBuf> {
    match match_sidecar_detailed(index, file_name) {
        SidecarMatch::Found(p) => Some(p),
        _ => None,
    }
}

/// As `match_sidecar`, but distinguishing ambiguity from absence.
pub(crate) fn match_sidecar_detailed(index: &SidecarIndex, file_name: &str) -> SidecarMatch {
    macro_rules! attempt {
        ($e:expr) => {
            match $e {
                SidecarMatch::Missing => {}
                hit => return hit,
            }
        };
    }

    attempt!(try_forms(index, file_name));

    // Google puts the duplicate counter on the media name before the
    // extension (`a(1).jpg`) and on the sidecar after it (`a.jpg(1).json`), so
    // the two never align textually and the counter needs its own rule. Tried
    // before the plain retry on the stripped base, or `a(1).jpg` would take
    // `a.jpg`'s sidecar.
    if let Some((base, n)) = split_counter(file_name) {
        attempt!(try_counter_forms(index, &base, &n));
        attempt!(try_forms(index, &base));
        if let Some(edited) = strip_edited(&base) {
            attempt!(try_forms(index, &edited));
        }
    }

    // An `-edited` render has no sidecar of its own; the original's applies.
    if let Some(edited) = strip_edited(file_name) {
        attempt!(try_forms(index, &edited));
    }

    SidecarMatch::Missing
}

/// The three name-shape rules, in order, for one candidate base name.
fn try_forms(index: &SidecarIndex, name: &str) -> SidecarMatch {
    if let Some(p) = index.exact(&format!("{name}{CANONICAL_SUFFIX}")) {
        return SidecarMatch::Found(p);
    }
    // A sidecar whose stem is exactly the file name: `a.jpg.json`.
    if let Some(p) = index.exact(&format!("{name}.json")) {
        return SidecarMatch::Found(p);
    }
    resolve(index.starting_with(&format!("{name}.")))
}

/// Counter-bearing sidecar names for a media file that carried `(n)`.
fn try_counter_forms(index: &SidecarIndex, base: &str, n: &str) -> SidecarMatch {
    if let Some(p) = index.exact(&format!("{base}({n}).json")) {
        return SidecarMatch::Found(p);
    }
    let mut hits = index.starting_with(&format!("{base}({n})"));
    // The truncated form keeps the counter last: `a.jpg.supplemental(1).json`.
    hits.extend(
        index
            .starting_with(&format!("{base}."))
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .map(|f| f.to_string_lossy().ends_with(&format!("({n}).json")))
                    .unwrap_or(false)
            }),
    );
    hits.sort();
    hits.dedup();
    resolve(hits)
}

fn resolve(mut hits: Vec<PathBuf>) -> SidecarMatch {
    hits.sort();
    hits.dedup();
    match hits.len() {
        0 => SidecarMatch::Missing,
        1 => SidecarMatch::Found(hits.remove(0)),
        _ => SidecarMatch::Ambiguous,
    }
}

/// `a(1).jpg` -> (`a.jpg`, `1`). Only a purely numeric counter counts, so a
/// photo genuinely named `Trip (Copenhagen).jpg` is left alone.
fn split_counter(file_name: &str) -> Option<(String, String)> {
    let (stem, ext) = split_extension(file_name);
    let stem = stem.strip_suffix(')')?;
    let (before, digits) = stem.rsplit_once('(')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((join_extension(before.trim_end(), ext), digits.to_string()))
}

/// `a-edited.jpg` -> `a.jpg`.
fn strip_edited(file_name: &str) -> Option<String> {
    let (stem, ext) = split_extension(file_name);
    let base = stem.strip_suffix("-edited")?;
    Some(join_extension(base, ext))
}

fn split_extension(file_name: &str) -> (&str, Option<&str>) {
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (file_name, None),
    }
}

fn join_extension(stem: &str, ext: Option<&str>) -> String {
    match ext {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    /// Builds a directory holding `files`, indexes it, and hands back both.
    fn indexed(files: &[&str]) -> (tempfile::TempDir, SidecarIndex) {
        let d = tempdir().unwrap();
        for f in files {
            std::fs::write(d.path().join(f), b"{}").unwrap();
        }
        let index = SidecarIndex::for_dir(d.path());
        (d, index)
    }

    fn name_of(p: Option<std::path::PathBuf>) -> Option<String> {
        p.map(|p| p.file_name().unwrap().to_string_lossy().to_string())
    }

    #[test]
    fn matches_the_canonical_sidecar_name() {
        let (_d, index) = indexed(&["a.jpg", "a.jpg.supplemental-metadata.json"]);
        assert_eq!(
            name_of(match_sidecar(&index, "a.jpg")).as_deref(),
            Some("a.jpg.supplemental-metadata.json")
        );
    }

    #[test]
    fn matches_a_truncated_suffix() {
        // Google truncates the whole sidecar filename at around 46 to 51
        // characters, so the suffix is routinely cut short.
        let (_d, index) = indexed(&["a.jpg", "a.jpg.suppl.json"]);
        assert_eq!(
            name_of(match_sidecar(&index, "a.jpg")).as_deref(),
            Some("a.jpg.suppl.json")
        );
    }

    #[test]
    fn matches_a_heavily_truncated_suffix() {
        let (_d, index) = indexed(&["a.jpg", "a.jpg.s.json"]);
        assert_eq!(
            name_of(match_sidecar(&index, "a.jpg")).as_deref(),
            Some("a.jpg.s.json")
        );
    }

    #[test]
    fn matches_a_counter_that_sits_on_the_opposite_side() {
        // Google puts the duplicate counter on the media name before the
        // extension, and on the sidecar name after it, so the two never align
        // textually and this needs its own rule.
        let (_d, index) = indexed(&["a(1).jpg", "a.jpg(1).json"]);
        assert_eq!(
            name_of(match_sidecar(&index, "a(1).jpg")).as_deref(),
            Some("a.jpg(1).json")
        );
    }

    #[test]
    fn an_edited_version_falls_back_to_the_originals_sidecar() {
        let (_d, index) = indexed(&["a-edited.jpg", "a.jpg.supplemental-metadata.json"]);
        assert_eq!(
            name_of(match_sidecar(&index, "a-edited.jpg")).as_deref(),
            Some("a.jpg.supplemental-metadata.json")
        );
    }

    #[test]
    fn two_prefix_matching_sidecars_yield_no_match_at_all() {
        // A wrong date is worse than a missing one, so ambiguity is never
        // resolved by guessing.
        let (_d, index) = indexed(&["a.jpg", "a.jpg.suppl.json", "a.jpg.supp.json"]);
        assert!(
            match_sidecar(&index, "a.jpg").is_none(),
            "an ambiguous prefix match must not pick one arbitrarily"
        );
        assert!(matches!(
            match_sidecar_detailed(&index, "a.jpg"),
            SidecarMatch::Ambiguous
        ));
    }

    #[test]
    fn a_file_with_no_sidecar_matches_nothing() {
        let (_d, index) = indexed(&["a.jpg"]);
        assert!(match_sidecar(&index, "a.jpg").is_none());
        assert!(matches!(
            match_sidecar_detailed(&index, "a.jpg"),
            SidecarMatch::Missing
        ));
    }

    #[test]
    fn takeouts_own_bookkeeping_json_is_never_a_sidecar() {
        let (_d, index) = indexed(&[
            "a.jpg",
            "metadata.json",
            "print-subscriptions.json",
            "shared_album_comments.json",
            "user-generated-memory-titles.json",
        ]);
        assert!(
            index.is_empty(),
            "album and account JSON must be excluded before matching"
        );
        assert!(match_sidecar(&index, "a.jpg").is_none());
    }

    #[test]
    fn the_capture_time_is_photo_taken_time_never_creation_time() {
        // creationTime is when the file was uploaded to Google, which for a
        // migrated library is years after the photo was taken. Preferring it
        // is the single most common way other tools get this wrong, and the
        // field names actively invite the mistake.
        let meta = parse_sidecar(
            r#"{
                "title": "IMG_1234.jpg",
                "photoTakenTime": { "timestamp": "1546344000", "formatted": "1 Jan 2019" },
                "creationTime":   { "timestamp": "1600000000", "formatted": "13 Sep 2020" }
            }"#,
        )
        .unwrap();
        assert_eq!(meta.taken_unix, Some(1_546_344_000));
    }

    #[test]
    fn the_timestamp_is_parsed_from_a_string() {
        // Google writes Unix seconds as a JSON string, not a number.
        let meta = parse_sidecar(r#"{"photoTakenTime":{"timestamp":"1546344000"}}"#).unwrap();
        assert_eq!(meta.taken_unix, Some(1_546_344_000));
    }

    #[test]
    fn geo_data_at_exactly_zero_means_absent() {
        // Takeout writes 0.0, 0.0 when there is no location rather than
        // omitting the field. Taken literally that is a point in the Gulf of
        // Guinea, and importing it would put a spurious cluster there for
        // every photo without GPS.
        let none = parse_sidecar(
            r#"{"photoTakenTime":{"timestamp":"1"},
                "geoData":{"latitude":0.0,"longitude":0.0,"altitude":0.0}}"#,
        )
        .unwrap();
        assert_eq!(none.gps, None);

        let some = parse_sidecar(
            r#"{"photoTakenTime":{"timestamp":"1"},
                "geoData":{"latitude":52.37,"longitude":4.89,"altitude":2.0}}"#,
        )
        .unwrap();
        assert_eq!(some.gps, Some((52.37, 4.89)));
    }

    #[test]
    fn a_missing_photo_taken_time_yields_no_date_at_all() {
        let meta = parse_sidecar(r#"{"creationTime":{"timestamp":"1600000000"}}"#).unwrap();
        assert_eq!(
            meta.taken_unix, None,
            "creationTime must never serve as a fallback"
        );
    }

    #[test]
    fn an_unparseable_timestamp_yields_no_date_rather_than_a_wrong_one() {
        let meta = parse_sidecar(r#"{"photoTakenTime":{"timestamp":"not a number"}}"#).unwrap();
        assert_eq!(meta.taken_unix, None);
    }

    #[test]
    fn malformed_json_is_an_error_the_caller_can_report_and_continue_from() {
        assert!(parse_sidecar("{ this is not json").is_err());
    }

    #[test]
    fn an_empty_directory_indexes_to_nothing() {
        let d = tempdir().unwrap();
        assert!(SidecarIndex::for_dir(d.path()).is_empty());
        assert!(SidecarIndex::for_dir(Path::new("/definitely/not/here")).is_empty());
    }
}
