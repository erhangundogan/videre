use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use videre::types::{ErrorJson, SCHEMA_VERSION};
use videre_core::query::{self, Candidates, Filters, SortField, SortKey, Sortable};
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, search};

#[derive(clap::Args)]
pub struct SearchArgs {
    /// SQLite database with embeddings (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Embedding model to search against (default: 'videre config set model', else
    /// the built-in default). Must already have been embedded; run
    /// 'videre stats' to see which models this library has.
    #[arg(long)]
    model: Option<String>,

    /// Text query, e.g. "sunset on beach" (omit when using --image)
    query: Option<String>,

    /// Search by example image instead of text
    #[arg(long, conflicts_with = "query")]
    image: Option<PathBuf>,

    /// Only files containing a named person (confirmed faces only)
    #[arg(long)]
    person: Option<String>,

    /// Only files classified as this category: photo/screenshot/document/
    /// meme/unknown (requires a prior 'videre classify' run)
    #[arg(long)]
    category: Option<String>,

    /// Only photos within --radius km of this place, e.g. "Berlin, Germany"
    /// (forward-geocoded via the free public Nominatim API, the first
    /// network call this CLI ever makes; results are cached locally)
    #[arg(long)]
    location: Option<String>,

    /// Search radius in km around --location
    #[arg(long, default_value_t = 20.0, requires = "location")]
    radius: f64,

    /// Only files whose date is on or after this (inclusive).
    /// Accepts YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS.
    #[arg(long, conflicts_with = "date")]
    after: Option<String>,

    /// Only files whose date is before this (exclusive), so adjacent ranges
    /// do not both match the boundary instant.
    #[arg(long, conflicts_with = "date")]
    before: Option<String>,

    /// Shorthand for a whole year, month, or day: YYYY, YYYY-MM, or YYYY-MM-DD
    #[arg(long)]
    date: Option<String>,

    /// Result order: comma-separated field[:asc|desc]. Fields: relevance,
    /// distance, date, size. Defaults are relevance/date/size descending and
    /// distance ascending.
    #[arg(long)]
    sort: Option<String>,

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) distance_km: Option<f64>,
    /// The effective date: EXIF capture date when present and valid, else the
    /// filesystem mtime. Absent when the row has neither.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) date: Option<String>,
}

/// One finished query: the survivors in their final order, plus what the
/// caller needs to render them.
struct Outcome {
    query: QueryJson,
    /// Already filtered, sorted and truncated. Carries every sortable field,
    /// which is what lets `--scores` prepend whichever one drove the order.
    rows: Vec<Sortable>,
    /// path -> content hash. Empty for a person query, whose hits have always
    /// been bare paths.
    hashes: HashMap<String, String>,
    /// The field `--scores` prepends in text mode.
    primary: SortField,
}

impl Outcome {
    fn hits(&self) -> Vec<SearchHitJson> {
        self.rows
            .iter()
            .map(|row| SearchHitJson {
                hash: self.hashes.get(&row.path).cloned(),
                path: row.path.clone(),
                score: row.score,
                distance_km: row.distance_km,
                date: row.date.clone(),
            })
            .collect()
    }
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
    let outcome = collect_hits(args)?;
    for row in &outcome.rows {
        if !args.scores {
            println!("{}", row.path);
            continue;
        }
        // `--scores` prepends whichever key drove the order, so the number in
        // front of a path always explains why it is where it is.
        match outcome.primary {
            SortField::Relevance => match row.score {
                Some(score) => println!("{score:.4}\t{}", row.path),
                None => println!("{}", row.path),
            },
            SortField::Distance => match row.distance_km {
                Some(km) => println!("{km:.2}km\t{}", row.path),
                None => println!("{}", row.path),
            },
            SortField::Date => match &row.date {
                Some(date) => println!("{date}\t{}", row.path),
                None => println!("{}", row.path),
            },
            SortField::Size => match row.size_bytes {
                Some(bytes) => println!("{bytes}\t{}", row.path),
                None => println!("{}", row.path),
            },
        }
    }
    Ok(())
}

