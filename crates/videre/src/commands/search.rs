use crate::command_context::CommandContext;
use anyhow::Result;
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
#[derive(clap::Args, Clone)]
pub struct SearchArgs {
    /// Embedding model to search against (default: 'videre config set model', else
    /// the built-in default). Must already have been embedded; run
    /// 'videre stats' to see which models this library has.
    #[arg(
        long,
        value_parser = super::parse_model_id,
        add = clap_complete::engine::ArgValueCompleter::new(crate::completions::model_candidates)
    )]
    pub(crate) model: Option<String>,

    /// What to find, in the query language: words to rank by, filters such
    /// as person:özgür, tag:deniz, date:2023 or rating:>=4, combined with
    /// OR, NOT or -, and (groups). Quote the whole query in single quotes.
    /// See https://docs.videre.sh/reference/query-syntax/
    // Not `allow_hyphen_values`: that would take a mistyped flag such as
    // `--persn` for a query instead of refusing it. A query starting with `-`
    // goes after `--`, or starts with NOT.
    #[arg(add = clap_complete::engine::ArgValueCompleter::new(
        crate::completions::query_candidates
    ))]
    pub(crate) query: Option<String>,

    /// Search by example image instead of text; a query may still filter
    #[arg(long)]
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
    #[arg(
        long,
        add = clap_complete::engine::ArgValueCompleter::new(
            crate::completions::person_candidates
        )
    )]
    pub(crate) person: Option<String>,

    /// Only files with anyone whose name has these words, as whole words:
    /// --people Erhan finds Erhan Gündoğan and Erhan Kaya, not Serhan
    #[arg(
        long,
        value_name = "WORDS",
        add = clap_complete::engine::ArgValueCompleter::new(
            crate::completions::person_candidates
        )
    )]
    pub(crate) people: Option<String>,

    /// Only files classified as this category: photo, screenshot, document,
    /// meme or unknown (requires a prior 'videre classify' run)
    #[arg(
        long,
        add = clap_complete::engine::ArgValueCompleter::new(
            crate::completions::category_candidates
        )
    )]
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

    /// Prepend the score to each output line: a text match probability, or an image similarity (no-op with --json: score is always included)
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

    /// --has / --missing database presence filters.
    #[command(flatten)]
    pub(crate) presence: super::selection_args::PresenceArgs,

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

pub fn run(args: SearchArgs, ctx: &CommandContext) -> Result<()> {
    if args.json {
        match run_json_in(&args, &FreshEmbedder, ctx) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                // stdout must always carry exactly one valid JSON object; the
                // error goes here, and main logs it without repeating it on stderr.
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                Err(crate::exit::Exit::shown(e).into())
            }
        }
    } else {
        run_text(&args, ctx)
    }
}

/// `--html`: these results, as a page you can keep.
///
/// The hits arrive as ranked paths, because ranking is what search did. The
/// rows behind them come from one lookup, and the ranking order is preserved:
/// the order *is* the answer.
fn write_html(
    ctx: &CommandContext,
    outcome: &Outcome,
    arg: Option<&std::path::Path>,
) -> Result<()> {
    let db = &ctx.library.paths.db;
    // A bare --html targets a page beside the selected database; an explicit
    // relative path is an operand, resolved against the launch directory.
    let output = match arg {
        Some(p) => ctx.operand(p),
        None => {
            let stem = db
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut p = db.clone();
            p.set_file_name(format!("{stem}_search.html"));
            p
        }
    };
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let paths: Vec<String> = outcome.rows.iter().map(|r| r.path.clone()).collect();
    let rows = crate::render::rows_for_paths(&conn, &paths);
    crate::render::write_static_page(&conn, &output, &[], &[], Some(&rows))
}

fn run_text(args: &SearchArgs, ctx: &CommandContext) -> Result<()> {
    let outcome = collect_hits(args, ctx, &FreshEmbedder)?;
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
        write_html(ctx, &outcome, arg.as_deref())?;
    }
    Ok(())
}

