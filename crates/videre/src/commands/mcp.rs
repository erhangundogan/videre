use anyhow::Result;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

use videre::types::SCHEMA_VERSION;

#[derive(clap::Args)]
pub struct McpArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Embedding model to serve searches from (default: 'videre config set model',
    /// else the built-in default). Bound once at startup, like --db, so a
    /// bad value fails before the server accepts a single call.
    #[arg(long)]
    model: Option<String>,
}

pub fn run(args: McpArgs) -> Result<()> {
    let db = super::resolve_reader_db_must_exist(args.db)?;
    let model_id = videre_core::embeddings::resolve_model_id(args.model.as_deref())?;
    // Probed at startup so a typo in --model is visible immediately rather
    // than minutes later inside an agent's search result, but NOT fatal:
    // find_duplicates and stats do not touch embeddings, and refusing to
    // start would make this command useless on a library that has been
    // scanned but never embedded, which is a perfectly normal state. The
    // search tool re-checks per call and returns a clear tool-level error.
    let embeddings_ready = {
        let probe = videre_core::db::open_wal(&db)?;
        match videre_core::embeddings_db::attach_for_read(&probe, &db, &model_id) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("videre mcp: search unavailable ({e})");
                false
            }
        }
    };
    eprintln!(
        "videre mcp: serving {} (model {model_id}{})",
        db.display(),
        if embeddings_ready {
            ""
        } else {
            ", no embeddings"
        }
    );

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let service = VidereServer::new(db, model_id).serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

#[derive(Clone)]
struct VidereServer {
    db: PathBuf,
    model_id: String,
    embedder: Arc<std::sync::Mutex<Option<videre_ml::model::Embedder>>>,
    tool_router: ToolRouter<Self>,
}

impl VidereServer {
    fn new(db: PathBuf, model_id: String) -> Self {
        Self {
            db,
            model_id,
            embedder: Arc::new(std::sync::Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }
}

/// Success: structured_content carries the document, content carries the same
/// JSON as text for clients that ignore structured content.
fn json_result(doc: &impl Serialize) -> Result<CallToolResult, McpError> {
    let value =
        serde_json::to_value(doc).map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::structured(value))
}

/// Runtime failure: a tool-level error (isError: true) carrying the anyhow
/// chain, exactly the message text the CLI would print. The server stays up.
fn tool_error(e: &anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))])
}

/// Run sync/heavy work (SQLite, model inference) off the protocol loop.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<anyhow::Result<T>, McpError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| McpError::internal_error(format!("task panic: {e}"), None))
}

#[derive(Debug, Serialize)]
struct StatsJson {
    schema_version: u32,
    total_files: u64,
    total_size_bytes: u64,
    unique_hashes: u64,
    embedded_count: u64,
    faces_count: u64,
    people: Vec<String>,
    files_with_gps: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exif_date_range: Option<DateRange>,
}

#[derive(Debug, Serialize)]
struct DateRange {
    min: String,
    max: String,
}

fn build_stats(db: &std::path::Path) -> anyhow::Result<StatsJson> {
    let conn = videre_core::db::open_wal(db)?;

    let (total_files, total_size_bytes, unique_hashes, files_with_gps, exif_date_range) =
        if videre_core::db::table_exists(&conn, "file_hashes")? {
            let (files, size, hashes, gps): (i64, i64, i64, i64) = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0), COUNT(DISTINCT hash),
                        COUNT(CASE WHEN gps_lat IS NOT NULL AND gps_lon IS NOT NULL THEN 1 END)
                 FROM file_hashes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let range: (Option<String>, Option<String>) = conn.query_row(
                "SELECT MIN(exif_date), MAX(exif_date) FROM file_hashes WHERE exif_date IS NOT NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let range = match range {
                (Some(min), Some(max)) => Some(DateRange { min, max }),
                _ => None,
            };
            (files as u64, size as u64, hashes as u64, gps as u64, range)
        } else {
            (0, 0, 0, 0, None)
        };

    // Per model, not a bare COUNT(*). With the table now living in a
    // per-model database, an unfiltered count would either miss it entirely
    // or, once several models exist, double-count hashes embedded by more
    // than one of them.
    let embedded_count: u64 = videre_core::embeddings_db::counts_by_model(db)
        .map(|counts| counts.iter().map(|c| c.count.max(0) as u64).sum())
        .unwrap_or(0);

    let (faces_count, people) = if videre_core::db::table_exists(&conn, "faces")? {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM faces", [], |r| r.get(0))?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT person_label FROM faces
             WHERE confirmed = 1 AND person_label IS NOT NULL
             ORDER BY person_label",
        )?;
        let people: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        (count as u64, people)
    } else {
        (0, Vec::new())
    };

    Ok(StatsJson {
        schema_version: SCHEMA_VERSION,
        total_files,
        total_size_bytes,
        unique_hashes,
        embedded_count,
        faces_count,
        people,
        files_with_gps,
        exif_date_range,
    })
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FindDuplicatesParams {
    /// Also return perceptual-hash near-duplicate clusters (review-only)
    #[serde(default)]
    include_similar: bool,
}

