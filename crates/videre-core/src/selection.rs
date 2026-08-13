//! Shared vocabulary for saying *which* files a command should work on.
//!
//! Two selections exist, deliberately as separate types:
//!
//! - `RowSelection` for commands that query rows (`search`, `embed`, `faces`,
//!   `classify`, `locations`). It can filter on anything recorded.
//! - `PathSelection` for commands that walk a filesystem (`scan`, `watch`).
//!   It can only filter on what is knowable *before* a file is read.
//!
//! Keeping them separate is the point: adding a predicate to the row side
//! cannot change what `scan` accepts, because `scan` does not take that type.
//!
//! This module holds the primitives both share. The selections themselves and
//! their resolution follow in the same module.

use anyhow::{bail, Result};

/// The coarse kind of a media file.
///
/// `--type image` / `--type video`. A value flag rather than boolean
/// `--image`/`--video` flags, for symmetry with `--ext` and `--mime`, which
/// have no boolean form, and because it extends to further kinds without
/// growing the flag surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

impl MediaKind {
    /// Parse a user-supplied kind, naming every valid value on failure.
    ///
    /// An unknown *kind* is a typo worth reporting, unlike an unknown
    /// extension, which legitimately matches nothing.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "image" => Ok(Self::Image),
            "video" => Ok(Self::Video),
            other => bail!("unknown --type {other:?}; valid values are: image, video"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
        }
    }
}

/// Normalise a user-supplied extension so `.MOV`, `MOV` and `mov` agree.
///
/// Done once at parse time rather than per row: a selection is compared
/// against tens of thousands of files.
pub fn normalise_ext(s: &str) -> String {
    s.trim().trim_start_matches('.').to_lowercase()
}

/// Whether a row's type matches `kind`.
///
/// Resolves through `mime_probe::effective_mime`, which is what every other
/// consumer does: a file whose magic bytes could not be identified stores the
/// sentinel `application/octet-stream`, and `effective_mime` falls back to the
/// extension. A `.jpg` whose header was unreadable is still a photo to its
/// owner, and treating the sentinel as a type of its own would drop those files
/// out of `--type image` for a reason the user cannot see.
pub fn row_matches_kind(kind: MediaKind, mime: Option<&str>, ext: &str) -> bool {
    let Some(m) = crate::mime_probe::effective_mime(mime, &ext.to_lowercase()) else {
        return false;
    };
    match kind {
        MediaKind::Image => crate::mime_probe::PHOTO_MIMES.contains(&m),
        MediaKind::Video => crate::mime_probe::VIDEO_MIMES.contains(&m),
    }
}

/// Whether a *path* matches `kind`, judged by extension alone.
///
/// :warning: This deliberately differs from `row_matches_kind`. A walk decides
/// whether to read a file before it has read it, so the magic bytes are not
/// available and the extension is all there is. On a library whose extensions
/// are wrong, `scan --type video` and `search --type video` will disagree, and
/// that is inherent to filtering before reading rather than a bug to fix.
pub fn path_matches_kind(kind: MediaKind, path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext.is_empty() {
        return false;
    }
    row_matches_kind(kind, None, &ext)
}

use crate::query::{self, GeoFilter};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A place to select around: either a name needing geocoding, or coordinates.
///
/// Both exist because `search` takes a place *name* and geocodes it (a network
/// call on a cache miss) while the underlying predicate wants coordinates. The
/// layer owns both so that every command accepting `--location` geocodes
/// identically instead of reimplementing it.
#[derive(Debug, Clone)]
pub enum PlaceQuery {
    Named { place: String, radius_km: f64 },
    Coords(GeoFilter),
}

/// What a command was asked to work on, over rows already in the database.
#[derive(Debug, Clone, Default)]
pub struct RowSelection {
    pub person: Option<String>,
    pub category: Option<String>,
    pub place: Option<PlaceQuery>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub kinds: Vec<MediaKind>,
    pub exts: Vec<String>,
    pub mimes: Vec<String>,
    pub paths: Vec<PathBuf>,
}

/// What a command can offer the resolver about itself.
#[derive(Debug, Clone, Default)]
pub struct SelectionCtx {
    /// Needed to resolve `--category`, which is scoped to an embedding model.
    /// `None` for commands with no model concept, such as `faces`.
    pub model_id: Option<String>,
}

/// The outcome of resolving a selection.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    /// `None` means nothing was selected: do not constrain, process everything.
    /// `Some(empty)` means a selection ran and matched nothing: process
    /// nothing. Collapsing the two would turn a typo into a full-library run.
    pub hashes: Option<HashSet<String>>,
    /// Km from the requested place, per surviving hash. `Some` only when a
    /// place was given. Carried because `search --sort distance` needs it.
    pub distances: Option<HashMap<String, f64>>,
}

