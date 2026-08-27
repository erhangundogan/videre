//! The gallery's HTTP endpoints have one source of truth, and the docs may not
//! name a route that does not exist.
//!
//! `src/commands/gallery_endpoints.json` is the manifest. Two things keep it
//! honest, so a change on either side that is not mirrored fails the build:
//!
//! 1. `manifest_matches_the_router` parses every `.route(...)` in
//!    `commands/report.rs` (the actual router, not the test fixtures - the
//!    `gallery_routes.rs` fixtures deliberately assert some paths 404) and asserts
//!    the set is exactly the manifest. Add, remove or move a route and this fails
//!    until the JSON is updated.
//! 2. `docs_do_not_name_a_missing_route` scans the docs for gallery routes and
//!    asserts each one exists in the manifest. A doc naming `/cluster/1` fails:
//!    the gallery serves it at `/people/cluster/1`. That is the exact 0.20.5
//!    regression this guards against.

use serde::Deserialize;
use std::collections::BTreeSet;

const MANIFEST: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/commands/gallery_endpoints.json"
);
const REPORT_RS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands/report.rs");

#[derive(Deserialize)]
struct Manifest {
    endpoints: Vec<Endpoint>,
}

#[derive(Deserialize, Clone)]
struct Endpoint {
    method: String,
    path: String,
}

fn manifest() -> Vec<Endpoint> {
    let text = std::fs::read_to_string(MANIFEST).expect("read gallery_endpoints.json");
    serde_json::from_str::<Manifest>(&text)
        .expect("parse gallery_endpoints.json")
        .endpoints
}

/// Normalise a path to method-independent segments, params as `{}`.
fn norm_path(p: &str) -> String {
    let inner: Vec<String> = p
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('{') && s.ends_with('}') {
                "{}".to_string()
            } else {
                s.to_string()
            }
        })
        .collect();
    if inner.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", inner.join("/"))
    }
}

/// A `(METHOD, normalised-path)` pair, the unit of comparison.
type Endpt = (String, String);

fn manifest_set() -> BTreeSet<Endpt> {
    manifest()
        .into_iter()
        .map(|e| (e.method.to_uppercase(), norm_path(&e.path)))
        .collect()
}

/// Parse the `(METHOD, path)` set the router registers, from the literal
/// `.route("/x", get(..))` calls in report.rs. `videre gallery` is the only
/// server configuration, so every route is a plain literal.
fn router_set() -> BTreeSet<Endpt> {
    let src = std::fs::read_to_string(REPORT_RS).expect("read report.rs");
    let mut out: BTreeSet<Endpt> = BTreeSet::new();
    for piece in src.split(".route(").skip(1) {
        let p = piece.trim_start();
        let method = first_method(p);
        if let Some(rest) = p.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                let path = &rest[..end];
                if path.starts_with('/') {
                    out.insert((method, norm_path(path)));
                }
            }
        }
    }
    out
}

/// The first axum method constructor in a `.route(...)` piece.
fn first_method(piece: &str) -> String {
    let cands = [
        ("get(", "GET"),
        ("post(", "POST"),
        ("put(", "PUT"),
        ("delete(", "DELETE"),
        ("patch(", "PATCH"),
    ];
    cands
        .iter()
        .filter_map(|(needle, m)| piece.find(needle).map(|i| (i, *m)))
        .min_by_key(|(i, _)| *i)
        .map(|(_, m)| m.to_string())
        .unwrap_or_else(|| "GET".to_string())
}

#[test]
fn manifest_matches_the_router() {
    let manifest = manifest_set();
    let router = router_set();

    let missing_from_manifest: Vec<_> = router.difference(&manifest).collect();
    let missing_from_router: Vec<_> = manifest.difference(&router).collect();

    assert!(
        missing_from_manifest.is_empty() && missing_from_router.is_empty(),
        "gallery_endpoints.json is out of sync with report.rs's .route(...) calls.\n\
         In report.rs but not the manifest (add them):\n{}\n\
         In the manifest but not report.rs (remove them):\n{}",
        fmt(&missing_from_manifest),
        fmt(&missing_from_router),
    );
}

