use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use videre::types::{ErrorJson, SCHEMA_VERSION};
use videre_core::query::{self, Candidates, SortField, SortKey, Sortable};
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, search};

/// One search request. Fields are `pub(crate)` rather than private because the
/// MCP server builds one directly from its tool parameters and runs it through
/// the same pipeline; a second, parallel query path is exactly what would let
/// the two surfaces drift apart.
#[derive(clap::Args)]
pub struct SearchArgs {
    /// SQLite database with embeddings (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    pub(crate) db: Option<PathBuf>,

    /// Embedding model to search against (default: 'videre config set model', else
    /// the built-in default). Must already have been embedded; run
    /// 'videre stats' to see which models this library has.
    #[arg(long, value_parser = super::parse_model_id)]
    pub(crate) model: Option<String>,

    /// Text query, e.g. "sunset on beach" (omit when using --image)
    pub(crate) query: Option<String>,

    /// Search by example image instead of text
    #[arg(long, conflicts_with = "query")]
    pub(crate) image: Option<PathBuf>,

    /// Rank by the stored embedding of a file already in this library, by hash.
    ///
    /// :warning: **`#[arg(skip)]`: deliberately not a CLI flag.** It is set by
    /// callers that already hold a hash, which is the gallery asking "more like
    /// this one". `--image` is the CLI's way to ask the same question about an
    /// arbitrary file, and it re-embeds, because a file outside the library has
    /// no stored vector to read. Exposing this as a flag would mean asking a
    /// person to type a 64-character hash.
    #[arg(skip)]
    pub(crate) like: Option<String>,

    /// Only files containing a named person (confirmed faces only)
    #[arg(long)]
    pub(crate) person: Option<String>,

    /// Only files classified as this category: photo/screenshot/document/
    /// meme/unknown (requires a prior 'videre classify' run)
    #[arg(long)]
    pub(crate) category: Option<String>,

    /// Only photos within --radius km of this place, e.g. "Berlin, Germany"
    /// (forward-geocoded via the free public Nominatim API, the first
    /// network call this CLI ever makes; results are cached locally)
    #[arg(long)]
    pub(crate) location: Option<String>,

    /// Search radius in km around --location
    #[arg(long, default_value_t = 20.0, requires = "location")]
    pub(crate) radius: f64,

    /// Only files whose date is on or after this (inclusive).
    /// Accepts YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS.
    #[arg(long, conflicts_with = "date")]
    pub(crate) after: Option<String>,

    /// Only files whose date is before this (exclusive), so adjacent ranges
    /// do not both match the boundary instant.
    #[arg(long, conflicts_with = "date")]
    pub(crate) before: Option<String>,

    /// Shorthand for a whole year, month, or day: YYYY, YYYY-MM, or YYYY-MM-DD
    #[arg(long)]
    pub(crate) date: Option<String>,

    /// Result order: comma-separated field[:asc|desc]. Fields: relevance,
    /// distance, date, size. Defaults are relevance/date/size descending and
    /// distance ascending.
    #[arg(long)]
    pub(crate) sort: Option<String>,

    /// Number of results
    #[arg(short = 'k', long, default_value_t = 20)]
    pub(crate) top_k: usize,

    /// Prepend the cosine score to each output line (no-op with --json: score is always included)
    #[arg(long)]
    pub(crate) scores: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    pub(crate) json: bool,

    /// Also write these results to a browsable HTML page.
    /// Bare --html targets <db>_search.html.
    /// Note: place a bare --html after the query.
    #[arg(long, num_args = 0..=1)]
    pub(crate) html: Option<Option<PathBuf>>,

    /// --type / --ext / --mime, from the shared selection vocabulary.
    ///
    /// Flattened from the shared groups rather than declared here, so a
    /// predicate is defined once and every command honouring it agrees. The
    /// older filters above predate the layer and still declare their own
    /// flags; they feed the same `RowSelection` and can be folded into the
    /// shared groups later without any user-visible change.
    #[command(flatten)]
    pub(crate) media: super::selection_args::MediaArgs,

    /// --path, from the shared selection vocabulary.
    #[command(flatten)]
    pub(crate) paths: super::selection_args::PathArgs,

    /// --rating / --pick / --label / --like filters.
    #[command(flatten)]
    pub(crate) marks: super::selection_args::MarkArgs,