impl RowSelection {
    /// True when no predicate was given at all.
    ///
    /// Derived from the fields directly rather than kept as a separate
    /// hand-maintained list, because the failure mode of forgetting to update
    /// such a list is silent: the command processes the entire library.
    pub fn is_empty(&self) -> bool {
        self.person.is_none()
            && self.category.is_none()
            && self.place.is_none()
            && self.after.is_none()
            && self.before.is_none()
            && self.kinds.is_empty()
            && self.exts.is_empty()
            && self.mimes.is_empty()
            && self.paths.is_empty()
    }

    /// Human-readable form for progress lines and confirmations.
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = &self.person {
            parts.push(format!("--person {p:?}"));
        }
        if let Some(c) = &self.category {
            parts.push(format!("--category {c}"));
        }
        match &self.place {
            Some(PlaceQuery::Named { place, radius_km }) => {
                parts.push(format!("--location {place:?} --radius {radius_km}"))
            }
            Some(PlaceQuery::Coords(g)) => parts.push(format!(
                "--location {},{} --radius {}",
                g.lat, g.lon, g.radius_km
            )),
            None => {}
        }
        if let Some(a) = &self.after {
            parts.push(format!("--after {a}"));
        }
        if let Some(b) = &self.before {
            parts.push(format!("--before {b}"));
        }
        for k in &self.kinds {
            parts.push(format!("--type {}", k.as_str()));
        }
        if !self.exts.is_empty() {
            parts.push(format!("--ext {}", self.exts.join(",")));
        }
        if !self.mimes.is_empty() {
            parts.push(format!("--mime {}", self.mimes.join(",")));
        }
        for p in &self.paths {
            parts.push(format!("--path {}", p.display()));
        }
        parts.join(" ")
    }

    /// Run every active predicate and intersect them.
    ///
    /// Predicates OR within an axis (`--ext mov,avi` matches either) and AND
    /// across axes (`--type video --ext jpg` matches nothing), which is how
    /// every existing filter already composes.
    pub fn resolve(&self, conn: &Connection, ctx: &SelectionCtx) -> anyhow::Result<Resolved> {
        if self.is_empty() {
            return Ok(Resolved::default());
        }

        let mut acc: Option<HashSet<String>> = None;
        let mut narrow = |s: HashSet<String>, acc: &mut Option<HashSet<String>>| match acc {
            Some(existing) => *acc = Some(existing.intersection(&s).cloned().collect()),
            None => *acc = Some(s),
        };

        if let Some(p) = &self.person {
            narrow(query::by_person(conn, p)?, &mut acc);
        }
        if let Some(c) = &self.category {
            let model = ctx.model_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "--category needs an embedding model, and this command has none; \
                     classifications are stored per model"
                )
            })?;
            narrow(query::by_category(conn, model, c)?, &mut acc);
        }
        if self.after.is_some() || self.before.is_some() {
            narrow(
                query::by_date(conn, self.after.as_deref(), self.before.as_deref())?,
                &mut acc,
            );
        }
        if !self.kinds.is_empty() {
            narrow(by_kinds(conn, &self.kinds)?, &mut acc);
        }
        if !self.exts.is_empty() {
            narrow(by_exts(conn, &self.exts)?, &mut acc);
        }
        if !self.mimes.is_empty() {
            narrow(by_mimes(conn, &self.mimes)?, &mut acc);
        }
        if !self.paths.is_empty() {
            narrow(by_paths(conn, &self.paths)?, &mut acc);
        }

        // Place last: geocoding may hit the network, so an already-empty
        // candidate set should skip it entirely.
        let mut distances = None;
        if let Some(place) = &self.place {
            if acc.as_ref().is_some_and(|h| h.is_empty()) {
                distances = Some(HashMap::new());
            } else {
                let geo = match place {
                    PlaceQuery::Coords(g) => *g,
                    PlaceQuery::Named { place, radius_km } => {
                        crate::geocode::ensure_geocode_cache_table(conn)?;
                        let (lat, lon) = crate::geocode::forward_geocode_cached(conn, place)?;
                        GeoFilter {
                            lat,
                            lon,
                            radius_km: *radius_km,
                        }
                    }
                };
                let within = query::by_location(conn, geo.lat, geo.lon, geo.radius_km)?;
                let keep: HashSet<String> = match &acc {
                    Some(existing) => within
                        .keys()
                        .filter(|h| existing.contains(*h))
                        .cloned()
                        .collect(),
                    None => within.keys().cloned().collect(),
                };
                distances = Some(
                    within
                        .into_iter()
                        .filter(|(h, _)| keep.contains(h))
                        .collect(),
                );
                acc = Some(keep);
            }
        }

        Ok(Resolved {
            hashes: acc,
            distances,
        })
    }
}

