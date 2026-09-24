//! Gallery settings: the built-in defaults in `static/gallery-defaults.json`
//! with a library's sparse overrides from `<root>/.videre/gallery.json`
//! merged on top.
//!
//! The defaults are the only declaration of a setting, and a setting's type
//! is the JSON type of its default. The server never needs a setting's name:
//! enumerations and ranges are checked by the client where the value is read
//! (`static/settings.js`), falling back to the default.
//!
//! The file holds only what differs from the defaults, so a default changed
//! in a later release reaches every library that never touched that key.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub(crate) const DEFAULTS_JSON: &str = include_str!("../../../static/gallery-defaults.json");

pub(crate) const FILE_NAME: &str = "gallery.json";

/// A write whose file would exceed this is refused. Settings are a few
/// hundred bytes; anything near this is not settings.
pub(crate) const MAX_BYTES: usize = 64 * 1024;

pub(crate) fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

pub(crate) enum Stored {
    Absent,
    Valid(Value),
    /// Present but not a readable JSON object. Never overwritten: it may hold
    /// hand edits the user wants back.
    Invalid(String),
}

pub(crate) fn load(path: &Path) -> Stored {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stored::Absent,
        Err(e) => Stored::Invalid(format!("cannot read {}: {e}", path.display())),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v @ Value::Object(_)) => Stored::Valid(v),
            Ok(_) => Stored::Invalid(format!("{} is not a JSON object", path.display())),
            Err(e) => Stored::Invalid(format!("{} is not valid JSON: {e}", path.display())),
        },
    }
}

pub(crate) enum SaveError {
    TooLarge,
    Io(anyhow::Error),
}

pub(crate) fn save(path: &Path, overrides: &Value) -> Result<(), SaveError> {
    let mut text = serde_json::to_string_pretty(overrides).expect("a Value serializes");
    text.push('\n');
    if text.len() > MAX_BYTES {
        return Err(SaveError::TooLarge);
    }
    videre_core::atomic_file::publish(path, |file| {
        use std::io::Write;
        file.write_all(text.as_bytes())?;
        Ok(())
    })
    .map_err(SaveError::Io)
}

/// Everything a page or `GET /api/settings` needs, read from disk each time
/// so a hand edit applies on the next load with no restart.
pub(crate) struct Snapshot {
    pub effective: Value,
    pub overrides: Value,
    pub ignored: Vec<String>,
    /// Why the file could not be used, when it exists but is unreadable.
    pub error: Option<String>,
    pub path: PathBuf,
}

pub(crate) fn snapshot(path: &Path) -> Snapshot {
    let (overrides, error) = match load(path) {
        Stored::Absent => (Value::Object(Map::new()), None),
        Stored::Valid(v) => (v, None),
        Stored::Invalid(e) => (Value::Object(Map::new()), Some(e)),
    };
    let Merged { effective, ignored } = merge(&defaults(), &overrides);
    Snapshot {
        effective,
        overrides,
        ignored,
        error,
        path: path.to_path_buf(),
    }
}

pub(crate) const SETTINGS_JS: &str = include_str!("../../../static/settings.js");

/// The settings as page globals plus the client runtime, inlined by
/// `templates/nav.html` so the first paint already uses them. `live` is false
/// on a static export, where changes last the session and are not saved.
/// Every `<` is written as `\u003c`, so no value can reach the HTML parser: not
/// `</script>`, and not `<!--<script>`, which leaves the element open and
/// swallows the rest of the page. A value can be anything a user imported.
pub(crate) fn page_script(s: &Snapshot, live: bool) -> String {
    let js = |v: &Value| v.to_string().replace('<', "\\u003c");
    let error = s.error.clone().map(Value::String).unwrap_or(Value::Null);
    format!(
        "<script>var VIDERE_SETTINGS={};var VIDERE_SETTINGS_DEFAULTS={};\
         var VIDERE_SETTINGS_LIVE={live};var VIDERE_SETTINGS_ERROR={};</script>\
         <script>{SETTINGS_JS}</script>",
        js(&s.effective),
        js(&defaults()),
        js(&error),
    )
}

/// `page_script` for the library whose state directory is `state_dir`.
pub(crate) fn page_script_for(state_dir: &Path, live: bool) -> String {
    page_script(&snapshot(&path(state_dir)), live)
}

pub(crate) fn defaults() -> Value {
    serde_json::from_str(DEFAULTS_JSON).expect("gallery-defaults.json is valid JSON")
}

pub(crate) struct Merged {
    pub effective: Value,
    /// Dotted paths of overrides whose type did not match the default.
    pub ignored: Vec<String>,
}

/// The defaults with `overrides` merged on top. Objects merge key by key; any
/// other value replaces the default only when its JSON type matches, and is
/// otherwise ignored and reported. A key the defaults do not declare passes
/// through, so a user can add keys; nothing reads them until code does.
pub(crate) fn merge(defaults: &Value, overrides: &Value) -> Merged {
    let mut effective = defaults.clone();
    let mut ignored = Vec::new();
    merge_into(&mut effective, overrides, "", &mut ignored);
    ignored.sort();
    Merged { effective, ignored }
}