    /// --tag filter (repeatable; all must be present).
    #[command(flatten)]
    pub(crate) tags: super::selection_args::TagFilterArgs,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchJson {
    pub(crate) schema_version: u32,
    pub(crate) query: QueryJson,
    pub(crate) count: usize,
    /// How many matched before `-k` truncated. Equal to `count` when nothing
    /// was dropped.
    ///
    /// Without this an agent cannot tell a complete answer from a truncated
    /// one: a filter-only query has no ranker, so the `count` it receives is an
    /// arbitrary slice with nothing to indicate more exist. The text path says
    /// so on stderr; JSON has no stderr to read.
    pub(crate) total_matches: usize,
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
    /// Match count before truncation; see `SearchJson::total_matches`.
    total_matches: usize,
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
        match run_json(&args, &FreshEmbedder) {
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

/// `--html`: these results, as a page you can keep.
///
/// The hits arrive as ranked paths, because ranking is what search did. The
/// rows behind them come from one lookup, and the ranking order is preserved:
/// the order *is* the answer.
fn write_html(args: &SearchArgs, outcome: &Outcome, arg: Option<&std::path::Path>) -> Result<()> {
    let db = super::resolve_reader_db_must_exist(args.db.clone())?;
    let output = if let Some(p) = arg {
        p.to_path_buf()
    } else {
        let stem = db
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut p = db.clone();
        p.set_file_name(format!("{stem}_search.html"));
        p
    };
    let conn = videre_core::db::open_wal(&db)?;
    let paths: Vec<String> = outcome.rows.iter().map(|r| r.path.clone()).collect();
    let rows = crate::render::rows_for_paths(&conn, &paths);
    crate::render::write_static_page(&conn, &output, &[], Some(&rows))
}

fn run_text(args: &SearchArgs) -> Result<()> {
    let outcome = collect_hits(args, &FreshEmbedder)?;
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
    if let Some(arg) = args.html.as_ref() {
        write_html(args, &outcome, arg.as_deref())?;
    }
    Ok(())
}

/// The whole query, as a JSON document. Shared with the MCP `search` tool,
/// which builds a `SearchArgs` of its own and calls straight in here.
pub(crate) fn run_json(args: &SearchArgs, embedder: &dyn QueryEmbedder) -> Result<SearchJson> {
    let outcome = collect_hits(args, embedder)?;
    let results = outcome.hits();
    Ok(SearchJson {
        schema_version: SCHEMA_VERSION,
        query: outcome.query,
        count: results.len(),
        total_matches: outcome.total_matches,
        results,
    })
}

/// What a ranking query is: text, or an example image.
pub(crate) enum QueryInput<'a> {
    Text(&'a str),
    Image(&'a Path),
}

/// Turns a ranking query into a vector.
///
/// Injected rather than constructed inside the pipeline because the two
/// surfaces have opposite lifetimes: the CLI loads an embedder, answers one
/// query and exits, while the MCP server keeps one alive across calls so only
/// the first search pays the load. Everything else about the query is shared,
/// which is what stops the two drifting apart.
pub(crate) trait QueryEmbedder {
    fn embed(&self, model_id: &str, input: QueryInput<'_>) -> Result<Vec<f32>>;
}

/// The CLI's: one embedder per invocation, dropped when the command exits.
pub(crate) struct FreshEmbedder;

impl QueryEmbedder for FreshEmbedder {
    fn embed(&self, model_id: &str, input: QueryInput<'_>) -> Result<Vec<f32>> {
        let embedder = model::Embedder::load(device::best_device(), model_id)?;
        match input {
            QueryInput::Text(text) => embedder.embed_text(text),
            QueryInput::Image(path) => model::embed_image_file(&embedder, path),
        }
    }
}

/// A long-lived server's embedder: loaded on the first ranking search and kept
/// for the life of the process, so only that one call pays the ~900ms load.
///
/// Shared by the MCP server and the gallery server, which have the same shape: a
/// process that answers many queries. `FreshEmbedder` above is the CLI's, which
/// loads per invocation. That difference is the only reason the pipeline takes
/// an embedder at all.
///
/// :warning: **It must stay lazy.** A server whose library nobody searches must
/// never load a model, for the reason `CLAUDE.md` records: a model loaded before
/// there was work to do once downloaded 778MB from inside a unit test.
pub(crate) struct CachedEmbedder<'a>(
    pub(crate) &'a std::sync::Mutex<Option<videre_ml::model::Embedder>>,
);

impl QueryEmbedder for CachedEmbedder<'_> {
    fn embed(&self, model_id: &str, input: QueryInput<'_>) -> Result<Vec<f32>> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(model::Embedder::load(device::best_device(), model_id)?);
        }
        let embedder = guard.as_ref().expect("just initialized");
        match input {
            QueryInput::Text(text) => embedder.embed_text(text),
            QueryInput::Image(path) => model::embed_image_file(embedder, path),
        }
    }
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

/// Whether this invocation ranks by similarity at all. Only a text query or
/// `--image` does; every other flag is a filter, which narrows without ordering.
fn is_ranked(args: &SearchArgs) -> bool {
    args.query.is_some() || args.image.is_some() || args.like.is_some()
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
    // :warning: Only claim "date" when a date was actually asked for. This used
    // to be the unconditional fall-through, so `--type video` alone reported
    // itself as a date query with a value of "..", and an agent reading `--json`
    // was told something untrue about its own request.
    let (after, before) = dates;
    if args.date.is_some() || after.is_some() || before.is_some() {
        return QueryJson {
            kind: "date",
            value: args.date.clone().unwrap_or_else(|| {
                format!(
                    "{}..{}",
                    after.as_deref().unwrap_or(""),
                    before.as_deref().unwrap_or("")
                )
            }),
        };
    }

    // Whatever media or path filters remain. Several can be active at once, so
    // the value lists them rather than picking one and hiding the rest.
    let mut parts: Vec<String> = Vec::new();
    for (label, values) in [
        ("type", &args.media.media_type),
        ("ext", &args.media.ext),
        ("mime", &args.media.mime),
    ] {
        for v in values {
            parts.push(format!("{label}={v}"));
        }
    }
    for p in &args.paths.path {
        parts.push(format!("path={}", p.display()));
    }
    QueryJson {
        kind: "filter",
        value: parts.join(" "),
    }
}

/// The single query pipeline behind both output modes: filters narrow, a text
/// or image query ranks the survivors, and the sort keys order them.
///
/// Person hits carry only a path (person search has always returned bare
/// paths); every other hit carries its hash, plus a cosine score when there
/// was a ranking query and a distance when `--location` was given.
fn collect_hits(args: &SearchArgs, embedder: &dyn QueryEmbedder) -> Result<Outcome> {
    let sort_keys = resolve_sort(args)?;
    let primary = sort_keys[0].field;
    let dates = resolve_dates(args)?;

    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db).with_context(|| format!("open {}", db.display()))?;

    let model_id = videre_core::embeddings::resolve_model_id(args.model.as_deref())?;

    // One selection, resolved in one place. `--location` is part of it rather
    // than a separate pass: `resolve` geocodes the place name, intersects, and
    // carries the per-hash distances that `--sort distance` reads.
    let selection = videre_core::selection::RowSelection {
        person: args.person.clone(),
        category: args.category.clone(),
        place: args
            .location
            .as_ref()
            .map(|p| videre_core::selection::PlaceQuery::Named {
                place: p.clone(),
                radius_km: args.radius,
            }),
        after: dates.0.clone(),
        before: dates.1.clone(),
        kinds: args.media.kinds()?,
        exts: args.media.ext.clone(),
        mimes: args.media.mime.clone(),
        has: Vec::new(),
        missing: Vec::new(),
        paths: args.paths.path.clone(),
        min_rating: args.marks.rating,
        pick: args.marks.pick_state(),
        label: args.marks.label.clone(),
        liked: args.marks.like,
        tags: args.tags.tags.clone(),
    };
    anyhow::ensure!(
        is_ranked(args) || !selection.is_empty(),
        "provide a text query, --image <path>, or at least one filter \
         (--person, --category, --location, --date, --after, --before, \
          --type, --ext, --mime, --path)"
    );

    // Only a ranking query reads vectors. Attaching for a pure filter query
    // would turn a working search into a hard error on an unembedded library.
    if is_ranked(args) {
        // create: false. A reader must never bring an empty model database
        // into existence, or "no results" would silently replace a clear
        // error naming the models that do exist.
        videre_core::embeddings_db::attach_for_read(&conn, &db, &model_id)?;
    }

    let resolved = selection.resolve(
        &conn,
        &videre_core::selection::SelectionCtx {
            model_id: Some(model_id.clone()),
        },
    )?;
    let cands = query::Candidates {
        hashes: resolved.hashes,
        distances: resolved.distances,
    };

    let scores = is_ranked(args)
        .then(|| rank(args, &conn, &db, &model_id, &cands, &sort_keys, embedder))
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

    // Say so when results are being dropped. Without this, a filter-only query
    // silently returns an arbitrary `-k` slice of a larger set: there is no
    // ranker to make "top 20" meaningful, so the 20 shown are simply the first
    // 20 in sort order and nothing indicates the other 27 exist. Reported as
    // "not working" on a real library, where a location+date query matched 47
    // files including 3 videos and the default 20 happened to contain none.
    let total_matches = rows.len();
    rows.truncate(args.top_k);
    if total_matches > rows.len() && !args.json {
        // stderr, so a piped stdout stays exactly the list of paths.
        eprintln!(
            "showing {} of {} matches; pass -k {} to see them all",
            rows.len(),
            total_matches,
            total_matches
        );
    }

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
        total_matches,
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
    embedder: &dyn QueryEmbedder,
) -> Result<HashMap<String, f32>> {
    let corpus = load_corpus(conn, db, model_id)?;

    // :warning: Resolved against the **unfiltered** corpus, before the lines
    // below narrow it. A selection can exclude the example itself - asking for
    // photos of one person that resemble a photo of someone else is a perfectly
    // ordinary request - and looking the vector up afterwards would fail on
    // exactly those queries.
    let stored = args
        .like
        .as_ref()
        .map(|hash| {
            corpus
                .iter()
                .find(|(h, _)| h == hash)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no embedding for {hash} in this library under {model_id}; \
                         run videre embed first"
                    )
                })
        })
        .transpose()?;

    let corpus: Vec<(String, Vec<f32>)> = match &cands.hashes {
        Some(keep) => corpus
            .into_iter()
            .filter(|(hash, _)| keep.contains(hash))
            .collect(),
        None => corpus,
    };

    // An embedder turns something *outside* the library into a vector. A stored
    // one is already here, so it never reaches the embedder at all, which is
    // why this is not a `QueryInput` variant: every implementation would need an
    // arm it could not answer.
    let query_vec = match (&args.query, &args.image, stored) {
        (Some(text), None, None) => embedder.embed(model_id, QueryInput::Text(text))?,
        (None, Some(img), None) => embedder.embed(model_id, QueryInput::Image(img))?,
        (None, None, Some(vec)) => vec,
        _ => {
            anyhow::bail!("provide exactly one of a text query, --image <path>, or an example hash")
        }
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
    use clap::Parser;

    /// `SearchArgs` derives `Args`, not `Parser`, so parsing one on its own
    /// needs a command to hang it off.
    #[derive(Parser)]
    struct Standalone {
        #[command(flatten)]
        inner: SearchArgs,
    }

    fn parse(argv: &[&str]) -> SearchArgs {
        Standalone::parse_from(argv).inner
    }

    /// A `SearchArgs` with nothing set, so each test names only what it is about.
    fn no_query() -> SearchArgs {
        parse(&["videre"])
    }

    #[test]
    fn a_media_filter_is_not_described_as_a_date_query() {
        // `--type video` alone used to report `kind: "date", value: ".."`,
        // because "date" was the unconditional fall-through. An agent reading
        // `--json` was told something untrue about its own request.
        for (argv, want) in [
            (vec!["videre", "--type", "video"], "type=video"),
            (vec!["videre", "--ext", "mov"], "ext=mov"),
            (
                vec!["videre", "--mime", "video/quicktime"],
                "mime=video/quicktime",
            ),
            (
                vec!["videre", "--path", "/Volumes/Archive"],
                "path=/Volumes/Archive",
            ),
        ] {
            let args = parse(&argv);
            let q = describe_query(&args, &(None, None));
            assert_eq!(q.kind, "filter", "{argv:?}");
            assert_eq!(q.value, want, "{argv:?}");
        }
    }

    #[test]
    fn several_filters_are_all_named_rather_than_one_hiding_the_rest() {
        let args = parse(&["videre", "--type", "image", "--ext", "jpg"]);
        let q = describe_query(&args, &(None, None));
        assert_eq!(q.kind, "filter");
        assert_eq!(q.value, "type=image ext=jpg");
    }

    #[test]
    fn a_date_query_is_still_a_date_query() {
        let args = parse(&["videre", "--date", "2024"]);
        assert_eq!(describe_query(&args, &(None, None)).kind, "date");

        // ...including the range form, which arrives resolved rather than as
        // `--date`, so the fall-through has to look at both.
        let ranged = describe_query(
            &no_query(),
            &(Some("2019-06-01".into()), Some("2019-09-01".into())),
        );
        assert_eq!(ranged.kind, "date");
        assert_eq!(ranged.value, "2019-06-01..2019-09-01");
    }

    #[test]
    fn the_ranking_query_wins_over_any_filter() {
        // Filters narrow; text and image rank. What produced the *order* is
        // what the description should name.
        let args = parse(&["videre", "sunset", "--type", "image"]);
        let q = describe_query(&args, &(None, None));
        assert_eq!(q.kind, "text");
        assert_eq!(q.value, "sunset");
    }

    #[test]
    fn text_hit_serializes_with_hash_and_score() {
        let doc = SearchJson {
            schema_version: SCHEMA_VERSION,
            query: QueryJson {
                kind: "text",
                value: "sunset".to_string(),
            },
            count: 1,
            total_matches: 1,
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
            total_matches: 1,
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
            total_matches: 1,
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
