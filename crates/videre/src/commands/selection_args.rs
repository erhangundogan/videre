//! The selection flags, defined once and flattened by whichever commands
//! honour them.
//!
//! Deliberately several small groups rather than one struct. If every command
//! flattened a single `SelectionArgs`, every command would get every flag -
//! including ones it cannot answer, like `--category` on `faces`, which has no
//! embedding model to resolve a classification against. Grouping lets a command
//! declare its vocabulary in its own `Args` type, so an unanswerable request
//! does not parse rather than failing at runtime.
//!
//! The predicates themselves live once in `videre_core::selection`; only the
//! composition of groups is per command.

use videre_core::selection::{MediaKind, PathSelection, PlaceQuery, PresenceField, RowSelection};

/// `--type`, `--ext`, `--mime`. The only group both selection kinds honour,
/// though they answer `--type` differently: rows by mime, paths by extension.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct MediaArgs {
    /// Media kind: image or video. Repeatable, or comma-separated
    #[arg(long = "type", value_delimiter = ',', value_name = "KIND")]
    pub media_type: Vec<String>,

    /// File extension, e.g. mov. Repeatable, or comma-separated
    #[arg(long, value_delimiter = ',', value_name = "EXT")]
    pub ext: Vec<String>,

    /// Exact mime type, e.g. video/quicktime. Repeatable, or comma-separated
    #[arg(long, value_delimiter = ',', value_name = "MIME")]
    pub mime: Vec<String>,
}

impl MediaArgs {
    /// Parse the kinds, reporting a typo rather than silently ignoring it.
    ///
    /// An unknown extension is not an error - `--ext xyz` legitimately matches
    /// nothing - but an unknown kind is a mistake the user wants told about.
    pub fn kinds(&self) -> anyhow::Result<Vec<MediaKind>> {
        self.media_type
            .iter()
            .map(|s| MediaKind::parse(s))
            .collect()
    }
}

/// `--after`, `--before`, `--date`. Row-side only: a walk has not read the
/// file, so it cannot know when the contents were captured.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct DateArgs {
    /// Only files whose date is on or after this (inclusive)
    #[arg(long, value_name = "DATE")]
    pub after: Option<String>,

    /// Only files whose date is before this (exclusive)
    #[arg(long, value_name = "DATE")]
    pub before: Option<String>,

    /// Shorthand for a whole year, month, or day: YYYY, YYYY-MM, or YYYY-MM-DD
    #[arg(long, value_name = "DATE", conflicts_with_all = ["after", "before"])]
    pub date: Option<String>,
}

impl DateArgs {
    /// Expand `--date` shorthand into the half-open bounds the predicate wants.
    pub fn bounds(&self) -> anyhow::Result<(Option<String>, Option<String>)> {
        if let Some(spec) = &self.date {
            let (a, b) = videre_core::query::expand_date(spec)?;
            return Ok((Some(a), Some(b)));
        }
        Ok((self.after.clone(), self.before.clone()))
    }
}

/// `--location`, `--radius`. Row-side only, and the one group that may reach
/// the network: a place name is geocoded (and cached) on first use.
#[derive(clap::Args, Clone, Debug)]
pub struct PlaceArgs {
    /// Only files within --radius km of this place, e.g. "Berlin, Germany"
    #[arg(long, value_name = "PLACE")]
    pub location: Option<String>,

    /// Search radius in km around --location
    #[arg(long, default_value_t = 20.0, value_name = "KM")]
    pub radius: f64,
}

impl Default for PlaceArgs {
    fn default() -> Self {
        Self {
            location: None,
            radius: 20.0,
        }
    }
}

impl PlaceArgs {
    pub fn place(&self) -> Option<PlaceQuery> {
        self.location.as_ref().map(|p| PlaceQuery::Named {
            place: p.clone(),
            radius_km: self.radius,
        })
    }
}