fn fmt(v: &[&Endpt]) -> String {
    if v.is_empty() {
        "  (none)".to_string()
    } else {
        v.iter()
            .map(|(m, p)| format!("  {m} {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// --- docs guard ---

/// Gallery-route stems a documented `/`-path may start with. Anything else
/// (docs-site links `/commands/...`, filesystem paths) is not a gallery route.
/// `cluster`/`person` are here so a bare `/cluster/1` is caught as the moved path.
const ROUTE_STEMS: &[&str] = &[
    "api",
    "people",
    "duplicates",
    "date",
    "map",
    "events",
    "smart",
    "faces",
    "cluster",
    "person",
];

fn doc_route_tokens(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in text.split(|c: char| c.is_whitespace() || c == '`' || c == '(' || c == ')') {
        let tok = raw.trim_matches(|c: char| !(c.is_alphanumeric() || "/{}._-".contains(c)));
        let tok = tok.split('?').next().unwrap_or(tok);
        let Some(first) = tok.strip_prefix('/').and_then(|r| r.split('/').next()) else {
            continue;
        };
        if ROUTE_STEMS.contains(&first) {
            out.insert(norm_path(tok));
        }
    }
    out
}

fn matches(pattern: &str, doc: &str) -> bool {
    let (a, b): (Vec<&str>, Vec<&str>) = (
        pattern.trim_matches('/').split('/').collect(),
        doc.trim_matches('/').split('/').collect(),
    );
    a.len() == b.len() && a.iter().zip(&b).all(|(p, s)| *p == "{}" || p == s)
}

#[test]
fn docs_do_not_name_a_missing_route() {
    let routes: Vec<String> = manifest().into_iter().map(|e| norm_path(&e.path)).collect();

    let docs_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/src/content/docs");
    let mut offenders: Vec<(String, String)> = Vec::new();
    for file in walk_markdown(std::path::Path::new(docs_dir)) {
        let text = std::fs::read_to_string(&file).unwrap();
        for tok in doc_route_tokens(&text) {
            if !routes.iter().any(|g| matches(g, &tok)) {
                offenders.push((
                    file.strip_prefix(docs_dir)
                        .unwrap_or(&file)
                        .display()
                        .to_string(),
                    tok,
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "docs name gallery routes that `videre gallery` does not serve:\n{}\n\n\
         Valid routes are the gallery entries in src/commands/gallery_endpoints.json \
         (cluster/person live under /people/). Fix the doc, or add the route.",
        offenders
            .iter()
            .map(|(f, p)| format!("  {f}: {p}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

fn walk_markdown(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("md") | Some("mdx")
            ) {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn norm_collapses_params_and_root() {
        assert_eq!(norm_path("/people/person/{name}"), "/people/person/{}");
        assert_eq!(norm_path("/"), "/");
        assert_eq!(norm_path("/api/files/"), "/api/files");
    }

    #[test]
    fn matches_param_against_concrete() {
        assert!(matches("/people/person/{}", "/people/person/isil"));
        assert!(!matches("/people/person/{}", "/people/person"));
    }

    #[test]
    fn moved_bare_cluster_is_not_a_route() {
        // The 0.20.5 regression: /cluster/1 moved to /people/cluster/1.
        let routes: Vec<String> = manifest().into_iter().map(|e| norm_path(&e.path)).collect();
        assert!(!routes.iter().any(|g| matches(g, &norm_path("/cluster/1"))));
        assert!(routes
            .iter()
            .any(|g| matches(g, &norm_path("/people/cluster/1"))));
    }

    #[test]
    fn docs_site_links_are_ignored() {
        let toks = doc_route_tokens("[x](/commands/mark/) /guides/y `/api/mark`");
        assert_eq!(toks, BTreeSet::from(["/api/mark".to_string()]));
    }
}