fn run_json(args: &SearchArgs) -> Result<SearchJson> {
    let outcome = collect_hits(args)?;
    let results = outcome.hits();
    Ok(SearchJson {
        schema_version: SCHEMA_VERSION,
        query: outcome.query,
        count: results.len(),
        results,
    })
}

/// Person query: bare paths, no hash/score (confirmed faces only).
pub(crate) fn person_hits(conn: &Connection, name: &str) -> Result<Vec<SearchHitJson>> {
    let paths = videre_core::person_search::search_by_person(conn, name, None)?;
    Ok(paths
        .into_iter()
        .map(|path| SearchHitJson {
            path,
            hash: None,
            score: None,
            distance_km: None,
            date: None,
        })
        .collect())
}

/// Load the embedding corpus, erroring if empty. Called BEFORE any model load
/// so a db without embeddings fails fast without downloading weights.
pub(crate) fn load_corpus(
    conn: &Connection,
    db: &Path,
    model_id: &str,
) -> Result<Vec<(String, Vec<f32>)>> {
    let corpus_raw = embeddings::load_embeddings(conn, model_id)?;
    anyhow::ensure!(
        !corpus_raw.is_empty(),
        "no embeddings found in {} for model {model_id}; run videre embed --model {model_id} first",
        db.display(),
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
                distance_km: None,
                date: None,
            });
        }
    }
    Ok(hits)
}

/// Whether this invocation ranks by similarity at all. Only a text query or
/// `--image` does; every other flag is a filter, which narrows without ordering.
fn is_ranked(args: &SearchArgs) -> bool {
    args.query.is_some() || args.image.is_some()
}

/// The requested order, or the one that keeps each invocation's historical
/// ordering when `--sort` is omitted.
///
/// Validated here, before anything opens a database or loads a model, so a
/// typo'd flag fails on its own terms rather than behind "no embeddings in
/// this library".
fn resolve_sort(args: &SearchArgs) -> Result<Vec<SortKey>> {
    let keys = match args.sort.as_deref() {
        Some(spec) => query::parse_sort(spec)?,
        None if is_ranked(args) => query::parse_sort("relevance")?,
        None if args.location.is_some() => query::parse_sort("distance")?,
        None => query::parse_sort("date")?,
    };
    // A sort key with nothing to read is a mistake, not a silent fallback: the
    // result would look ordered while being arbitrary.
    for key in &keys {
        match key.field {
            SortField::Relevance if !is_ranked(args) => {
                anyhow::bail!("--sort relevance needs a text query or --image <path>")
            }
            SortField::Distance if args.location.is_none() => {
                anyhow::bail!("--sort distance needs --location <place>")
            }
            _ => {}
        }
    }
    Ok(keys)
}

/// `--date` shorthand, or the normalised `--after`/`--before` pair.
fn resolve_dates(args: &SearchArgs) -> Result<(Option<String>, Option<String>)> {
    match args.date.as_deref() {
        Some(spec) => {
            let (after, before) = query::expand_date(spec)?;
            Ok((Some(after), Some(before)))
        }
        None => Ok((
            args.after
                .as_deref()
                .map(query::normalise_bound)
                .transpose()?,
            args.before
                .as_deref()
                .map(query::normalise_bound)
                .transpose()?,
        )),
    }
}

/// Narrows `cands` to what is within `radius_km` of `place`, recording each
/// survivor's distance.
///
/// Applied after the database-only predicates rather than inside
/// `candidates_with_model`, because geocoding can reach the network: a filter
/// that has already matched nothing must not pay for a lookup whose answer
/// cannot change the result.
fn apply_location(
    conn: &Connection,
    cands: &mut Candidates,
    place: &str,
    radius_km: f64,
) -> Result<()> {
    if cands.hashes.as_ref().is_some_and(|h| h.is_empty()) {
        cands.distances = Some(HashMap::new());
        return Ok(());
    }
    videre_core::geocode::ensure_geocode_cache_table(conn)?;
    let (lat, lon) = videre_core::geocode::forward_geocode_cached(conn, place)
        .with_context(|| format!("could not geocode {place:?}"))?;

    let within = query::by_location(conn, lat, lon, radius_km)?;
    let keep: HashSet<String> = match &cands.hashes {
        Some(existing) => within
            .keys()
            .filter(|h| existing.contains(*h))
            .cloned()
            .collect(),
        None => within.keys().cloned().collect(),
    };
    cands.distances = Some(
        within
            .into_iter()
            .filter(|(h, _)| keep.contains(h))
            .collect(),
    );
    cands.hashes = Some(keep);
    Ok(())
}