/// `--person`, `--category`. Row-side, and only for commands with an embedding
/// model: classifications are stored per model.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct PeopleArgs {
    /// Only files containing this labeled person, confirmed faces only
    #[arg(long, value_name = "NAME")]
    pub person: Option<String>,

    /// Only files classified as this category
    #[arg(long, value_name = "CATEGORY")]
    pub category: Option<String>,
}

/// `--path`. The only predicate needing no database at all, so both groups
/// honour it identically.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct PathArgs {
    /// Only files under this directory. Repeatable
    #[arg(long, value_name = "DIR")]
    pub path: Vec<std::path::PathBuf>,
}

/// `--has`, `--missing`. Row-side only: these predicates ask what is present
/// in the database, which a filesystem walk cannot know before reading files.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct PresenceArgs {
    /// Only files where this database field is present. Repeatable, or comma-separated
    #[arg(long = "has", value_delimiter = ',', value_name = "FIELD")]
    pub has: Vec<String>,

    /// Only files where this database field is missing. Repeatable, or comma-separated
    #[arg(long = "missing", value_delimiter = ',', value_name = "FIELD")]
    pub missing: Vec<String>,
}

impl PresenceArgs {
    pub fn fields(&self) -> anyhow::Result<(Vec<PresenceField>, Vec<PresenceField>)> {
        let has = self
            .has
            .iter()
            .map(|s| PresenceField::parse(s))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let missing = self
            .missing
            .iter()
            .map(|s| PresenceField::parse(s))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((has, missing))
    }
}

/// Assemble a row selection from whichever groups a command flattened.
///
/// Takes options so a command passes `None` for a group it does not honour,
/// which is what keeps the vocabulary per command without duplicating this
/// assembly in each of them.
pub fn row_selection(
    media: Option<&MediaArgs>,
    dates: Option<&DateArgs>,
    place: Option<&PlaceArgs>,
    people: Option<&PeopleArgs>,
    presence: Option<&PresenceArgs>,
    paths: Option<&PathArgs>,
    marks: Option<&MarkArgs>,
    tags: Option<&TagFilterArgs>,
) -> anyhow::Result<RowSelection> {
    let (after, before) = match dates {
        Some(d) => d.bounds()?,
        None => (None, None),
    };
    let (has, missing) = match presence {
        Some(p) => p.fields()?,
        None => (Vec::new(), Vec::new()),
    };
    Ok(RowSelection {
        person: people.and_then(|p| p.person.clone()),
        category: people.and_then(|p| p.category.clone()),
        place: place.and_then(|p| p.place()),
        after,
        before,
        kinds: match media {
            Some(m) => m.kinds()?,
            None => Vec::new(),
        },
        exts: media.map(|m| m.ext.clone()).unwrap_or_default(),
        mimes: media.map(|m| m.mime.clone()).unwrap_or_default(),
        has,
        missing,
        paths: paths.map(|p| p.path.clone()).unwrap_or_default(),
        // Mark and tag predicates are stored per hash, so both are row-side
        // filters. A command that sets marks (`mark`) or manages tags (`tag`)
        // passes `None` for the group it uses as a setter instead of a filter.
        min_rating: marks.and_then(|m| m.rating),
        pick: marks.and_then(|m| m.pick_state()),
        label: marks.and_then(|m| m.label.clone()),
        liked: marks.is_some_and(|m| m.like),
        tags: tags.map(|t| t.tags.clone()).unwrap_or_default(),
    })
}

/// `--rating`/`--pick`/`--label`/`--like` as *filters* (row-side only: marks are
/// stored per hash, unknown at walk time). `videre mark` uses the mark flags as
/// *setters* instead, so it does not flatten this group.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct MarkArgs {
    /// Only photos rated at least this many stars (0-5)
    #[arg(long, value_name = "N")]
    pub rating: Option<i64>,

    /// Only photos with this pick state
    #[arg(long, value_name = "keep|reject", value_parser = ["keep", "reject"])]
    pub pick: Option<String>,

    /// Only photos with this colour label
    #[arg(long, value_name = "COLOUR")]
    pub label: Option<String>,

    /// Only liked photos
    #[arg(long)]
    pub like: bool,
}