/// Hashes whose type matches any of `kinds`.
///
/// Filtered in Rust rather than SQL because the sentinel-mime fallback lives in
/// `effective_mime`; expressing it in SQL would duplicate logic that already
/// exists and would drift from it.
fn by_kinds(conn: &Connection, kinds: &[MediaKind]) -> anyhow::Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT hash, mime, ext FROM file_hashes")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = HashSet::new();
    for row in rows {
        let (hash, mime, ext) = row?;
        let ext = ext.unwrap_or_default();
        if kinds
            .iter()
            .any(|k| row_matches_kind(*k, mime.as_deref(), &ext))
        {
            out.insert(hash);
        }
    }
    Ok(out)
}

fn by_exts(conn: &Connection, exts: &[String]) -> anyhow::Result<HashSet<String>> {
    let wanted: HashSet<String> = exts.iter().map(|e| normalise_ext(e)).collect();
    let mut stmt = conn.prepare("SELECT hash, ext FROM file_hashes WHERE ext IS NOT NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashSet::new();
    for row in rows {
        let (hash, ext) = row?;
        if wanted.contains(&normalise_ext(&ext)) {
            out.insert(hash);
        }
    }
    Ok(out)
}

fn by_mimes(conn: &Connection, mimes: &[String]) -> anyhow::Result<HashSet<String>> {
    let wanted: HashSet<String> = mimes.iter().map(|m| m.trim().to_lowercase()).collect();
    let mut stmt = conn.prepare("SELECT hash, mime FROM file_hashes WHERE mime IS NOT NULL")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashSet::new();
    for row in rows {
        let (hash, mime) = row?;
        if wanted.contains(&mime.to_lowercase()) {
            out.insert(hash);
        }
    }
    Ok(out)
}

/// Hashes whose path lies under any of `roots`.
///
/// Compares **path components**, not string prefixes, so `/Pictures/2024` does
/// not also match `/Pictures/2024-old`. Both sides are canonicalised where
/// possible so a symlinked or relative selection still matches; a root that
/// cannot be canonicalised (it may not exist) is used as given rather than
/// treated as an error, since selecting a missing directory legitimately
/// matches nothing.
fn by_paths(conn: &Connection, roots: &[PathBuf]) -> anyhow::Result<HashSet<String>> {
    let roots: Vec<PathBuf> = roots
        .iter()
        .map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.clone()))
        .collect();
    let mut stmt = conn.prepare("SELECT hash, path FROM file_hashes")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashSet::new();
    for row in rows {
        let (hash, path) = row?;
        let p = Path::new(&path);
        if roots.iter().any(|r| under(p, r)) {
            out.insert(hash);
        }
    }
    Ok(out)
}

fn under(path: &Path, root: &Path) -> bool {
    let mut a = path.components();
    for c in root.components() {
        match a.next() {
            Some(x) if x == c => {}
            _ => return false,
        }
    }
    true
}

/// What a *walking* command was asked to work on.
///
/// Deliberately smaller than `RowSelection` and a separate type: a walk decides
/// whether to read a file before reading it, so it cannot answer questions
/// about dates, coordinates, people or true mime type. Excluding those from the
/// vocabulary is what makes the flags they share safe to name identically -
/// a `scan --after` meaning "re-scan known rows in this range" while
/// `embed --after` means "restrict to these files" would be one flag with two
/// meanings, one command apart.
#[derive(Debug, Clone, Default)]
pub struct PathSelection {
    pub kinds: Vec<MediaKind>,
    pub exts: Vec<String>,
    pub paths: Vec<PathBuf>,
}