/// Every field is optional and every filter is ANDed. The doc comments are the
/// descriptions an agent actually reads in the tool schema, so they carry the
/// accepted forms rather than pointing elsewhere.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SearchParams {
    /// Ranker: semantic text query, e.g. "sunset on beach" (requires prior
    /// 'videre embed'). Cannot be combined with image_path.
    #[serde(default)]
    query: Option<String>,
    /// Ranker: path to a local example image to find similar files to
    /// (requires prior 'videre embed'). Cannot be combined with query.
    #[serde(default)]
    image_path: Option<String>,
    /// Filter: only files containing this labeled person, confirmed faces only
    /// (requires 'videre faces' + labeling)
    #[serde(default)]
    person: Option<String>,
    /// Filter: only files classified as this category, one of photo,
    /// screenshot, document, meme, unknown (requires a prior 'videre classify')
    #[serde(default)]
    category: Option<String>,
    /// Filter: place name, e.g. "Berlin, Germany", combined with radius_km.
    /// Reaches the network on a cache miss and writes the result to the
    /// geocode_cache table; every later query for the same place is local.
    #[serde(default)]
    location: Option<String>,
    /// Radius in km around location (default 20). Ignored without location.
    #[serde(default)]
    radius_km: Option<f64>,
    /// Filter: inclusive lower date bound, YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS
    #[serde(default)]
    after: Option<String>,
    /// Filter: exclusive upper date bound, YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS,
    /// so adjacent ranges never both match the boundary instant
    #[serde(default)]
    before: Option<String>,
    /// Filter: shorthand for a whole year, month or day, as YYYY, YYYY-MM or
    /// YYYY-MM-DD. Mutually exclusive with after/before.
    #[serde(default)]
    date: Option<String>,
    /// Filter: media kind, "image" or "video". Repeatable as a list; several
    /// kinds match any of them.
    #[serde(default)]
    media_type: Vec<String>,
    /// Filter: file extension without the dot, e.g. "mov". Several extensions
    /// match any of them.
    #[serde(default)]
    ext: Vec<String>,
    /// Filter: exact mime type, e.g. "video/quicktime". Note this is not
    /// interchangeable with media_type: DNG files report image/tiff.
    #[serde(default)]
    mime: Vec<String>,
    /// Filter: only files under these directories.
    #[serde(default)]
    path: Vec<String>,
    /// Result order: comma-separated field[:asc|desc] over relevance, distance,
    /// date and size, e.g. "distance:asc,date:desc". Defaults are relevance,
    /// date and size descending and distance ascending. relevance needs a
    /// query or image_path; distance needs location.
    #[serde(default)]
    sort: Option<String>,
    /// Maximum results to return (default 20)
    #[serde(default)]
    top_k: Option<usize>,
}

/// The MCP server's embedder: loaded on the first ranking search and kept for
/// the life of the process, so only that one call pays the load. The CLI's
/// equivalent (`search::FreshEmbedder`) loads per invocation instead; that
/// difference is the only reason the pipeline takes an embedder at all.
struct CachedEmbedder<'a>(&'a std::sync::Mutex<Option<videre_ml::model::Embedder>>);

impl crate::commands::search::QueryEmbedder for CachedEmbedder<'_> {
    fn embed(
        &self,
        model_id: &str,
        input: crate::commands::search::QueryInput<'_>,
    ) -> anyhow::Result<Vec<f32>> {
        use crate::commands::search::QueryInput;

        let mut guard = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(videre_ml::model::Embedder::load(
                videre_ml::device::best_device(),
                model_id,
            )?);
        }
        let embedder = guard.as_ref().expect("just initialized");
        match input {
            QueryInput::Text(text) => embedder.embed_text(text),
            QueryInput::Image(path) => videre_ml::model::embed_image_file(embedder, path),
        }
    }
}