/// `--tag` as a *filter* (row-side: tags are stored per hash). `videre tag` uses
/// tags as *setters* instead, so it does not flatten this group.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct TagFilterArgs {
    /// Only files carrying this tag. Repeatable; all must be present
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,
}

impl MarkArgs {
    /// The parsed pick state, if `--pick` was given.
    pub fn pick_state(&self) -> Option<videre_core::marks::Pick> {
        match self.pick.as_deref() {
            Some("reject") => Some(videre_core::marks::Pick::Reject),
            Some("keep") => Some(videre_core::marks::Pick::Keep),
            _ => None,
        }
    }
}

/// Assemble a path selection, canonicalising its roots once.
pub fn path_selection(
    media: Option<&MediaArgs>,
    paths: Option<&PathArgs>,
) -> anyhow::Result<PathSelection> {
    Ok(PathSelection {
        kinds: match media {
            Some(m) => m.kinds()?,
            None => Vec::new(),
        },
        exts: media.map(|m| m.ext.clone()).unwrap_or_default(),
        paths: paths.map(|p| p.path.clone()).unwrap_or_default(),
    }
    .canonicalised())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_kind_is_reported_but_an_unknown_extension_is_not() {
        let m = MediaArgs {
            media_type: vec!["vidoe".into()],
            ..Default::default()
        };
        assert!(m.kinds().is_err(), "a typo in --type must be reported");

        let m = MediaArgs {
            ext: vec!["xyz".into()],
            ..Default::default()
        };
        // Nothing to validate: an absent extension legitimately matches nothing.
        assert!(m.kinds().unwrap().is_empty());
    }

    #[test]
    fn date_shorthand_expands_to_half_open_bounds() {
        let d = DateArgs {
            date: Some("2024-06".into()),
            ..Default::default()
        };
        let (a, b) = d.bounds().unwrap();
        assert_eq!(a.as_deref(), Some("2024-06-01T00:00:00"));
        assert_eq!(b.as_deref(), Some("2024-07-01T00:00:00"));
    }

    #[test]
    fn a_command_omitting_a_group_gets_no_predicate_from_it() {
        // The point of grouping: faces passes None for people, so --category
        // cannot arrive at all rather than failing later.
        let s = row_selection(None, None, None, None, None, None, None, None).unwrap();
        assert!(s.is_empty());

        let media = MediaArgs {
            media_type: vec!["video".into()],
            ..Default::default()
        };
        let s = row_selection(Some(&media), None, None, None, None, None, None, None).unwrap();
        assert_eq!(s.kinds.len(), 1);
        assert!(s.person.is_none() && s.category.is_none());
        assert!(!s.is_empty());
    }

    // ---- Appendix A: row_selection() assembler ----

    fn full_mark_args() -> MarkArgs {
        MarkArgs {
            rating: Some(4),
            pick: Some("keep".into()),
            label: Some("Green".into()),
            like: true,
        }
    }

    #[test]
    fn mark_and_tag_groups_populate_the_row_selection() {
        let marks = full_mark_args();
        let tags = TagFilterArgs {
            tags: vec!["trip".into(), "beach".into()],
        };
        let s = row_selection(
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&marks),
            Some(&tags),
        )
        .unwrap();
        assert_eq!(s.min_rating, Some(4));
        assert_eq!(s.pick, Some(videre_core::marks::Pick::Keep));
        assert_eq!(s.label.as_deref(), Some("Green"));
        assert!(s.liked);
        assert_eq!(s.tags, vec!["trip".to_string(), "beach".to_string()]);
        assert!(!s.is_empty());
    }

    #[test]
    fn omitting_the_mark_and_tag_groups_leaves_their_fields_default() {
        // Every other group None too, so only the mark/tag defaults are under
        // test: the selection must read as empty.
        let s = row_selection(None, None, None, None, None, None, None, None).unwrap();
        assert_eq!(s.min_rating, None);
        assert_eq!(s.pick, None);
        assert_eq!(s.label, None);
        assert!(!s.liked);
        assert!(s.tags.is_empty());
        assert!(s.is_empty());
    }

    #[test]
    fn empty_mark_and_tag_groups_do_not_make_a_selection_nonempty() {
        // A command flattens the groups unconditionally, so the common case is
        // the user supplying none of their flags: default groups must not
        // fabricate a predicate.
        let marks = MarkArgs::default();
        let tags = TagFilterArgs::default();
        let s = row_selection(
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&marks),
            Some(&tags),
        )
        .unwrap();
        assert!(s.is_empty(), "default mark/tag groups must select nothing");
    }

    #[test]
    fn each_group_in_isolation_sets_only_its_own_fields() {
        // marks alone
        let marks = full_mark_args();
        let s = row_selection(None, None, None, None, None, None, Some(&marks), None).unwrap();
        assert!(s.min_rating.is_some() && s.pick.is_some() && s.label.is_some() && s.liked);
        assert!(s.tags.is_empty() && s.person.is_none() && s.kinds.is_empty());

        // tags alone
        let tags = TagFilterArgs {
            tags: vec!["only".into()],
        };
        let s = row_selection(None, None, None, None, None, None, None, Some(&tags)).unwrap();
        assert_eq!(s.tags, vec!["only".to_string()]);
        assert!(s.min_rating.is_none() && s.pick.is_none() && s.label.is_none() && !s.liked);
    }

    #[test]
    fn pick_state_maps_each_keyword() {
        let keep = MarkArgs {
            pick: Some("keep".into()),
            ..Default::default()
        };
        assert_eq!(keep.pick_state(), Some(videre_core::marks::Pick::Keep));
        let reject = MarkArgs {
            pick: Some("reject".into()),
            ..Default::default()
        };
        assert_eq!(reject.pick_state(), Some(videre_core::marks::Pick::Reject));
        assert_eq!(MarkArgs::default().pick_state(), None);
        let unknown = MarkArgs {
            pick: Some("bogus".into()),
            ..Default::default()
        };
        assert_eq!(unknown.pick_state(), None);
    }

    #[test]
    fn every_group_together_populates_every_field() {
        let media = MediaArgs {
            media_type: vec!["image".into()],
            ext: vec!["heic".into()],
            mime: vec!["image/heic".into()],
        };
        let dates = DateArgs {
            date: Some("2024".into()),
            ..Default::default()
        };
        let place = PlaceArgs {
            location: Some("Berlin, Germany".into()),
            radius: 10.0,
        };
        let people = PeopleArgs {
            person: Some("Ada".into()),
            category: Some("photo".into()),
        };
        let presence = PresenceArgs {
            has: vec!["gps".into()],
            missing: vec!["date".into()],
        };
        let paths = PathArgs {
            path: vec!["Trips".into()],
        };
        let marks = full_mark_args();
        let tags = TagFilterArgs {
            tags: vec!["t".into()],
        };
        let s = row_selection(
            Some(&media),
            Some(&dates),
            Some(&place),
            Some(&people),
            Some(&presence),
            Some(&paths),
            Some(&marks),
            Some(&tags),
        )
        .unwrap();
        assert_eq!(s.person.as_deref(), Some("Ada"));
        assert_eq!(s.category.as_deref(), Some("photo"));
        assert!(s.place.is_some());
        assert!(s.after.is_some() && s.before.is_some());
        assert_eq!(s.kinds.len(), 1);
        assert_eq!(s.exts, vec!["heic".to_string()]);
        assert_eq!(s.mimes, vec!["image/heic".to_string()]);
        assert_eq!(s.has.len(), 1);
        assert_eq!(s.missing.len(), 1);
        assert_eq!(s.paths.len(), 1);
        assert_eq!(s.min_rating, Some(4));
        assert_eq!(s.pick, Some(videre_core::marks::Pick::Keep));
        assert_eq!(s.label.as_deref(), Some("Green"));
        assert!(s.liked);
        assert_eq!(s.tags, vec!["t".to_string()]);
    }
}