fn merge_into(base: &mut Value, over: &Value, path: &str, ignored: &mut Vec<String>) {
    if let (Value::Object(b), Value::Object(o)) = (&mut *base, over) {
        for (k, v) in o {
            let child = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            match b.get_mut(k) {
                None => {
                    b.insert(k.clone(), v.clone());
                }
                Some(slot) => merge_into(slot, v, &child, ignored),
            }
        }
    } else if same_type(base, over) {
        *base = over.clone();
    } else {
        ignored.push(path.to_string());
    }
}

/// Integer and float are one type, since JSON has one number type. Objects
/// are handled by the caller, so they never match here.
fn same_type(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Number(_), Value::Number(_))
            | (Value::String(_), Value::String(_))
            | (Value::Bool(_), Value::Bool(_))
            | (Value::Array(_), Value::Array(_))
    )
}

/// RFC 7396 JSON merge patch: `null` removes a key, objects merge, anything
/// else replaces.
pub(crate) fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(p) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let t = target.as_object_mut().expect("just made an object");
    for (k, v) in p {
        if v.is_null() {
            t.remove(k);
        } else {
            merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
        }
    }
}

/// Drop every override equal to its default, and any object left empty by
/// that, so the file stays sparse. Without this, choosing Tile again after List
/// stored `"view":"tile"`, and a later release changing the default would
/// never reach that library. Undeclared keys have no default and are kept.
pub(crate) fn prune_defaults(overrides: &mut Value, defaults: &Value) {
    let (Value::Object(o), Value::Object(d)) = (overrides, defaults) else {
        return;
    };
    o.retain(|key, value| {
        let Some(default) = d.get(key) else {
            return true;
        };
        if value.is_object() && default.is_object() {
            prune_defaults(value, default);
            return !value.as_object().is_some_and(Map::is_empty);
        }
        !same_value(value, default)
    });
}

/// Equality with numbers compared by value, so `200.0` equals a default of
/// `200` the way `merge` treats them as one type.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

const RESUME_MAX: usize = 2048;

/// Paths that are not pages. `/settings` is refused separately: it is a
/// detour, not a place to come back to.
const NOT_RESUMABLE: &[&str] = &["/api/", "/tiles/", "/vendor/"];