/// Runs an MCP `search` call through exactly the pipeline `videre search
/// --json` uses, by building the same `SearchArgs` the CLI parses into. The
/// only MCP-specific parts are the two validity checks, whose wording names
/// tool parameters rather than CLI flags, and the cached embedder.
fn build_search(
    db: &std::path::Path,
    model_id: &str,
    embedder_cell: &std::sync::Mutex<Option<videre_ml::model::Embedder>>,
    params: &SearchParams,
) -> anyhow::Result<crate::commands::search::SearchJson> {
    use crate::commands::search::{self as search_cmd, SearchArgs};

    // Filters compose; rankers do not, since only one thing can order a result
    // list. Checked here rather than left to the CLI's own guard so the message
    // names the parameters the caller actually passed.
    anyhow::ensure!(
        !(params.query.is_some() && params.image_path.is_some()),
        "provide at most one ranker: 'query' or 'image_path', not both"
    );
    // Hand-maintained, and it must not fall behind the vocabulary: forgetting a
    // new axis here rejects a valid query rather than running it, which is the
    // visible failure. The CLI derives the same check from the selection
    // itself, so this is the one place a predicate can be missed.
    let any_filter = params.person.is_some()
        || params.category.is_some()
        || params.location.is_some()
        || params.after.is_some()
        || params.before.is_some()
        || params.date.is_some()
        || !params.media_type.is_empty()
        || !params.ext.is_empty()
        || !params.mime.is_empty()
        || !params.path.is_empty();
    anyhow::ensure!(
        params.query.is_some() || params.image_path.is_some() || any_filter,
        "provide at least one of 'query', 'image_path', 'person', 'category', \
         'location', 'after', 'before', 'date', 'media_type', 'ext', 'mime' \
         or 'path'; an unfiltered search would return the whole library"
    );

    let args = SearchArgs {
        // Built from the same flag groups the CLI flattens, so the two surfaces
        // resolve through one vocabulary rather than drifting apart, which is
        // the rule in CLAUDE.md that exists because they have drifted before.
        media: super::selection_args::MediaArgs {
            media_type: params.media_type.clone(),
            ext: params.ext.clone(),
            mime: params.mime.clone(),
        },
        paths: super::selection_args::PathArgs {
            path: params.path.iter().map(std::path::PathBuf::from).collect(),
        },
        // Both bound at startup, so a call cannot retarget the server.
        db: Some(db.to_path_buf()),
        model: Some(model_id.to_string()),
        query: params.query.clone(),
        image: params.image_path.as_deref().map(std::path::PathBuf::from),
        person: params.person.clone(),
        category: params.category.clone(),
        location: params.location.clone(),
        radius: params.radius_km.unwrap_or(20.0),
        after: params.after.clone(),
        before: params.before.clone(),
        date: params.date.clone(),
        sort: params.sort.clone(),
        top_k: params.top_k.unwrap_or(20),
        scores: false,
        // json: true keeps the pipeline off stderr. An empty result is
        // reported as count 0, which is what the protocol channel carries.
        json: true,
    };

    search_cmd::run_json(&args, &CachedEmbedder(embedder_cell))
}

#[tool_router]
impl VidereServer {
    /// Library orientation summary: file/size/hash counts, embedding and face
    /// counts, labeled people, GPS coverage, and the EXIF date range. Cheap;
    /// call this first to understand what the library contains.
    #[tool(
        description = "Summary of the videre library: total files, total size, unique hashes, embedded count, face count, labeled people, files with GPS, and the EXIF date range. Results reflect the database (kept fresh by 'videre watch' or CLI scans)."
    )]
    async fn stats(&self) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        match blocking(move || build_stats(&db)).await? {
            Ok(doc) => json_result(&doc),
            Err(e) => Ok(tool_error(&e)),
        }
    }

    /// Exact-duplicate groups from the database, instantly (no scan).
    #[tool(
        description = "Exact-duplicate groups from the videre database. Each group has 'keep' (the oldest file, safe to keep) and 'remove' (byte-identical copies, safe to delete). With include_similar=true, also returns review-only near-duplicate clusters ('files' arrays; NOT safe to auto-delete). Results reflect the last scan: verify paths still exist before acting."
    )]
    async fn find_duplicates(
        &self,
        Parameters(params): Parameters<FindDuplicatesParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        match blocking(move || super::build_find_duplicates(&db, params.include_similar)).await? {
            Ok(doc) => json_result(&doc),
            Err(e) => Ok(tool_error(&e)),
        }
    }

    /// Composed search: filters narrow, an optional ranker orders.
    #[tool(
        description = "Search the videre library with composable filters. Every filter given is ANDed, so they only ever narrow: 'person' (labeled person, confirmed faces only), 'category' (photo/screenshot/document/meme/unknown), 'location' + 'radius_km', and a date range as either 'after'/'before' (before is exclusive) or the 'date' shorthand YYYY, YYYY-MM or YYYY-MM-DD. At most one ranker may be given: 'query' (semantic text) or 'image_path' (similar to a local image), never both; a ranker orders the filtered survivors by cosine score. At least one filter or ranker is required. 'sort' takes a comma-separated field[:asc|desc] list over relevance, distance, date and size, e.g. \"distance:asc,date:desc\"; relevance needs a ranker and distance needs 'location'. Results carry path, hash, and where applicable score, distance_km and date. Dates match the EXIF capture date when present and valid, otherwise the file mtime. WARNING: 'location' forward-geocodes the place name over the network on a cache miss and writes the answer to the geocode_cache table; repeats are local. Requires 'videre embed' for query/image_path, 'videre faces' + labeling for person, and 'videre classify' for category. The first text/image search loads the embedding model and may be slow; later calls are fast."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let model_id = self.model_id.clone();
        let embedder = self.embedder.clone();
        match blocking(move || build_search(&db, &model_id, &embedder, &params)).await? {
            Ok(doc) => json_result(&doc),
            Err(e) => Ok(tool_error(&e)),
        }
    }
}

#[tool_handler(router = self.tool_router.clone())]
impl ServerHandler for VidereServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Read-only query tools over a videre media library (SQLite). \
                 Results reflect the last scan; verify paths still exist before \
                 acting on them, and run 'videre scan'/'videre watch' to freshen.",
            )
            .with_server_info(Implementation::new("videre", env!("CARGO_PKG_VERSION")))
    }
}
