//! Where a clustering pass gets its parameters: command-line flags, then the
//! library's `gallery.json` (`faces.clustering`, written by the gallery's
//! recluster), then the built-in set. Per field, so a saved `eps` alone
//! changes only `eps`.
//!
//! A bad `gallery.json` never stops a faces run: an unreadable file or an
//! invalid field is reported and the built-in value is used instead.

use serde_json::Value;
use std::path::Path;
use videre_ml::cluster_params::{ClusteringParameters, PartialClusteringParameters};

/// The saved override's place in `gallery.json`.
pub(crate) const SECTION: &str = "faces";
pub(crate) const KEY: &str = "clustering";

/// How the one-line note names the file.
const SHOWN_PATH: &str = ".videre/gallery.json";

/// The override saved in a library's `gallery.json`, and anything wrong with it.
#[derive(Debug, Default)]
pub(crate) struct Saved {
    pub partial: PartialClusteringParameters,
    pub warnings: Vec<String>,
}

pub(crate) fn saved(state_dir: &Path) -> Saved {
    use crate::commands::gallery::settings::{load, path, Stored};
    let file = path(state_dir);
    match load(&file) {
        Stored::Absent => Saved::default(),
        Stored::Invalid(error) => Saved {
            partial: PartialClusteringParameters::default(),
            warnings: vec![format!(
                "gallery clustering settings not read, using the built-in values: {error}"
            )],
        },
        Stored::Valid(value) => from_settings(&value),
    }
}

/// Read `faces.clustering` out of a settings document, keeping each field
/// that is the right type and in range and reporting every other one.
pub(crate) fn from_settings(settings: &Value) -> Saved {
    let mut out = Saved::default();
    let Some(section) = settings.get(SECTION).and_then(|s| s.get(KEY)) else {
        return out;
    };
    let Some(fields) = section.as_object() else {
        out.warnings.push(format!(
            "{SECTION}.{KEY} in {SHOWN_PATH} is not an object; using the built-in clustering values"
        ));
        return out;
    };
    for (name, value) in fields {
        let one = serde_json::json!({ name: value });
        let parsed = serde_json::from_value::<PartialClusteringParameters>(one)
            .ok()
            .filter(|p| !p.is_empty());
        let Some(parsed) = parsed else {
            out.warnings.push(format!(
                "{SECTION}.{KEY}.{name} in {SHOWN_PATH} is not a clustering setting of the right type; ignored"
            ));
            continue;
        };
        let mut check = ClusteringParameters::default();
        parsed.apply_to(&mut check);
        if check.validate().is_err() {
            out.warnings.push(format!(
                "{SECTION}.{KEY}.{name} in {SHOWN_PATH} is out of range ({value}); ignored"
            ));
            continue;
        }
        parsed.apply_to_partial(&mut out.partial);
    }
    out
}