/// The whole query, as a JSON document. Shared with the MCP `search` tool and
/// the gallery server, which build a `SearchArgs` of their own and call
/// straight in here against their own startup-bound library context.
pub(crate) fn run_json_in(
    args: &SearchArgs,
    embedder: &dyn QueryEmbedder,
    ctx: &CommandContext,
) -> Result<SearchJson> {
    let outcome = collect_hits(args, ctx, embedder)?;
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
    fn embed(&self, model_id: &str, input: QueryInput<'_>) -> Result<QueryVector>;
}

/// A ranking query's vector, and for text, how the model turns a cosine into
/// a match probability. An image query has no calibration: image against
/// image was never trained with one, so it ranks and filters by cosine.
pub(crate) struct QueryVector {
    pub(crate) vector: Vec<f32>,
    pub(crate) calibration: Option<videre_ml::search::Calibration>,
}

/// One embedder's answer to one query, the same for every implementation.
fn query_vector(embedder: &model::Embedder, input: QueryInput<'_>) -> Result<QueryVector> {
    Ok(match input {
        QueryInput::Text(text) => QueryVector {
            vector: embedder.embed_text(text)?,
            calibration: Some(embedder.calibration()),
        },
        QueryInput::Image(path) => QueryVector {
            vector: model::embed_image_file(embedder, path)?,
            calibration: None,
        },
    })
}

/// The CLI's: one embedder per invocation, dropped when the command exits.
pub(crate) struct FreshEmbedder;

impl QueryEmbedder for FreshEmbedder {
    fn embed(&self, model_id: &str, input: QueryInput<'_>) -> Result<QueryVector> {
        let embedder = model::Embedder::load(device::best_device(), model_id)?;
        query_vector(&embedder, input)
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
    fn embed(&self, model_id: &str, input: QueryInput<'_>) -> Result<QueryVector> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(model::Embedder::load(device::best_device(), model_id)?);
        }
        let embedder = guard.as_ref().expect("just initialized");
        query_vector(embedder, input)
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

/// Assemble the row selection for a search through the one shared assembler, so
/// search honours every predicate exactly as the other row-backed commands do
/// and the two cannot drift.
///
/// Search keeps its own `--person`/`--category`/`--location`/`--radius`/
/// `--after`/`--before`/`--date` flag declarations, which predate the shared
/// groups; rather than regress that surface, their values are packed into the
/// shared group types here. Dates arrive already resolved (see `resolve_dates`),
/// so `--date` expansion and bound normalisation are preserved and the date
/// group is passed with its raw `--date` cleared.
fn selection_for(
    args: &SearchArgs,
    dates: &(Option<String>, Option<String>),
) -> Result<videre_core::selection::RowSelection> {
    let date_group = super::selection_args::DateArgs {
        after: dates.0.clone(),
        before: dates.1.clone(),
        date: None,
    };
    let place_group = super::selection_args::PlaceArgs {
        location: args.location.clone(),
        radius: args.radius,
    };
    let people_group = super::selection_args::PeopleArgs {
        person: args.person.clone(),
        category: args.category.clone(),
    };
    let mut selection = super::selection_args::row_selection(
        Some(&args.media),
        Some(&date_group),
        Some(&place_group),
        Some(&people_group),
        Some(&args.presence),
        Some(&args.paths),
        Some(&args.marks),
        Some(&args.tags),
    )?;
    selection.people = args.people.clone();
    Ok(selection)
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
        ("people", &args.people),
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
    for field in &args.presence.has {
        parts.push(format!("has={field}"));
    }
    for field in &args.presence.missing {
        parts.push(format!("missing={field}"));
    }
    if let Some(rating) = args.marks.rating {
        parts.push(format!("rating={rating}"));
    }
    if let Some(pick) = &args.marks.pick {
        parts.push(format!("pick={pick}"));
    }
    if let Some(label) = &args.marks.label {
        parts.push(format!("label={label}"));
    }
    if args.marks.like {
        parts.push("like=true".to_string());
    }
    for tag in &args.tags.tags {
        parts.push(format!("tag={tag}"));
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
/// paths); every other hit carries its hash, plus a score when there
/// was a ranking query and a distance when `--location` was given.
fn collect_hits(
    args: &SearchArgs,
    ctx: &CommandContext,
    embedder: &dyn QueryEmbedder,
) -> Result<Outcome> {
    // The positional query is the query language: its words without a key are
    // the text that ranks, its filters narrow. Everything below sees only the
    // text as `query`, so ranking, sorting and reporting are unchanged.
    let compiled = videre::query_lang::compile(args.query.as_deref().unwrap_or(""))
        .map_err(|e| anyhow::anyhow!("invalid query: {e}"))?;
    anyhow::ensure!(
        compiled.text.is_none() || (args.image.is_none() && args.like.is_none()),
        "the query's words rank the results, and so does --image: use one; \
         filters such as tag:deniz can still go with --image"
    );
    let text_only = SearchArgs {
        query: compiled.text.clone(),
        ..args.clone()
    };
    let args = &text_only;
    let sort_keys = resolve_sort(args)?;
    let primary = sort_keys[0].field;
    let dates = resolve_dates(args)?;

    let db = ctx.library.paths.db.clone();
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // Search is a reader; it takes the library's shared activity lease so its
    // query cannot run against rows an exclusive maintenance pass is removing.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;

    let model_id = videre_core::embeddings::resolve_model_id_from(
        &ctx.library.settings,
        args.model.as_deref(),
    )?;
    // One selection, resolved in one place, and built by the shared assembler so
    // search honours the same vocabulary as every other row-backed command.
    // `--location` is part of it rather than a separate pass: `resolve` geocodes
    // the place name, intersects, and carries the per-hash distances that
    // `--sort distance` reads.
    let selection = selection_for(args, &dates)?;
    anyhow::ensure!(
        is_ranked(args) || !selection.is_empty() || compiled.filter.is_some(),
        "provide a text query, --image <path>, or at least one filter \
         (--person, --category, --location, --date, --after, --before, \
          --type, --ext, --mime, --path, --has, --missing, \
          --rating, --pick, --label, --like, --tag)"
    );

    // Only a ranking query reads vectors. Attaching for a pure filter query
    // would turn a working search into a hard error on an unembedded library.
    if is_ranked(args) {
        // create: false. A reader must never bring an empty model database
        // into existence, or "no results" would silently replace a clear
        // error naming the models that do exist.
        videre_core::embeddings_db::attach_for_read_in(&conn, &ctx.library, &model_id)?;
    }

    // resolve_in guards every --path against the selected root before it
    // geocodes or reads a row, so an out-of-root or unresolved path filter
    // rejects the whole query before any model load below.
    let selection_ctx = videre_core::selection::SelectionCtx {
        model_id: Some(model_id.clone()),
    };
    let resolved = selection.resolve_in(&conn, &selection_ctx, &ctx.library)?;
    // The query's filter narrows what the flags selected; it never widens it.
    let hashes = match &compiled.filter {
        Some(filter) => {
            let matched = videre::query_lang::resolve(filter, &conn, &selection_ctx, &ctx.library)?;
            Some(match resolved.hashes {
                Some(flags) => flags.intersection(&matched).cloned().collect(),
                None => matched,
            })
        }
        None => resolved.hashes,
    };
    let cands = query::Candidates {
        hashes,
        distances: resolved.distances,
    };

    let scores = is_ranked(args)
        .then(|| {
            rank(
                args,
                &conn,
                &db,
                &model_id,
                &cands,
                &sort_keys,
                embedder,
                &ctx.library.settings,
            )
        })
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
        tracing::info!(
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
            "person" => tracing::info!("No confirmed photos found for person: {}", query.value),
            "category" => tracing::info!("No files found classified as: {}", query.value),
            "location" => tracing::info!(
                "No photos found within {}km of: {}",
                args.radius,
                query.value
            ),
            "text" => tracing::info!("{}", empty_text_note(ctx.library.settings.search_min_match)),
            "image" => {
                if let Some(floor) = ctx.library.settings.similar_min_score {
                    tracing::info!(
                        "No matches scoring at least {floor} (similar_min_score); \
                         videre config unset similar-min-score shows every ranked result"
                    )
                }
            }
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

/// Scores for the candidate hashes, keyed by hash: a text query's match
/// probability, or an image's cosine, with the library's cutoff applied.
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
    settings: &videre_core::library_config::LibraryConfig,
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
    let query = match (&args.query, &args.image, stored) {
        (Some(text), None, None) => embedder.embed(model_id, QueryInput::Text(text))?,
        (None, Some(img), None) => embedder.embed(model_id, QueryInput::Image(img))?,
        (None, None, Some(vector)) => QueryVector {
            vector,
            calibration: None,
        },
        _ => {
            anyhow::bail!("provide exactly one of a text query, --image <path>, or an example hash")
        }
    };

    let k = if sort_keys[0].field == SortField::Relevance && sort_keys[0].desc {
        args.top_k
    } else {
        corpus.len()
    };
    Ok(search::top_k(&query.vector, &corpus, k)
        .into_iter()
        .filter_map(|(hash, cos)| match query.calibration {
            // Text: the model's own match probability, so one cutoff means
            // the same for every query and model. A cosine never did: what
            // was noise on one model was a match on another.
            Some(c) => {
                let p = c.probability(cos);
                (p >= settings.search_min_match as f32).then_some((hash, p))
            }
            // An image: its cosine, against the image floor when one is set.
            None => settings
                .similar_min_score
                .is_none_or(|floor| cos >= floor as f32)
                .then_some((hash, cos)),
        })
        .collect())
}

/// What an empty text search says. With a floor set, the floor is the likely
/// cause, so it is named, or an empty answer reads as "nothing in the
/// library". With none, the filters left nothing to rank.
fn empty_text_note(floor: f64) -> String {
    if floor > 0.0 {
        format!(
            "No matches of at least {:.0}% (search_min_match {floor}); \
             videre config set search-min-match 0 shows every ranked result",
            floor * 100.0
        )
    } else {
        "No matches: the filters left nothing to rank".to_string()
    }
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

    #[test]
    fn people_selects_everyone_with_the_word_in_their_name() {
        let args = parse(&["search", "--people", "Erhan", "deniz"]);
        let dates = resolve_dates(&args).unwrap();
        let got = selection_for(&args, &dates).unwrap();
        assert_eq!(got.people.as_deref(), Some("Erhan"));
        assert_eq!(got.person, None);
        assert_eq!(describe_query(&args, &dates).kind, "text");
    }

    #[test]
    fn search_parses_a_bare_query_image_and_the_full_filter_vocabulary() {
        // A bare text query.
        assert_eq!(
            parse(&["videre", "sunset"]).query.as_deref(),
            Some("sunset")
        );
        // An example image, no positional query.
        assert_eq!(
            parse(&["videre", "--image", "/e.jpg"]).image.as_deref(),
            Some(std::path::Path::new("/e.jpg"))
        );
        // Every filter, including the mark/tag ones, parses together with a query.
        let a = parse(&[
            "videre",
            "cat",
            "--person",
            "Ada",
            "--category",
            "photo",
            "--location",
            "Berlin",
            "--radius",
            "10",
            "--type",
            "image",
            "--ext",
            "jpg",
            "--mime",
            "image/jpeg",
            "--path",
            "/x",
            "--has",
            "gps",
            "--missing",
            "date",
            "--rating",
            "3",
            "--pick",
            "keep",
            "--label",
            "Green",
            "--like",
            "--tag",
            "beach",
        ]);
        assert_eq!(a.person.as_deref(), Some("Ada"));
        assert_eq!(a.marks.rating, Some(3));
        assert_eq!(a.marks.label.as_deref(), Some("Green"));
        assert!(a.marks.like);
        assert_eq!(a.tags.tags, vec!["beach".to_string()]);
    }

    /// `--image` goes with a query: its filters narrow what the image ranks.
    /// Only a query's words conflict with it, and that is refused at run time
    /// (`tests/search.rs`), because only the compiled query knows its words.
    #[test]
    fn image_parses_alongside_a_query() {
        let a = parse(&["videre", "tag:deniz", "--image", "/e.jpg"]);
        assert_eq!(a.query.as_deref(), Some("tag:deniz"));
        assert!(a.image.is_some());
    }

    #[test]
    fn selection_for_matches_a_hand_built_selection() {
        // The refactor routes search's RowSelection through the shared assembler.
        // For a representative argument set the result must be identical to the
        // hand-built selection search used before (full ISO dates so bound
        // normalisation is a no-op and the expected strings are exact).
        let args = parse(&[
            "videre",
            "--person",
            "Ada",
            "--category",
            "photo",
            "--location",
            "Berlin",
            "--radius",
            "10",
            "--after",
            "2024-01-01T00:00:00",
            "--before",
            "2025-01-01T00:00:00",
            "--type",
            "image",
            "--ext",
            "jpg",
            "--mime",
            "image/jpeg",
            "--path",
            "/x",
            "--has",
            "gps",
            "--missing",
            "date",
            "--rating",
            "3",
            "--pick",
            "keep",
            "--label",
            "Green",
            "--like",
            "--tag",
            "beach",
            "--tag",
            "sea",
        ]);
        let dates = resolve_dates(&args).unwrap();
        let got = selection_for(&args, &dates).unwrap();

        let want = videre_core::selection::RowSelection {
            person: Some("Ada".into()),
            people: None,
            category: Some("photo".into()),
            place: Some(videre_core::selection::PlaceQuery::Named {
                place: "Berlin".into(),
                radius_km: 10.0,
            }),
            place_name: None,
            after: Some("2024-01-01T00:00:00".into()),
            before: Some("2025-01-01T00:00:00".into()),
            has: vec![videre_core::selection::PresenceField::Gps],
            missing: vec![videre_core::selection::PresenceField::Date],
            kinds: vec![videre_core::selection::MediaKind::Image],
            exts: vec!["jpg".into()],
            mimes: vec!["image/jpeg".into()],
            paths: vec!["/x".into()],
            min_rating: Some(3),
            pick: Some(videre_core::marks::Pick::Keep),
            label: Some("Green".into()),
            liked: true,
            tags: vec!["beach".into(), "sea".into()],
            query: None,
        };
        assert_eq!(got.describe(), want.describe());
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
    fn mark_and_tag_filters_are_named_in_a_composed_query() {
        let args = parse(&[
            "videre", "--rating", "4", "--pick", "keep", "--label", "Green", "--like", "--tag",
            "beach", "--tag", "summer",
        ]);
        let q = describe_query(&args, &(None, None));
        assert_eq!(q.kind, "filter");
        assert_eq!(
            q.value,
            "rating=4 pick=keep label=Green like=true tag=beach tag=summer"
        );
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

#[cfg(test)]
mod relevance_tests {
    use super::*;
    use clap::Parser;
    use videre_ml::search::Calibration;

    #[derive(Parser)]
    struct Standalone {
        #[command(flatten)]
        inner: SearchArgs,
    }

    /// 50% at a cosine of 0.1, so the three files below land at about 100%,
    /// 50% and 0.7%.
    const CALIBRATION: Calibration = Calibration {
        scale: 100.0,
        bias: -10.0,
    };

    /// Answers every text query with `[1, 0]` and the calibration above, and
    /// every image with `[1, 0]` and none, the way the real embedders do.
    struct Fake;

    impl QueryEmbedder for Fake {
        fn embed(&self, _model_id: &str, input: QueryInput<'_>) -> Result<QueryVector> {
            Ok(QueryVector {
                vector: vec![1.0, 0.0],
                calibration: matches!(input, QueryInput::Text(_)).then_some(CALIBRATION),
            })
        }
    }

    /// deniz, kumsal and kedi at cosines 0.2, 0.1 and 0.05 to `[1, 0]`.
    fn library(config: &str) -> (tempfile::TempDir, CommandContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("kütüphane");
        std::fs::create_dir_all(root.join(".videre")).unwrap();
        std::fs::write(root.join(".videre/config.toml"), config).unwrap();
        let library = std::sync::Arc::new(
            videre_core::library::LibraryContext::new(&root, &temp.path().join("cache")).unwrap(),
        );
        let conn = videre_core::library_db::initialize(&library).unwrap();
        let model = &library.settings.default_model;
        let path = videre_core::embeddings_db::db_path_in(&library, model).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let store = Connection::open(&path).unwrap();
        store
            .execute_batch(
                "CREATE TABLE embeddings
                 (hash TEXT PRIMARY KEY, model_id TEXT NOT NULL, embedding BLOB NOT NULL);",
            )
            .unwrap();
        for (name, cos) in [("deniz", 0.2f32), ("kumsal", 0.1), ("kedi", 0.05)] {
            conn.execute(
                "INSERT INTO file_hashes (path, hash, ext, size_bytes) VALUES (?1, ?2, 'jpg', 1)",
                rusqlite::params![root.join(format!("{name}.jpg")).to_string_lossy(), name],
            )
            .unwrap();
            let v = [cos, (1.0 - cos * cos).sqrt()];
            store
                .execute(
                    "INSERT INTO embeddings VALUES (?1, ?2, ?3)",
                    rusqlite::params![name, model, vectors::to_f16_bytes(&v)],
                )
                .unwrap();
        }
        let ctx = CommandContext {
            library,
            invocation_dir: root,
            source: crate::command_context::LibrarySource::Cwd,
        };
        (temp, ctx)
    }

    fn search(ctx: &CommandContext, argv: &[&str]) -> Vec<(String, f32)> {
        let mut full = vec!["search"];
        full.extend_from_slice(argv);
        let args = Standalone::parse_from(full).inner;
        let out = run_json_in(&args, &Fake, ctx).unwrap();
        assert_eq!(out.count, out.results.len());
        assert_eq!(
            out.total_matches,
            out.results.len(),
            "the count is what passed"
        );
        out.results
            .iter()
            .map(|h| (h.hash.clone().unwrap(), h.score.unwrap()))
            .collect()
    }

    /// No floor by default: a weak match is still a result, ranked last and
    /// scored as weak. On real photos the best match for a one-word query
    /// can score 1%, so a default floor hid what the user asked for.
    #[test]
    fn a_text_search_scores_the_match_probability_and_keeps_every_ranked_match() {
        let (_t, ctx) = library("");
        let got = search(&ctx, &["deniz"]);
        let names: Vec<&str> = got.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(names, ["deniz", "kumsal", "kedi"], "{got:?}");
        assert!(got[0].1 > 0.99, "{got:?}");
        assert!((got[1].1 - 0.5).abs() < 0.02, "{got:?}");
        assert!(got[2].1 < 0.01, "kedi is a 0.7% match: {got:?}");
    }

    /// An empty text result names the floor only when one is set; with none
    /// it was the filters that left nothing, and pointing at the setting
    /// would send the user after the wrong cause.
    #[test]
    fn an_empty_text_result_names_the_floor_only_when_one_is_set() {
        let none = empty_text_note(0.0);
        assert!(!none.contains("search_min_match"), "{none}");
        assert!(none.contains("filters"), "{none}");
        let set = empty_text_note(0.1);
        assert!(set.contains("at least 10%"), "{set}");
        assert!(set.contains("search-min-match 0"), "{set}");
    }

    #[test]
    fn the_text_cutoff_is_the_library_s_setting() {
        let (_t, ctx) = library("search_min_match = 0.1\n");
        let names: Vec<String> = search(&ctx, &["deniz"])
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(names, ["deniz", "kumsal"], "a set floor drops weak matches");
        let (_t, ctx) = library("search_min_match = 0.9\n");
        let got = search(&ctx, &["deniz"]);
        assert_eq!(got.len(), 1, "{got:?}");
    }

    #[test]
    fn an_image_search_keeps_cosines_and_its_own_floor() {
        let image = "/dev/null";
        let (_t, ctx) = library("");
        let got = search(&ctx, &["--image", image]);
        assert_eq!(got.len(), 3, "no floor by default: {got:?}");
        assert!(
            (got[0].1 - 0.2).abs() < 0.01,
            "a cosine, not a probability: {got:?}"
        );
        let (_t, ctx) = library("similar_min_score = 0.08\n");
        let names: Vec<String> = search(&ctx, &["--image", image])
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(names, ["deniz", "kumsal"]);
    }
}
