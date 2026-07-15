use anyhow::{Context, Result};
use serde::Serialize;
use videre::types::{ErrorJson, SCHEMA_VERSION};
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, search};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct SearchArgs {
    /// SQLite database with embeddings (run videre embed first)
    db: PathBuf,

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
struct SearchJson {
    schema_version: u32,
    query: QueryJson,
    count: usize,
    results: Vec<SearchHitJson>,
}

#[derive(Debug, Serialize)]
struct QueryJson {
    kind: &'static str,
    value: String,
}

#[derive(Debug, Serialize)]
struct SearchHitJson {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f32>,
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

/// The single query pipeline behind both output modes. Person hits carry only
/// a path (person search returns bare paths, no hash/score); text and image
/// hits carry hash + cosine score, one entry per on-disk path of a matched hash.
fn collect_hits(args: &SearchArgs) -> Result<(QueryJson, Vec<SearchHitJson>)> {
    let conn = videre_core::db::open_wal(&args.db)
        .with_context(|| format!("open {}", args.db.display()))?;

    if let Some(name) = &args.person {
        let paths = videre_core::person_search::search_by_person(&conn, name, None)?;
        if paths.is_empty() && !args.json {
            // In --json mode the empty result is conveyed as count 0; keep stdout
            // the only channel so a clean agent invocation emits nothing on stderr.
            eprintln!("No confirmed photos found for person: {name}");
        }
        let hits = paths
            .into_iter()
            .map(|path| SearchHitJson { path, hash: None, score: None })
            .collect();
        return Ok((QueryJson { kind: "person", value: name.clone() }, hits));
    }

    let corpus_raw = embeddings::load_embeddings(&conn, model::MODEL_ID)?;
    anyhow::ensure!(
        !corpus_raw.is_empty(),
        "no embeddings found in {} for model {}; run videre embed first",
        args.db.display(),
        model::MODEL_ID
    );
    let corpus: Vec<(String, Vec<f32>)> = corpus_raw
        .into_iter()
        .map(|(hash, blob)| (hash, vectors::from_f16_bytes(&blob)))
        .collect();

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

    let mut hits = Vec::new();
    for (hash, score) in search::top_k(&query_vec, &corpus, args.top_k) {
        for path in embeddings::paths_for_hash(&conn, &hash)? {
            hits.push(SearchHitJson {
                path,
                hash: Some(hash.clone()),
                score: Some(score),
            });
        }
    }
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
