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

use videre_core::selection::{MediaKind, PathSelection, PlaceQuery, RowSelection};

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
    paths: Option<&PathArgs>,
) -> anyhow::Result<RowSelection> {
    let (after, before) = match dates {
        Some(d) => d.bounds()?,
        None => (None, None),
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
        paths: paths.map(|p| p.path.clone()).unwrap_or_default(),
        ..Default::default()
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
        let s = row_selection(None, None, None, None, None).unwrap();
        assert!(s.is_empty());

        let media = MediaArgs {
            media_type: vec!["video".into()],
            ..Default::default()
        };
        let s = row_selection(Some(&media), None, None, None, None).unwrap();
        assert_eq!(s.kinds.len(), 1);
        assert!(s.person.is_none() && s.category.is_none());
        assert!(!s.is_empty());
    }
}
