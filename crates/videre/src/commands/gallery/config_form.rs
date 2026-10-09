//! `/settings/config`: the library's `videre config` keys as a form. The
//! keys, their limits and when a change applies come from
//! `library_config::KEYS`; this module turns them into JSON for the page and
//! a submitted change back into config values.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use videre_core::library_config::{self, Applies, ConfigKey, Kind, LibraryConfig, KEYS};

fn kind_json(kind: Kind) -> Value {
    match kind {
        Kind::Bool => json!({"type": "bool"}),
        Kind::Int { min, max } => json!({"type": "int", "min": min, "max": max}),
        Kind::Number { min, max } => json!({"type": "number", "min": min, "max": max}),
        Kind::Choice(options) => json!({"type": "enum", "options": options}),
        Kind::Model => json!({"type": "model"}),
    }
}

fn applies_str(applies: Applies) -> &'static str {
    match applies {
        Applies::Now => "now",
        Applies::NextRun => "next-run",
        Applies::NextStart => "next-start",
    }
}

/// Each key's value in `config`, by CLI name.
pub(crate) fn values_json(config: &LibraryConfig) -> Map<String, Value> {
    KEYS.iter()
        .map(|s| (s.cli.to_string(), library_config::value_json(config, s.key)))
        .collect()
}

/// What the page needs: every key's description, its value and its
/// built-in default, or why the config could not be read.
pub(crate) fn page_json(loaded: anyhow::Result<LibraryConfig>) -> Value {
    let defaults = LibraryConfig::default();
    let (config, error) = match loaded {
        Ok(c) => (c, Value::Null),
        Err(e) => (defaults.clone(), json!(format!("{e:#}"))),
    };
    let keys: Vec<Value> = KEYS
        .iter()
        .map(|s| {
            json!({
                "cli": s.cli,
                "label": s.label,
                "hint": s.hint,
                "kind": kind_json(s.kind),
                "applies": applies_str(s.applies),
                "optional": s.optional,
                "value": library_config::value_json(&config, s.key),
                "default": library_config::value_json(&defaults, s.key),
            })
        })
        .collect();
    json!({"keys": keys, "error": error})
}

/// The message a value gets when its key refuses it, as the page shows it.
fn refusal(kind: Kind, value: &Value) -> Option<String> {
    let range = |min: String, max: Option<String>| match max {
        Some(max) => format!(" from {min} to {max}"),
        None => format!(" of at least {min}"),
    };
    match kind {
        Kind::Bool => (!value.is_boolean()).then(|| "Choose on or off".into()),
        Kind::Int { min, max } => {
            let ok = value
                .as_i64()
                .is_some_and(|n| n >= min && max.is_none_or(|m| n <= m));
            (!ok).then(|| {
                format!(
                    "Enter a whole number{}",
                    range(min.to_string(), max.map(|m| m.to_string()))
                )
            })
        }
        Kind::Number { min, max } => {
            let ok = value
                .as_f64()
                .is_some_and(|n| n.is_finite() && n >= min && n <= max);
            (!ok).then(|| {
                format!(
                    "Enter a number{}",
                    range(min.to_string(), Some(max.to_string()))
                )
            })
        }
        Kind::Choice(options) => {
            let ok = value.as_str().is_some_and(|s| options.contains(&s));
            (!ok).then(|| format!("Choose one of {}", options.join(", ")))
        }
        Kind::Model => {
            let ok = value
                .as_str()
                .is_some_and(|s| videre_core::embeddings::validate_model_id(s).is_ok());
            (!ok).then(|| "Enter a model as owner/name".into())
        }
    }
}

fn toml_value(kind: Kind, value: &Value) -> toml::Value {
    match kind {
        Kind::Bool => toml::Value::Boolean(value.as_bool().expect("checked")),
        Kind::Int { .. } => toml::Value::Integer(value.as_i64().expect("checked")),
        Kind::Number { .. } => toml::Value::Float(value.as_f64().expect("checked")),
        Kind::Choice(_) | Kind::Model => {
            toml::Value::String(value.as_str().expect("checked").into())
        }
    }
}

/// A submitted change, `{cli: value | null}`, as config edits; `null`
/// unsets a key. Every refused key is named with its message.
pub(crate) type Changes = Vec<(ConfigKey, Option<toml::Value>)>;

pub(crate) fn parse(body: &Value) -> Result<Changes, BTreeMap<String, String>> {
    let mut errors = BTreeMap::new();
    let mut changes = Vec::new();
    let Some(fields) = body.as_object() else {
        errors.insert(String::new(), "Send an object of settings".into());
        return Err(errors);
    };
    for (cli, value) in fields {
        let Some(spec) = library_config::spec_for(cli) else {
            errors.insert(cli.clone(), "Not a setting".into());
            continue;
        };
        if value.is_null() {
            changes.push((spec.key, None));
        } else if let Some(message) = refusal(spec.kind, value) {
            errors.insert(cli.clone(), message);
        } else {
            changes.push((spec.key, Some(toml_value(spec.kind, value))));
        }
    }
    if errors.is_empty() {
        Ok(changes)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_is_described_with_its_value_and_default() {
        let page = page_json(Ok(LibraryConfig::default()));
        let keys = page["keys"].as_array().unwrap();
        assert_eq!(keys.len(), KEYS.len());
        let run = keys.iter().find(|k| k["cli"] == "run-history").unwrap();
        assert_eq!(run["kind"], json!({"type": "int", "min": 1, "max": 100}));
        assert_eq!(
            (run["value"].clone(), run["default"].clone()),
            (json!(3), json!(3))
        );
        let street = keys.iter().find(|k| k["cli"] == "street-detail").unwrap();
        assert_eq!(street["applies"], "now");
        assert!(street["hint"].as_str().unwrap().contains("deletes"));
    }

    #[test]
    fn a_change_is_parsed_or_each_bad_key_named() {
        let ok = parse(&json!({"log-level": "debug", "similar-min-score": null})).unwrap();
        assert_eq!(ok.len(), 2);
        let bad = parse(&json!({
            "log-level": "çok",
            "search-min-match": 1.5,
            "model": "siglip",
            "street-detail": "evet"
        }))
        .unwrap_err();
        assert_eq!(bad["log-level"], "Choose one of error, warn, info, debug");
        assert_eq!(bad["search-min-match"], "Enter a number from 0 to 1");
        assert_eq!(bad["model"], "Enter a model as owner/name");
        assert_eq!(bad["street-detail"], "Choose on or off");
    }
}