/// The parameters a run uses, and which of them came from where.
#[derive(Debug)]
pub(crate) struct Resolved {
    pub params: ClusteringParameters,
    /// Saved fields in effect (not overridden by a flag).
    pub from_gallery: PartialClusteringParameters,
    /// `(name, flag value, saved value)` for each flag that overrode a saved field.
    pub overridden: Vec<(&'static str, String, String)>,
}

pub(crate) fn resolve(
    flags: &PartialClusteringParameters,
    saved: &PartialClusteringParameters,
) -> Resolved {
    let mut params = ClusteringParameters::default();
    saved.apply_to(&mut params);
    flags.apply_to(&mut params);

    let flag_fields = flags.fields();
    let mut from_gallery = PartialClusteringParameters::default();
    let mut overridden = Vec::new();
    for (name, saved_value) in saved.fields() {
        match flag_fields.iter().find(|(n, _)| *n == name) {
            Some((_, flag_value)) => overridden.push((name, flag_value.clone(), saved_value)),
            None => saved.copy_field_to(name, &mut from_gallery),
        }
    }
    Resolved {
        params,
        from_gallery,
        overridden,
    }
}

impl Resolved {
    /// One line saying `gallery.json` shaped this run, or `None` when it did not.
    pub(crate) fn info_line(&self) -> Option<String> {
        let join = |parts: Vec<String>| parts.join(", ");
        let used: Vec<String> = self
            .from_gallery
            .fields()
            .into_iter()
            .map(|(n, v)| format!("{n} {v}"))
            .collect();
        let over: Vec<String> = self
            .overridden
            .iter()
            .map(|(n, flag, saved)| format!("{n} {flag} (gallery.json has {saved})"))
            .collect();
        match (used.is_empty(), over.is_empty()) {
            (true, true) => None,
            (false, true) => Some(format!(
                "Clustering with gallery settings: {} (from {SHOWN_PATH})",
                join(used)
            )),
            (false, false) => Some(format!(
                "Clustering with gallery settings: {} (from {SHOWN_PATH}); flags override {}",
                join(used),
                join(over)
            )),
            (true, false) => Some(format!(
                "Clustering flags override gallery settings: {}",
                join(over)
            )),
        }
    }
}

/// Resolve and report: warnings about `gallery.json`, then the one-line note.
/// Under `--silent` the note goes to the log file only.
pub(crate) fn resolve_for_run(
    state_dir: &Path,
    flags: &PartialClusteringParameters,
    silent: bool,
) -> ClusteringParameters {
    let saved = saved(state_dir);
    for warning in &saved.warnings {
        tracing::warn!("{warning}");
    }
    let resolved = resolve(flags, &saved.partial);
    if let Some(line) = resolved.info_line() {
        if silent {
            tracing::info!(target: "videre::file_only", "{line}");
        } else {
            tracing::info!("{line}");
        }
    }
    resolved.params
}

/// What the gallery saves after applying `params`: the fields that differ
/// from the built-in set, or `null` to remove the override entirely.
pub(crate) fn saved_value(params: &ClusteringParameters) -> Value {
    let diff = PartialClusteringParameters::differing_from_default(params);
    if diff.is_empty() {
        Value::Null
    } else {
        serde_json::to_value(diff).expect("parameters serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn partial(eps: Option<f32>, min: Option<usize>) -> PartialClusteringParameters {
        PartialClusteringParameters {
            eps,
            min_cluster_size: min,
            ..Default::default()
        }
    }

    #[test]
    fn a_missing_file_or_section_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = saved(dir.path());
        assert!(s.partial.is_empty() && s.warnings.is_empty());
        let s = from_settings(&json!({ "routes": {} }));
        assert!(s.partial.is_empty() && s.warnings.is_empty());
    }

    #[test]
    fn saved_fields_apply_one_by_one() {
        let s = from_settings(
            &json!({ "faces": { "clustering": { "eps": 0.7, "min_cluster_size": 2 } } }),
        );
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert_eq!(s.partial, partial(Some(0.7), Some(2)));
        let r = resolve(&PartialClusteringParameters::default(), &s.partial);
        assert_eq!((r.params.eps, r.params.min_cluster_size), (0.7, 2));
        assert_eq!(
            r.params.attach_sim,
            ClusteringParameters::default().attach_sim
        );
    }

    #[test]
    fn a_bad_field_is_reported_and_the_rest_still_apply() {
        let s = from_settings(&json!({ "faces": { "clustering": {
            "eps": "wide", "min_cluster_size": 0, "attach_sim": 0.3, "colour": 1
        } } }));
        assert_eq!(s.partial.attach_sim, Some(0.3));
        assert_eq!(s.partial.eps, None);
        assert_eq!(s.partial.min_cluster_size, None);
        assert_eq!(s.warnings.len(), 3, "{:?}", s.warnings);
        assert!(s.warnings.iter().any(|w| w.contains("clustering.eps")));
        assert!(s
            .warnings
            .iter()
            .any(|w| w.contains("min_cluster_size") && w.contains("out of range")));
    }

    #[test]
    fn an_unreadable_file_warns_and_uses_the_built_in_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gallery.json"), "{oops").unwrap();
        let s = saved(dir.path());
        assert!(s.partial.is_empty());
        assert_eq!(s.warnings.len(), 1);
    }

    #[test]
    fn a_flag_overrides_the_saved_value_and_the_line_says_so() {
        let r = resolve(&partial(Some(0.65), None), &partial(Some(0.7), Some(2)));
        assert_eq!((r.params.eps, r.params.min_cluster_size), (0.65, 2));
        assert_eq!(
            r.info_line().unwrap(),
            "Clustering with gallery settings: min_cluster_size 2 (from .videre/gallery.json); \
             flags override eps 0.65 (gallery.json has 0.7)"
        );
        let r = resolve(&partial(Some(0.65), None), &partial(Some(0.7), None));
        assert_eq!(
            r.info_line().unwrap(),
            "Clustering flags override gallery settings: eps 0.65 (gallery.json has 0.7)"
        );
    }

    #[test]
    fn the_line_names_what_came_from_gallery_json_and_nothing_else() {
        let r = resolve(&partial(None, Some(4)), &partial(Some(0.7), None));
        assert_eq!(
            r.info_line().unwrap(),
            "Clustering with gallery settings: eps 0.7 (from .videre/gallery.json)"
        );
        let r = resolve(
            &partial(Some(0.5), None),
            &PartialClusteringParameters::default(),
        );
        assert_eq!(r.info_line(), None, "flags alone are not news");
    }

    #[test]
    fn only_non_default_fields_are_saved() {
        let mut p = ClusteringParameters::default();
        assert_eq!(saved_value(&p), Value::Null);
        p.eps = 0.7;
        assert_eq!(saved_value(&p), json!({ "eps": 0.7f32 }));
    }
}