/// The route to reopen the gallery at: the saved one when it is a safe local
/// page, otherwise `/`. The client saves whatever it is on; this is the one
/// place that decides whether it is fit to hand to a browser.
pub(crate) fn resume_route(effective: &Value) -> String {
    let route = effective["resume"]["route"].as_str().unwrap_or("/");
    let path = route.split('?').next().unwrap_or("");
    let ok = route.len() <= RESUME_MAX
        && route.starts_with('/')
        && !route.starts_with("//")
        && !route.contains('\\')
        && path != "/settings"
        && !NOT_RESUMABLE.iter().any(|p| route.starts_with(p));
    if ok {
        route.to_string()
    } else {
        "/".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn embedded_defaults_parse_and_hold_no_nulls() {
        fn walk(v: &Value, path: &str) {
            match v {
                Value::Null => panic!("null default at {path}"),
                Value::Object(m) => m.iter().for_each(|(k, v)| walk(v, &format!("{path}.{k}"))),
                _ => {}
            }
        }
        walk(&defaults(), "");
    }

    #[test]
    fn nested_objects_merge_and_scalars_replace() {
        let m = merge(
            &defaults(),
            &json!({"routes": {"files": {"tile": {"rowHeight": 320}}}}),
        );
        assert_eq!(m.effective["routes"]["files"]["tile"]["rowHeight"], 320);
        assert_eq!(m.effective["routes"]["files"]["tile"]["colGap"], 10);
        assert_eq!(m.effective["routes"]["files"]["view"], "tile");
        assert!(m.ignored.is_empty());
    }

    #[test]
    fn a_mismatched_type_is_ignored_and_reported() {
        let m = merge(
            &defaults(),
            &json!({"routes": {"files": {"pageSize": "lots", "view": 3}}}),
        );
        assert_eq!(m.effective["routes"]["files"]["pageSize"], 200);
        assert_eq!(m.effective["routes"]["files"]["view"], "tile");
        assert_eq!(
            m.ignored,
            vec!["routes.files.pageSize", "routes.files.view"]
        );
    }

    #[test]
    fn integer_and_float_are_one_type() {
        let m = merge(&defaults(), &json!({"routes": {"map": {"radiusKm": 12.5}}}));
        assert_eq!(m.effective["routes"]["map"]["radiusKm"], 12.5);
        assert!(m.ignored.is_empty());
    }

    #[test]
    fn an_undeclared_key_passes_through() {
        let m = merge(
            &defaults(),
            &json!({"routes": {"files": {"mine": true}}, "extra": 1}),
        );
        assert_eq!(m.effective["routes"]["files"]["mine"], true);
        assert_eq!(m.effective["extra"], 1);
        assert!(m.ignored.is_empty());
    }

    #[test]
    fn an_object_over_a_scalar_is_ignored_and_a_scalar_over_an_object_too() {
        let m = merge(
            &defaults(),
            &json!({"routes": {"files": {"view": {"x": 1}, "tile": 5}}}),
        );
        assert_eq!(m.effective["routes"]["files"]["view"], "tile");
        assert_eq!(m.effective["routes"]["files"]["tile"]["rowHeight"], 280);
        assert_eq!(m.ignored, vec!["routes.files.tile", "routes.files.view"]);
    }

    #[test]
    fn merge_patch_null_removes_and_untouched_keys_stay() {
        let mut t = json!({"routes": {"files": {"view": "list", "pageSize": 50}}});
        merge_patch(&mut t, &json!({"routes": {"files": {"view": null}}}));
        assert_eq!(t, json!({"routes": {"files": {"pageSize": 50}}}));
        merge_patch(&mut t, &json!({"resume": {"route": "/map"}}));
        assert_eq!(t["routes"]["files"]["pageSize"], 50);
        assert_eq!(t["resume"]["route"], "/map");
    }

    #[test]
    fn the_page_script_carries_the_settings_and_cannot_be_closed_by_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        std::fs::write(
            &p,
            r#"{"routes":{"files":{"view":"</script><b>"}},"mine":"<!--<script>"}"#,
        )
        .unwrap();
        let live = page_script_for(dir.path(), true);
        assert!(live.starts_with("<script>var VIDERE_SETTINGS={"), "{live}");
        assert!(live.ends_with("</script>"));
        assert!(live.contains("VIDERE_SETTINGS_LIVE=true"));
        // Not only `</`: `<!--<script>` inside a script element sends the HTML
        // parser into its escaped state and swallows the rest of the page. No
        // `<` from a value may reach the page at all.
        let data = live
            .split("</script>")
            .next()
            .unwrap()
            .trim_start_matches("<script>");
        assert!(!data.contains('<'), "{data}");
        assert!(data.contains(r"\u003c!--\u003cscript>"), "{data}");
        assert!(page_script_for(dir.path(), false).contains("VIDERE_SETTINGS_LIVE=false"));
    }

    #[test]
    fn values_equal_to_their_default_are_pruned() {
        let mut o = json!({
            "resume": {"route": "/"},
            "routes": {
                "files": {"view": "tile", "pageSize": 200.0, "tile": {"rowHeight": 320, "colGap": 10}},
                "people": {"align": "right"},
            },
            "mine": "kept",
        });
        prune_defaults(&mut o, &defaults());
        // Equal values go, including 200.0 against 200; objects left empty go
        // with them; a value that differs and an undeclared key stay.
        assert_eq!(
            o,
            json!({"routes": {"files": {"tile": {"rowHeight": 320}}}, "mine": "kept"})
        );
    }

    #[test]
    fn load_tells_absent_valid_and_invalid_apart() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        assert!(matches!(load(&p), Stored::Absent));
        std::fs::write(&p, r#"{"routes":{}}"#).unwrap();
        assert!(matches!(load(&p), Stored::Valid(_)));
        std::fs::write(&p, "not json").unwrap();
        assert!(matches!(load(&p), Stored::Invalid(m) if m.contains("not valid JSON")));
        std::fs::write(&p, "[1]").unwrap();
        assert!(matches!(load(&p), Stored::Invalid(m) if m.contains("not a JSON object")));
    }

    #[test]
    fn save_round_trips_as_pretty_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(&dir.path().join(".videre"));
        let v = json!({"routes": {"files": {"view": "list"}}});
        assert!(save(&p, &v).is_ok());
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.ends_with("}\n"), "{text}");
        assert!(text.contains("\n  \"routes\""), "{text}");
        assert!(matches!(load(&p), Stored::Valid(back) if back == v));
    }

    #[test]
    fn an_oversized_save_is_refused_and_leaves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        std::fs::write(&p, "{}\n").unwrap();
        let big = json!({"x": "a".repeat(MAX_BYTES)});
        assert!(matches!(save(&p, &big), Err(SaveError::TooLarge)));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{}\n");
    }

    #[test]
    fn a_snapshot_of_an_invalid_file_is_the_defaults_with_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        std::fs::write(&p, "{oops").unwrap();
        let s = snapshot(&p);
        assert_eq!(s.effective, defaults());
        assert_eq!(s.overrides, json!({}));
        assert!(s.error.is_some());
    }

    #[test]
    fn resume_route_keeps_safe_routes_and_refuses_the_rest() {
        let with = |r: &str| merge(&defaults(), &json!({"resume": {"route": r}})).effective;
        assert_eq!(
            resume_route(&with("/map/location/Kadıköy?radius=5")),
            "/map/location/Kadıköy?radius=5"
        );
        assert_eq!(resume_route(&with("/date/2021/08")), "/date/2021/08");
        for bad in [
            "//evil.example",
            "/api/files",
            "/tiles/basemap.pmtiles",
            "/vendor/1/x.js",
            "/settings",
            "/settings?x=1",
            "map",
            "/\\evil",
            "",
        ] {
            assert_eq!(resume_route(&with(bad)), "/", "{bad}");
        }
        assert_eq!(resume_route(&with(&format!("/{}", "a".repeat(2048)))), "/");
    }
}
