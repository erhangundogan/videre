use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use videre::types::{ErrorJson, SCHEMA_VERSION};
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, search};
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct SearchArgs {
    /// SQLite database with embeddings (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Text query, e.g. "sunset on beach" (omit when using --image)
    query: Option<String>,

    /// Search by example image instead of text
    #[arg(long, conflicts_with = "query")]
    image: Option<PathBuf>,

    /// Return paths containing a named person (confirmed faces only)
    #[arg(long, conflicts_with = "query", conflicts_with = "image")]
    person: Option<String>,

    /// Number of results
    #[arg(short = 'k', long, default_value_t = 20)]
    top_k: usize,

    /// Prepend the cosine score to each output line (no-op with --json: score is always included)
    #[arg(long)]
    scores: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchJson {
    pub(crate) schema_version: u32,
    pub(crate) query: QueryJson,
    pub(crate) count: usize,
    pub(crate) results: Vec<SearchHitJson>,
}

#[derive(Debug, Serialize)]
pub(crate) struct QueryJson {
    pub(crate) kind: &'static str,
    pub(crate) value: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchHitJson {
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) score: Option<f32>,
}

pub fn run(args: SearchArgs) -> Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                // stdout must always carry exactly one valid JSON object; the
                // error goes here (not stderr) and we exit before main's eprintln.
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                std::process::exit(1);
            }
        }
    } else {
        run_text(&args)
    }
}

fn run_text(args: &SearchArgs) -> Result<()> {
    let (_query, hits) = collect_hits(args)?;
    for hit in hits {
        match hit.score {
            Some(score) if args.scores => println!("{score:.4}\t{}", hit.path),
            _ => println!("{}", hit.path),
        }
    }
    Ok(())
}

fn run_json(args: &SearchArgs) -> Result<SearchJson> {
    let (query, results) = collect_hits(args)?;
    Ok(SearchJson {
        schema_version: SCHEMA_VERSION,
        query,
        count: results.len(),
        results,
    })
}

/// Person query: bare paths, no hash/score (confirmed faces only).
pub(crate) fn person_hits(conn: &Connection, name: &str) -> Result<Vec<SearchHitJson>> {
    let paths = videre_core::person_search::search_by_person(conn, name, None)?;
    Ok(paths
        .into_iter()
        .map(|path| SearchHitJson { path, hash: None, score: None })
        .collect())
}

/// Load the embedding corpus, erroring if empty. Called BEFORE any model load
/// so a db without embeddings fails fast without downloading weights.
pub(crate) fn load_corpus(conn: &Connection, db: &Path) -> Result<Vec<(String, Vec<f32>)>> {
    let corpus_raw = embeddings::load_embeddings(conn, model::MODEL_ID)?;
    anyhow::ensure!(
        !corpus_raw.is_empty(),
        "no embeddings found in {} for model {}; run videre embed first",
        db.display(),
        model::MODEL_ID
    );
    Ok(corpus_raw
        .into_iter()
        .map(|(hash, blob)| (hash, vectors::from_f16_bytes(&blob)))
        .collect())
}

/// Rank the corpus against a query vector; one hit per on-disk path of each
/// matched hash, carrying hash + cosine score.
pub(crate) fn ranked_hits(
    conn: &Connection,
    corpus: &[(String, Vec<f32>)],
    query_vec: &[f32],
    top_k: usize,
) -> Result<Vec<SearchHitJson>> {
    let mut hits = Vec::new();
    for (hash, score) in search::top_k(query_vec, corpus, top_k) {
        for path in embeddings::paths_for_hash(conn, &hash)? {
            hits.push(SearchHitJson {
                path,
                hash: Some(hash.clone()),
                score: Some(score),
            });
        }
    }
    Ok(hits)
}

/// The single query pipeline behind both output modes. Person hits carry only
/// a path (person search returns bare paths, no hash/score); text and image
/// hits carry hash + cosine score, one entry per on-disk path of a matched hash.
fn collect_hits(args: &SearchArgs) -> Result<(QueryJson, Vec<SearchHitJson>)> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)
        .with_context(|| format!("open {}", db.display()))?;

    if let Some(name) = &args.person {
        let hits = person_hits(&conn, name)?;
        if hits.is_empty() && !args.json {
            // In --json mode the empty result is conveyed as count 0; keep stdout
            // the only channel so a clean agent invocation emits nothing on stderr.
            eprintln!("No confirmed photos found for person: {name}");
        }
        return Ok((QueryJson { kind: "person", value: name.clone() }, hits));
    }

    let corpus = load_corpus(&conn, &db)?;

    let embedder = model::Embedder::load(device::best_device())?;
    let (query_vec, query) = match (&args.query, &args.image) {
        (Some(text), None) => (
            embedder.embed_text(text)?,
            QueryJson { kind: "text", value: text.clone() },
        ),
        (None, Some(img)) => (
            model::embed_image_file(&embedder, img)?,
            QueryJson { kind: "image", value: img.display().to_string() },
        ),
        _ => anyhow::bail!("provide either a text query or --image <path>"),
    };

    let hits = ranked_hits(&conn, &corpus, &query_vec, args.top_k)?;
    Ok((query, hits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_hit_serializes_with_hash_and_score() {
        let doc = SearchJson {
            schema_version: SCHEMA_VERSION,
            query: QueryJson { kind: "text", value: "sunset".to_string() },
            count: 1,
            results: vec![SearchHitJson {
                path: "/a.jpg".to_string(),
                hash: Some("abc".to_string()),
                score: Some(0.5),
            }],
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(json.contains("\"kind\":\"text\""));
        assert!(json.contains("\"hash\":\"abc\""));
        assert!(json.contains("\"score\":0.5"));
        assert!(json.contains("\"count\":1"));
    }

    #[test]
    fn person_hit_omits_hash_and_score_keys() {
        let doc = SearchJson {
            schema_version: SCHEMA_VERSION,
            query: QueryJson { kind: "person", value: "Alice".to_string() },
            count: 1,
            results: vec![SearchHitJson {
                path: "/a.jpg".to_string(),
                hash: None,
                score: None,
            }],
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(!json.contains("hash"));
        assert!(!json.contains("score"));
        assert!(json.contains("\"path\":\"/a.jpg\""));
    }
}