impl PathSelection {
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty() && self.exts.is_empty() && self.paths.is_empty()
    }

    /// Whether the walk should keep this path.
    ///
    /// Pure: no database, and no I/O beyond the canonicalisation the roots
    /// already had. An empty selection accepts everything, so a command with no
    /// flags behaves exactly as before.
    pub fn accepts(&self, path: &Path) -> bool {
        if self.is_empty() {
            return true;
        }
        if !self.kinds.is_empty() && !self.kinds.iter().any(|k| path_matches_kind(*k, path)) {
            return false;
        }
        if !self.exts.is_empty() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(normalise_ext)
                .unwrap_or_default();
            if !self.exts.iter().any(|e| normalise_ext(e) == ext) {
                return false;
            }
        }
        if !self.paths.is_empty() && !self.paths.iter().any(|r| under(path, r)) {
            return false;
        }
        true
    }

    /// Canonicalise the roots once, so `accepts` stays cheap across a walk of
    /// tens of thousands of entries.
    pub fn canonicalised(mut self) -> Self {
        self.paths = self
            .paths
            .iter()
            .map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.clone()))
            .collect();
        self
    }

    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for k in &self.kinds {
            parts.push(format!("--type {}", k.as_str()));
        }
        if !self.exts.is_empty() {
            parts.push(format!("--ext {}", self.exts.join(",")));
        }
        for p in &self.paths {
            parts.push(format!("--path {}", p.display()));
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod path_selection_tests {
    use super::*;

    #[test]
    fn an_empty_selection_accepts_everything() {
        // A command with no flags must walk exactly as it did before.
        let s = PathSelection::default();
        assert!(s.accepts(Path::new("/a/b.jpg")));
        assert!(s.accepts(Path::new("/a/b.mov")));
    }

    #[test]
    fn kinds_and_exts_and_paths_all_narrow() {
        let s = PathSelection {
            kinds: vec![MediaKind::Video],
            ..Default::default()
        };
        assert!(s.accepts(Path::new("/a/b.mov")));
        assert!(!s.accepts(Path::new("/a/b.jpg")));

        let s = PathSelection {
            exts: vec![".MOV".into()],
            ..Default::default()
        };
        assert!(s.accepts(Path::new("/a/b.mov")), "case and dot normalise");

        let s = PathSelection {
            paths: vec![PathBuf::from("/lib")],
            ..Default::default()
        };
        assert!(s.accepts(Path::new("/lib/a.jpg")));
        assert!(
            !s.accepts(Path::new("/library/a.jpg")),
            "components, not prefix"
        );
    }

    #[test]
    fn axes_intersect() {
        let s = PathSelection {
            kinds: vec![MediaKind::Video],
            paths: vec![PathBuf::from("/lib")],
            ..Default::default()
        };
        assert!(s.accepts(Path::new("/lib/a.mov")));
        assert!(!s.accepts(Path::new("/lib/a.jpg")), "wrong kind");
        assert!(!s.accepts(Path::new("/other/a.mov")), "wrong place");
    }

    #[test]
    fn type_here_is_by_extension_and_that_differs_from_rows() {
        // Asserted on purpose. A walk has not read the file, so a .mov whose
        // bytes are really a JPEG is accepted by `scan --type video` and
        // rejected by `search --type video`. Inherent to filtering before
        // reading; not a bug to "fix" into agreement.
        let s = PathSelection {
            kinds: vec![MediaKind::Video],
            ..Default::default()
        };
        assert!(s.accepts(Path::new("/a/mislabelled.mov")));
        assert!(!row_matches_kind(
            MediaKind::Video,
            Some("image/jpeg"),
            "mov"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_names_every_valid_value_on_a_typo() {
        assert_eq!(MediaKind::parse("image").unwrap(), MediaKind::Image);
        assert_eq!(MediaKind::parse("VIDEO").unwrap(), MediaKind::Video);
        assert_eq!(MediaKind::parse("  video  ").unwrap(), MediaKind::Video);
        let e = MediaKind::parse("vidoe").unwrap_err().to_string();
        assert!(e.contains("image") && e.contains("video"), "got: {e}");
    }

    #[test]
    fn extensions_normalise_to_one_spelling() {
        for s in [".MOV", "MOV", "mov", " .mov "] {
            assert_eq!(normalise_ext(s), "mov", "input {s:?}");
        }
    }

    #[test]
    fn an_unidentified_file_is_still_its_extension() {
        // The sentinel means "could not identify", not "a type of its own".
        // Dropping these from --type image would be invisible to the user.
        assert!(row_matches_kind(
            MediaKind::Image,
            Some(crate::mime_probe::UNKNOWN_MIME),
            "jpg"
        ));
        assert!(row_matches_kind(MediaKind::Video, None, "mov"));
    }

    #[test]
    fn mime_beats_a_wrong_extension_for_rows() {
        assert!(row_matches_kind(
            MediaKind::Image,
            Some("image/jpeg"),
            "txt"
        ));
        assert!(!row_matches_kind(
            MediaKind::Video,
            Some("image/jpeg"),
            "mov"
        ));
    }

    #[test]
    fn paths_are_judged_by_extension_only() {
        // The divergence from row matching, asserted on purpose: a walk has not
        // read the file, so this is all it can know.
        assert!(path_matches_kind(MediaKind::Video, Path::new("/a/b.mov")));
        assert!(path_matches_kind(MediaKind::Image, Path::new("/a/b.HEIC")));
        assert!(!path_matches_kind(MediaKind::Image, Path::new("/a/b.mov")));
        assert!(!path_matches_kind(MediaKind::Image, Path::new("/a/noext")));
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                created_at TEXT, modified_at TEXT, ext TEXT, mime TEXT, phash INTEGER,
                exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
             INSERT INTO file_hashes (path, hash, ext, mime, exif_date, modified_at) VALUES
               ('/lib/a.jpg','h_jpg','jpg','image/jpeg','2024-05-01T10:00:00','2024-05-01T10:00:00'),
               ('/lib/b.mov','h_mov','mov','video/quicktime','2024-06-01T10:00:00','2024-06-01T10:00:00'),
               ('/lib/c.heic','h_heic','heic','image/heic','2023-01-01T10:00:00','2023-01-01T10:00:00'),
               ('/other/d.mp4','h_mp4','mp4','video/mp4','2024-07-01T10:00:00','2024-07-01T10:00:00');",
        )
        .unwrap();
        c
    }

    fn sel() -> RowSelection {
        RowSelection::default()
    }

    #[test]
    fn no_selection_means_unconstrained_not_empty() {
        // The distinction that matters: None = process everything,
        // Some(empty) = process nothing. Collapsing them turns a typo into a
        // full-library run.
        let r = sel().resolve(&db(), &SelectionCtx::default()).unwrap();
        assert!(r.hashes.is_none(), "no predicate given must not constrain");
        assert!(sel().is_empty());
    }

    #[test]
    fn or_within_an_axis() {
        let mut s = sel();
        s.exts = vec!["mov".into(), "mp4".into()];
        let r = s.resolve(&db(), &SelectionCtx::default()).unwrap();
        let h = r.hashes.unwrap();
        assert_eq!(h.len(), 2);
        assert!(h.contains("h_mov") && h.contains("h_mp4"));
    }

    #[test]
    fn and_across_axes_can_be_empty_without_being_unconstrained() {
        let mut s = sel();
        s.kinds = vec![MediaKind::Video];
        s.exts = vec!["jpg".into()];
        let r = s.resolve(&db(), &SelectionCtx::default()).unwrap();
        let h = r.hashes.expect("an active selection must constrain");
        assert!(h.is_empty(), "video AND jpg matches nothing");
    }

    #[test]
    fn kind_uses_mime_and_covers_every_image_type() {
        let mut s = sel();
        s.kinds = vec![MediaKind::Image];
        let h = s
            .resolve(&db(), &SelectionCtx::default())
            .unwrap()
            .hashes
            .unwrap();
        assert_eq!(h.len(), 2, "jpg and heic");
        assert!(h.contains("h_jpg") && h.contains("h_heic"));
    }

    #[test]
    fn dates_and_types_intersect() {
        let mut s = sel();
        s.kinds = vec![MediaKind::Video];
        s.after = Some("2024-06-15T00:00:00".into());
        let h = s
            .resolve(&db(), &SelectionCtx::default())
            .unwrap()
            .hashes
            .unwrap();
        assert_eq!(h.len(), 1, "only the July mp4");
        assert!(h.contains("h_mp4"));
    }

    #[test]
    fn path_matches_components_not_string_prefixes() {
        // /lib must not also match a sibling like /library.
        let mut s = sel();
        s.paths = vec![PathBuf::from("/lib")];
        let h = s
            .resolve(&db(), &SelectionCtx::default())
            .unwrap()
            .hashes
            .unwrap();
        assert_eq!(h.len(), 3, "the three under /lib, not /other");
        assert!(!h.contains("h_mp4"));

        assert!(under(Path::new("/lib/a.jpg"), Path::new("/lib")));
        assert!(!under(Path::new("/library/a.jpg"), Path::new("/lib")));
    }

    #[test]
    fn category_without_a_model_is_an_error_naming_the_reason() {
        // Silently returning nothing would look like "no files match".
        let mut s = sel();
        s.category = Some("document".into());
        let e = s
            .resolve(&db(), &SelectionCtx::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("model"), "got: {e}");
    }

    #[test]
    fn describe_round_trips_the_flags_a_user_typed() {
        let mut s = sel();
        s.kinds = vec![MediaKind::Video];
        s.after = Some("2024-01-01".into());
        let d = s.describe();
        assert!(
            d.contains("--type video") && d.contains("--after 2024-01-01"),
            "{d}"
        );
    }
}
