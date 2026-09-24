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

pub(crate) const DEFAULTS_JSON: &str = include_str!("../../../static/gallery-defaults.json");

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
                Value::Object(m) => m
                    .iter()
                    .for_each(|(k, v)| walk(v, &format!("{path}.{k}"))),
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
        assert_eq!(m.ignored, vec!["routes.files.pageSize", "routes.files.view"]);
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