/// Every file in the library as `(path, hash, effective date, size)`.
///
/// One scan rather than a `paths_for_hash` per surviving hash: the date and
/// size are needed for sorting anyway, and the row count is trivial next to
/// the embedding corpus a ranked query already loads.
fn library_rows(conn: &Connection) -> Result<Vec<(String, String, Option<String>, Option<i64>)>> {
    let sql = format!(
        "SELECT path, hash, {}, size_bytes FROM file_hashes ORDER BY path",
        query::EFFECTIVE_DATE_SQL
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// What the JSON `query` object reports when several filters compose.
///
/// A ranking query names itself first, since it is the only thing that
/// ordered the results; otherwise the most specific filter wins. Under the old
/// mutually-exclusive flags exactly one of these was ever set, so every
/// single-filter invocation still reports what it always did.
fn describe_query(args: &SearchArgs, dates: &(Option<String>, Option<String>)) -> QueryJson {
    if let Some(text) = &args.query {
        return QueryJson {
            kind: "text",
            value: text.clone(),
        };
    }
    if let Some(img) = &args.image {
        return QueryJson {
            kind: "image",
            value: img.display().to_string(),
        };
    }
    for (kind, value) in [
        ("person", &args.person),
        ("category", &args.category),
        ("location", &args.location),
    ] {
        if let Some(value) = value {
            return QueryJson {
                kind,
                value: value.clone(),
            };
        }
    }
    QueryJson {
        kind: "date",
        value: args.date.clone().unwrap_or_else(|| {
            let (after, before) = dates;
            format!(
                "{}..{}",
                after.as_deref().unwrap_or(""),
                before.as_deref().unwrap_or("")
            )
        }),
    }
}

/// The single query pipeline behind both output modes: filters narrow, a text
/// or image query ranks the survivors, and the sort keys order them.
///
/// Person hits carry only a path (person search has always returned bare
/// paths); every other hit carries its hash, plus a cosine score when there
/// was a ranking query and a distance when `--location` was given.
fn collect_hits(args: &SearchArgs) -> Result<Outcome> {
    let sort_keys = resolve_sort(args)?;
    let primary = sort_keys[0].field;
    let dates = resolve_dates(args)?;

    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db).with_context(|| format!("open {}", db.display()))?;

    let model_id = videre_core::embeddings::resolve_model_id(args.model.as_deref())?;

    let filters = Filters {
        person: args.person.clone(),
        category: args.category.clone(),
        location: None, // applied separately, see apply_location
        after: dates.0.clone(),
        before: dates.1.clone(),
    };
    anyhow::ensure!(
        is_ranked(args) || filters.any_active() || args.location.is_some(),
        "provide a text query, --image <path>, or at least one filter \
         (--person, --category, --location, --date, --after, --before)"
    );

    // Only a ranking query reads vectors. Attaching for a pure filter query
    // would turn a working search into a hard error on an unembedded library.
    if is_ranked(args) {
        // create: false. A reader must never bring an empty model database
        // into existence, or "no results" would silently replace a clear
        // error naming the models that do exist.
        videre_core::embeddings_db::attach_for_read(&conn, &db, &model_id)?;
    }

    let mut cands = query::candidates_with_model(&conn, &filters, &model_id)?;
    if let Some(place) = &args.location {
        apply_location(&conn, &mut cands, place, args.radius)?;
    }

    let scores = is_ranked(args)
        .then(|| rank(args, &conn, &db, &model_id, &cands, &sort_keys))
        .transpose()?;

    // A ranked query has already reduced the field to its scored hashes.
    let allowed: Option<HashSet<String>> = match &scores {
        Some(scored) => Some(scored.keys().cloned().collect()),
        None => cands.hashes.clone(),
    };

    let mut hashes: HashMap<String, String> = HashMap::new();
    let mut rows: Vec<Sortable> = Vec::new();
    for (path, hash, date, size_bytes) in library_rows(&conn)? {
        if allowed.as_ref().is_some_and(|a| !a.contains(&hash)) {
            continue;
        }
        rows.push(Sortable {
            score: scores.as_ref().and_then(|s| s.get(&hash).copied()),
            distance_km: cands.distances.as_ref().and_then(|d| d.get(&hash).copied()),
            date,
            size_bytes,
            path: path.clone(),
        });
        hashes.insert(path, hash);
    }

    query::apply_sort(&mut rows, &sort_keys);
    rows.truncate(args.top_k);

    let query = describe_query(args, &dates);
    if query.kind == "person" {
        hashes.clear(); // person hits have always been bare paths
    }
    if rows.is_empty() && !args.json {
        // In --json mode the empty result is conveyed as count 0; keep stdout
        // the only channel so a clean agent invocation emits nothing on stderr.
        match query.kind {
            "person" => eprintln!("No confirmed photos found for person: {}", query.value),
            "category" => eprintln!("No files found classified as: {}", query.value),
            "location" => eprintln!(
                "No photos found within {}km of: {}",
                args.radius, query.value
            ),
            _ => {}
        }
    }

    Ok(Outcome {
        query,
        rows,
        hashes,
        primary,
    })
}

/// Cosine scores for the candidate hashes, keyed by hash.
///
/// Truncating to `top_k` inside the ranker is only safe when relevance is the
/// primary key and descending; under any other order a lower-scoring row can
/// legitimately come first, so the whole candidate set has to be scored.
fn rank(
    args: &SearchArgs,
    conn: &Connection,
    db: &Path,
    model_id: &str,
    cands: &Candidates,
    sort_keys: &[SortKey],
) -> Result<HashMap<String, f32>> {
    let corpus = load_corpus(conn, db, model_id)?;
    let corpus: Vec<(String, Vec<f32>)> = match &cands.hashes {
        Some(keep) => corpus
            .into_iter()
            .filter(|(hash, _)| keep.contains(hash))
            .collect(),
        None => corpus,
    };

    let embedder = model::Embedder::load(device::best_device(), model_id)?;
    let query_vec = match (&args.query, &args.image) {
        (Some(text), None) => embedder.embed_text(text)?,
        (None, Some(img)) => model::embed_image_file(&embedder, img)?,
        _ => anyhow::bail!("provide either a text query or --image <path>"),
    };

    let k = if sort_keys[0].field == SortField::Relevance && sort_keys[0].desc {
        args.top_k
    } else {
        corpus.len()
    };
    Ok(search::top_k(&query_vec, &corpus, k).into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_hit_serializes_with_hash_and_score() {
        let doc = SearchJson {
            schema_version: SCHEMA_VERSION,
            query: QueryJson {
                kind: "text",
                value: "sunset".to_string(),
            },
            count: 1,
            results: vec![SearchHitJson {
                path: "/a.jpg".to_string(),
                hash: Some("abc".to_string()),
                score: Some(0.5),
                distance_km: None,
                date: None,
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
            query: QueryJson {
                kind: "person",
                value: "Alice".to_string(),
            },
            count: 1,
            results: vec![SearchHitJson {
                path: "/a.jpg".to_string(),
                hash: None,
                score: None,
                distance_km: None,
                date: None,
            }],
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(!json.contains("hash"));
        assert!(!json.contains("score"));
        assert!(json.contains("\"path\":\"/a.jpg\""));
    }

    #[test]
    fn category_hit_includes_hash_but_omits_score() {
        let doc = SearchJson {
            schema_version: SCHEMA_VERSION,
            query: QueryJson {
                kind: "category",
                value: "screenshot".to_string(),
            },
            count: 1,
            results: vec![SearchHitJson {
                path: "/a.png".to_string(),
                hash: Some("abc".to_string()),
                score: None,
                distance_km: None,
                date: None,
            }],
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"kind\":\"category\""));
        assert!(json.contains("\"hash\":\"abc\""));
        assert!(!json.contains("\"score\""));
    }
}
