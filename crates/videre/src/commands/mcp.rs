use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

use videre::types::SCHEMA_VERSION;

#[derive(clap::Args)]
pub struct McpArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,
}

pub fn run(args: McpArgs) -> Result<()> {
    let db = super::resolve_reader_db_must_exist(args.db)?;
    eprintln!("videre mcp: serving {}", db.display());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let service = VidereServer::new(db).serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

#[derive(Clone)]
struct VidereServer {
    db: PathBuf,
    embedder: Arc<std::sync::Mutex<Option<videre_ml::model::Embedder>>>,
    tool_router: ToolRouter<Self>,
}

impl VidereServer {
    fn new(db: PathBuf) -> Self {
        Self {
            db,
            embedder: Arc::new(std::sync::Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }
}

/// Success: structured_content carries the document, content carries the same
/// JSON as text for clients that ignore structured content.
fn json_result(doc: &impl Serialize) -> Result<CallToolResult, McpError> {
    let value = serde_json::to_value(doc).map_err(|e| McpError::internal_error(e.to_string(), None))?;
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

fn table_exists(conn: &Connection, name: &str) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

fn build_stats(db: &std::path::Path) -> anyhow::Result<StatsJson> {
    let conn = videre_core::db::open_wal(db)?;

    let (total_files, total_size_bytes, unique_hashes, files_with_gps, exif_date_range) =
        if table_exists(&conn, "file_hashes")? {
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

    let embedded_count: u64 = if table_exists(&conn, "embeddings")? {
        conn.query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get::<_, i64>(0))? as u64
    } else {
        0
    };

    let (faces_count, people) = if table_exists(&conn, "faces")? {
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

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SearchParams {
    /// Text query, e.g. "sunset on beach" (requires prior 'videre embed')
    #[serde(default)]
    query: Option<String>,
    /// Person name, confirmed faces only (requires 'videre faces' + labeling)
    #[serde(default)]
    person: Option<String>,
    /// Path to a local example image to search by (requires prior 'videre embed')
    #[serde(default)]
    image_path: Option<String>,
    /// Maximum matched hashes to return (default 20); paths may exceed this
    /// when duplicate files share a hash
    #[serde(default)]
    top_k: Option<usize>,
}

fn build_search(
    db: &std::path::Path,
    embedder_cell: &std::sync::Mutex<Option<videre_ml::model::Embedder>>,
    params: &SearchParams,
) -> anyhow::Result<crate::commands::search::SearchJson> {
    use crate::commands::search::{self as search_cmd, QueryJson};

    let mode_count = [
        params.query.is_some(),
        params.person.is_some(),
        params.image_path.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    anyhow::ensure!(
        mode_count == 1,
        "provide exactly one of 'query', 'person', or 'image_path'"
    );

    let conn = videre_core::db::open_wal(db)?;
    let top_k = params.top_k.unwrap_or(20);

    let (query, results) = if let Some(name) = &params.person {
        let hits = search_cmd::person_hits(&conn, name)?;
        (QueryJson { kind: "person", value: name.clone() }, hits)
    } else {
        // Corpus first (fails fast without embeddings, before any model load).
        let corpus = search_cmd::load_corpus(&conn, db)?;

        let (query_vec, query) = {
            let mut guard = embedder_cell
                .lock()
                .map_err(|_| anyhow::anyhow!("embedder lock poisoned"))?;
            if guard.is_none() {
                *guard = Some(videre_ml::model::Embedder::load(
                    videre_ml::device::best_device(),
                )?);
            }
            let embedder = guard.as_ref().expect("just initialized");

            if let Some(text) = &params.query {
                (
                    embedder.embed_text(text)?,
                    QueryJson { kind: "text", value: text.clone() },
                )
            } else {
                let img =
                    std::path::PathBuf::from(params.image_path.as_ref().expect("mode checked"));
                (
                    videre_ml::model::embed_image_file(embedder, &img)?,
                    QueryJson { kind: "image", value: img.display().to_string() },
                )
            }
            // guard drops here, before ranked_hits runs
        };

        let hits = search_cmd::ranked_hits(&conn, &corpus, &query_vec, top_k)?;
        (query, hits)
    };

    Ok(search_cmd::SearchJson {
        schema_version: SCHEMA_VERSION,
        query,
        count: results.len(),
        results,
    })
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

    /// Semantic and person search over the indexed library.
    #[tool(
        description = "Search the videre library. Provide exactly one of: 'query' (semantic text search), 'person' (labeled person name), or 'image_path' (find similar to a local image). Returns per-path results with hash and cosine score (person hits carry path only). The first text/image search loads the embedding model and may be slow; later calls are fast. Requires 'videre embed' for text/image and 'videre faces' + labeling for person."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        let embedder = self.embedder.clone();
        match blocking(move || build_search(&db, &embedder, &params)).await? {
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
