# `videre mcp` Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `videre mcp` subcommand serving three read-only MCP tools (`search`, `find_duplicates`, `stats`) over stdio so LLM agents can drive the library against the existing SQLite database.

**Architecture:** Per the spec at `docs/superpowers/specs/2026-07-14-mcp-server-design.md` (as amended for the home-dir slice: `videre mcp [--db <path>]`, resolved like every other reader). The official `rmcp` SDK owns the protocol; handlers are thin wrappers over the same functions the CLI uses. Tool results reuse the CLI `--json` document shapes and `schema_version: 1`. All heavy/sync work (SQLite, model inference) runs inside `tokio::task::spawn_blocking`; the SigLIP embedder is lazily loaded once and cached in server state behind `Arc<std::sync::Mutex<Option<Embedder>>>`. Runtime failures become MCP tool-level errors (`isError: true`) carrying the rendered anyhow chain; the server never dies on a bad call.

**Tech Stack:** Rust; new dependency `rmcp = "2.2"` (default features: `base64`, `macros`, `server`; the `server` feature brings the stdio transport and schemars) plus a direct `schemars` dependency at the SAME major version rmcp resolves (verify with `cargo tree`, see Task 3). `tokio = { features = ["full"] }` is already a videre dependency. Baseline: `cargo test --workspace` = 159 passing on `main` at `8f40bf7`. Expected after all tasks: 169.

**API notes verified against rmcp 2.2.0** (adapt to compiler guidance if minor details drifted, but these are the shapes): `use rmcp::{ServiceExt, transport::stdio};` and `server.serve(stdio()).await?` then `service.waiting().await?`. Tools: `#[tool_router]` on an impl block, `#[tool(description = "...")]` per method, params via `Parameters<T>` where `T: serde::Deserialize + schemars::JsonSchema`, return `Result<CallToolResult, McpError>` (`use rmcp::ErrorData as McpError`). `#[tool_handler]` on the `impl ServerHandler` block, with `get_info()` returning `ServerInfo`. `CallToolResult` has public fields `content: Vec<ContentBlock>`, `structured_content: Option<serde_json::Value>`, `is_error`, plus constructors `structured(value)` and `error(content)`.

**House rules:** never use the em dash character anywhere; no Co-Authored-By trailer or "Generated with" line; use the exact commit messages given.

**Branch:** work on a new branch `mcp-server` off `main`:

```bash
cd /Users/erhangundogan/projects/rust/videre
git checkout -b mcp-server
```

---

### Task 1: `FileRecord` loader in `sqlite_output`

`find_duplicates` needs `file_hashes` rows as `Vec<FileRecord>` (the CLI gets them from a filesystem scan; the MCP server reads them back from the db). Add the reader next to the writer.

**Files:**
- Modify: `crates/videre/src/sqlite_output.rs`

- [ ] **Step 1: Write the failing tests**

`sqlite_output.rs` currently has no test module. Append one (tempfile is already a dev-dependency of the videre crate):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: hash.to_string(),
            size_bytes: 10,
            created_at: Some("2020-01-01T00:00:00+00:00".to_string()),
            modified_at: Some("2021-01-01T00:00:00+00:00".to_string()),
            ext: "jpg".to_string(),
            phash: Some(u64::MAX), // exercises the i64 sign-cast roundtrip
            exif_date: Some("2019-06-01T10:00:00".to_string()),
            gps_lat: Some(48.85),
            gps_lon: Some(2.35),
            width: Some(100),
            height: Some(80),
        }
    }

    #[test]
    fn load_records_roundtrips_write_records() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let written = vec![rec("/a.jpg", "h1"), rec("/b.jpg", "h2")];
        write_records(&written, &db).unwrap();

        let mut loaded = load_records(&db).unwrap();
        loaded.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].path, "/a.jpg");
        assert_eq!(loaded[0].hash, "h1");
        assert_eq!(loaded[0].size_bytes, 10);
        assert_eq!(loaded[0].phash, Some(u64::MAX));
        assert_eq!(loaded[0].exif_date.as_deref(), Some("2019-06-01T10:00:00"));
        assert_eq!(loaded[0].gps_lat, Some(48.85));
        assert_eq!(loaded[0].width, Some(100));
    }

    #[test]
    fn load_records_empty_table_yields_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        write_records(&[], &db).unwrap(); // creates the table, writes nothing
        assert!(load_records(&db).unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --lib sqlite_output::`
Expected: COMPILE ERROR (`load_records` not found).

- [ ] **Step 3: Implement**

Add to `crates/videre/src/sqlite_output.rs` after `write_records` (note: SELECT names only the columns `FileRecord` owns, so dbs that gained the `location_name` column from `videre report`'s migration load fine, as do dbs that never did):

```rust
/// Read every file_hashes row back as FileRecords (the inverse of write_records;
/// used by consumers that need records without re-scanning the filesystem).
pub fn load_records(db_path: &Path) -> Result<Vec<FileRecord>> {
    let conn = videre_core::db::open_wal(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT path, hash, size_bytes, created_at, modified_at, ext,
                phash, exif_date, gps_lat, gps_lon, width, height
         FROM file_hashes",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FileRecord {
            path: row.get(0)?,
            hash: row.get(1)?,
            size_bytes: row.get::<_, i64>(2)? as u64,
            created_at: row.get(3)?,
            modified_at: row.get(4)?,
            ext: row.get(5)?,
            phash: row.get::<_, Option<i64>>(6)?.map(|p| p as u64),
            exif_date: row.get(7)?,
            gps_lat: row.get(8)?,
            gps_lon: row.get(9)?,
            width: row.get(10)?,
            height: row.get(11)?,
        })
    })?;
    rows.collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --lib sqlite_output::`
Expected: PASS (2 tests).

Run: `cargo test --workspace`
Expected: PASS, 161 total (159 baseline + 2).

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/sqlite_output.rs
git commit -m "feat: load_records reads file_hashes rows back as FileRecords"
```

---

### Task 2: search pipeline refactor (no behavior change)

The MCP search tool needs the query pipeline WITHOUT the model-loading baked in (the server caches the embedder; the CLI loads per invocation). Split `collect_hits` into reusable pub(crate) pieces and make the result structs pub(crate). CLI behavior must stay byte-identical, including the order load-corpus-then-load-model (so a db without embeddings errors BEFORE any model download).

**Files:**
- Modify: `crates/videre/src/commands/search.rs`

- [ ] **Step 1: Refactor**

In `crates/videre/src/commands/search.rs`:

1. Make the three structs and their fields `pub(crate)`:

```rust
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
```

2. Add `use rusqlite::Connection;` to the imports and extract three pub(crate) helpers (bodies lifted verbatim from today's `collect_hits`):

```rust
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
pub(crate) fn load_corpus(conn: &Connection, db_label: &str) -> Result<Vec<(String, Vec<f32>)>> {
    let corpus_raw = embeddings::load_embeddings(conn, model::MODEL_ID)?;
    anyhow::ensure!(
        !corpus_raw.is_empty(),
        "no embeddings found in {} for model {}; run videre embed first",
        db_label,
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
```

(If `search::top_k`'s parameter types differ slightly, e.g. it takes `&Vec<...>` or returns references, match the existing call site exactly; the current `collect_hits` compiles against it, so lift its exact expressions.)

3. Rewrite `collect_hits` as a thin composition (identical observable behavior):

```rust
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

    let corpus = load_corpus(&conn, &db.display().to_string())?;

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
```

- [ ] **Step 2: Run the full suite to prove no behavior change**

Run: `cargo test --workspace`
Expected: PASS, 161 total, 0 failed (this task adds no tests; the 9 person_search integration tests and 2 search unit tests are the regression guard).

Also: `cargo build --workspace 2>&1 | grep -i warning` must be empty (the new pub(crate) helpers all have callers: collect_hits).

- [ ] **Step 3: Commit**

```bash
git add crates/videre/src/commands/search.rs
git commit -m "refactor: split search pipeline into reusable person_hits/load_corpus/ranked_hits"
```

---

### Task 3: mcp skeleton + `stats` tool

**Files:**
- Modify: `crates/videre/Cargo.toml`, `crates/videre/src/commands/mod.rs`, `crates/videre/src/main.rs`
- Create: `crates/videre/src/commands/mcp.rs`
- Test: `crates/videre/tests/mcp.rs` (new file)

- [ ] **Step 1: Add dependencies**

In `crates/videre/Cargo.toml` `[dependencies]`, add:

```toml
rmcp = "2.2"
schemars = "1"
```

Then run `cargo build -p videre` and verify the schemars versions agree:

```bash
cargo tree -p videre -i schemars 2>&1 | head -5
```

Expected: ONE schemars version in the tree, used by both videre and rmcp. If rmcp resolves a different schemars major (e.g. 0.8), change videre's `schemars` line to that major so the `JsonSchema` derive matches what rmcp consumes, and note it in your report.

- [ ] **Step 2: Write the failing integration tests**

Create `crates/videre/tests/mcp.rs`. This file carries the raw line-delimited JSON-RPC harness (no client library) plus a fixture db builder used by all mcp tests:

```rust
use rusqlite::Connection;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use tempfile::tempdir;

fn videre_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    p.pop(); // debug/
    p.push("videre");
    p
}

/// Fixture: 4 files, one exact-duplicate pair (hash1: alice1 older KEEP, dup newer),
/// 3 confirmed faces (Alice x2, Bob x1), empty embeddings table, no GPS, no exif.
fn make_db(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
         bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
         cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
         is_primary INTEGER DEFAULT 0);
         CREATE TABLE embeddings (hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
         embedding BLOB NOT NULL, embedded_at TEXT NOT NULL);
         INSERT INTO file_hashes (path, hash, size_bytes, modified_at, ext) VALUES
           ('/tmp/alice1.jpg', 'hash1', 10, '2020-01-01T00:00:00+00:00', 'jpg'),
           ('/tmp/alice1_copy.jpg', 'hash1', 10, '2024-01-01T00:00:00+00:00', 'jpg'),
           ('/tmp/alice2.jpg', 'hash2', 10, '2021-01-01T00:00:00+00:00', 'jpg'),
           ('/tmp/bob.jpg', 'hash3', 10, '2022-01-01T00:00:00+00:00', 'jpg');
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) VALUES
           ('hash1', '0,0,50,50', X'0000', 'Alice', 1),
           ('hash2', '0,0,50,50', X'0000', 'Alice', 1),
           ('hash3', '0,0,50,50', X'0000', 'Bob', 1);",
    )
    .unwrap();
    db
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl McpClient {
    /// Spawn `videre mcp --db <db>` and complete the initialize handshake.
    fn start(db: &std::path::Path) -> Self {
        let mut child = Command::new(videre_bin())
            .arg("mcp")
            .arg("--db")
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn videre mcp");
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut client = McpClient { child, stdin, reader };
        client.initialize();
        client
    }

    fn send(&mut self, msg: serde_json::Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self) -> serde_json::Value {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.reader.read_line(&mut line).expect("read from server");
            assert!(n > 0, "server closed stdout unexpectedly");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return serde_json::from_str(trimmed).expect("each stdout line must be valid JSON");
        }
    }

    /// Send a request and read messages until the response with our id arrives
    /// (skipping any server-initiated notifications).
    fn request(&mut self, id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self.recv();
            if msg.get("id") == Some(&json!(id)) {
                return msg;
            }
        }
    }

    fn initialize(&mut self) {
        let resp = self.request(
            0,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "videre-test", "version": "0"}
            }),
        );
        assert_eq!(
            resp["result"]["serverInfo"]["name"], "videre",
            "unexpected initialize response: {resp}"
        );
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    fn call_tool(&mut self, id: u64, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.request(id, "tools/call", json!({"name": name, "arguments": arguments}))
    }

    fn shutdown(mut self) {
        drop(self.stdin); // EOF: normal client shutdown
        let _ = self.child.wait();
    }
}
```

(If the server rejects `protocolVersion: "2025-06-18"`, read the error it returns, use the newest version it advertises, and note the change in your report.)

Then the three tests for this task:

```rust
#[test]
fn initialize_lists_exactly_three_tools() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let mut client = McpClient::start(&db);
    let resp = client.request(1, "tools/list", json!({}));
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["find_duplicates", "search", "stats"]);
    client.shutdown();
}

#[test]
fn stats_tool_returns_counts() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let mut client = McpClient::start(&db);
    let resp = client.call_tool(2, "stats", json!({}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["schema_version"], 1, "full response: {resp}");
    assert_eq!(doc["total_files"], 4);
    assert_eq!(doc["total_size_bytes"], 40);
    assert_eq!(doc["unique_hashes"], 3);
    assert_eq!(doc["embedded_count"], 0);
    assert_eq!(doc["faces_count"], 3);
    assert_eq!(doc["people"], json!(["Alice", "Bob"]));
    assert_eq!(doc["files_with_gps"], 0);
    assert!(doc.get("exif_date_range").is_none(), "no exif dates in fixture");
    // text content mirrors the structured document
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let text_doc: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(&text_doc, doc);
    client.shutdown();
}

#[test]
fn stats_tool_zero_counts_without_optional_tables() {
    // A db with only file_hashes (no embeddings/faces tables): stats must
    // degrade to zero counts, not error.
    let dir = tempdir().unwrap();
    let db = dir.path().join("minimal.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         INSERT INTO file_hashes (path, hash, size_bytes, ext)
           VALUES ('/tmp/only.jpg', 'h1', 5, 'jpg');",
    )
    .unwrap();
    drop(conn);

    let mut client = McpClient::start(&db);
    let resp = client.call_tool(2, "stats", json!({}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["total_files"], 1, "full response: {resp}");
    assert_eq!(doc["embedded_count"], 0);
    assert_eq!(doc["faces_count"], 0);
    assert_eq!(doc["people"], json!([]));
    client.shutdown();
}

#[test]
fn startup_fails_without_db() {
    // bare mcp with an empty VIDERE_HOME: resolved default db does not exist
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("mcp")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("run videre mcp");
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "nothing may be written to the protocol channel");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no database found"), "{stderr}");

    // explicit --db to a nonexistent path fails the same way (mcp is a reader:
    // the resolved db must exist even when explicit)
    let out2 = Command::new(videre_bin())
        .arg("mcp")
        .arg("--db")
        .arg(home.path().join("nope.db"))
        .output()
        .expect("run videre mcp");
    assert!(!out2.status.success());
    assert!(out2.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out2.stderr).contains("no database found"));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p videre --test mcp`
Expected: all 4 FAIL (clap: unrecognized subcommand `mcp`).

- [ ] **Step 4: Implement the skeleton and stats**

Create `crates/videre/src/commands/mcp.rs`:

```rust
use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo},
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
    let db = super::resolve_reader_db(args.db)?;
    // mcp is a reader bound for the whole session: even an explicit --db must
    // exist, or a typo'd path would silently serve an empty library.
    anyhow::ensure!(
        db.exists(),
        "no database found at {}; run 'videre dedupe <dir>' first",
        db.display()
    );
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
    let value = serde_json::to_value(doc)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    let text = value.to_string();
    let mut result = CallToolResult::structured(value);
    result.content = vec![rmcp::model::Content::text(text).into()];
    Ok(result)
}

/// Runtime failure: a tool-level error (isError: true) carrying the anyhow
/// chain, exactly the message text the CLI would print. The server stays up.
fn tool_error(e: &anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![rmcp::model::Content::text(format!("{e:#}")).into()])
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
}

#[tool_handler]
impl ServerHandler for VidereServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Read-only query tools over a videre media library (SQLite). \
                 Results reflect the last scan; verify paths still exist before \
                 acting on them, and run 'videre dedupe'/'videre watch' to freshen."
                    .to_string(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "videre".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}
```

API-drift note (requirement over form): if the compiler objects to `Content::text(..).into()`, the `Implementation` fields, the `Parameters` import path, or the `CallToolResult` field names, consult the rmcp 2.2 docs (`cargo doc -p rmcp --no-deps --open` or docs.rs) and adapt. The REQUIREMENTS are fixed: tool success = structured_content carrying the document plus the same JSON serialized in text content; tool failure = isError result carrying `format!("{e:#}")`; server name "videre" with `CARGO_PKG_VERSION`; tools capability enabled.

Wire in: `crates/videre/src/commands/mod.rs` gains `pub mod mcp;` (alphabetical: after `fix_dates`, before `prune`). `crates/videre/src/main.rs` gains, after `Config`:

```rust
    /// Serve read-only MCP tools (search, find_duplicates, stats) over stdio for LLM agents
    Mcp(commands::mcp::McpArgs),
```

and the dispatch arm:

```rust
        Command::Mcp(args) => commands::mcp::run(args),
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p videre --test mcp`
Expected: PASS (4 tests).

Run: `cargo test --workspace`
Expected: PASS, 165 total (161 + 4). No compiler warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/Cargo.toml Cargo.lock crates/videre/src/commands/mcp.rs crates/videre/src/commands/mod.rs crates/videre/src/main.rs crates/videre/tests/mcp.rs
git commit -m "feat: videre mcp subcommand serving MCP over stdio with a stats tool"
```

---

### Task 4: `find_duplicates` tool

**Files:**
- Modify: `crates/videre/src/commands/mcp.rs`
- Test: `crates/videre/tests/mcp.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/videre/tests/mcp.rs`:

```rust
#[test]
fn find_duplicates_tool_returns_keep_remove_groups() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let mut client = McpClient::start(&db);

    // without include_similar: no similar_groups key
    let resp = client.call_tool(3, "find_duplicates", json!({}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["schema_version"], 1, "full response: {resp}");
    assert_eq!(doc["total_files"], 4);
    let groups = doc["duplicate_groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["hash"], "hash1");
    assert_eq!(groups[0]["keep"]["path"], "/tmp/alice1.jpg", "oldest is KEEP");
    let remove = groups[0]["remove"].as_array().unwrap();
    assert_eq!(remove.len(), 1);
    assert_eq!(remove[0]["path"], "/tmp/alice1_copy.jpg");
    assert!(doc.get("similar_groups").is_none(), "absent without include_similar");

    // with include_similar: key present (empty here, fixture has no phashes)
    let resp2 = client.call_tool(4, "find_duplicates", json!({"include_similar": true}));
    let doc2 = &resp2["result"]["structuredContent"];
    let similar = doc2["similar_groups"].as_array().expect("similar_groups present");
    assert!(similar.is_empty());

    client.shutdown();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre --test mcp find_duplicates`
Expected: FAIL (tool not found: the tools/call response is an error or isError result).

- [ ] **Step 3: Implement**

In `crates/videre/src/commands/mcp.rs`, add near `StatsJson`:

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FindDuplicatesParams {
    /// Also return perceptual-hash near-duplicate clusters (review-only)
    #[serde(default)]
    include_similar: bool,
}

#[derive(Debug, Serialize)]
struct FindDuplicatesJson {
    schema_version: u32,
    total_files: usize,
    duplicate_groups: Vec<videre::types::DupGroupJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    similar_groups: Option<Vec<videre::types::SimilarGroupJson>>,
}

fn build_find_duplicates(db: &std::path::Path, include_similar: bool) -> anyhow::Result<FindDuplicatesJson> {
    let records = videre::sqlite_output::load_records(db)?;
    let total_files = records.len();
    let duplicate_groups = videre::output::find_duplicate_groups(&records)
        .into_iter()
        .map(videre::types::DupGroupJson::from)
        .collect();
    let similar_groups = include_similar.then(|| {
        videre::output::find_similar_groups(&records, 10)
            .into_iter()
            .map(videre::types::SimilarGroupJson::from)
            .collect()
    });
    Ok(FindDuplicatesJson {
        schema_version: SCHEMA_VERSION,
        total_files,
        duplicate_groups,
        similar_groups,
    })
}
```

and inside the `#[tool_router] impl VidereServer` block:

```rust
    /// Exact-duplicate groups from the database, instantly (no scan).
    #[tool(
        description = "Exact-duplicate groups from the videre database. Each group has 'keep' (the oldest file, safe to keep) and 'remove' (byte-identical copies, safe to delete). With include_similar=true, also returns review-only near-duplicate clusters ('files' arrays; NOT safe to auto-delete). Results reflect the last scan: verify paths still exist before acting."
    )]
    async fn find_duplicates(
        &self,
        Parameters(params): Parameters<FindDuplicatesParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        match blocking(move || build_find_duplicates(&db, params.include_similar)).await? {
            Ok(doc) => json_result(&doc),
            Err(e) => Ok(tool_error(&e)),
        }
    }
```

(If the fixture's duplicate group orders keep/remove unexpectedly, remember KEEP = oldest by exif_date, else min(created_at, modified_at); the fixture gives alice1 the older modified_at deliberately.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --test mcp`
Expected: PASS (5 tests).

Run: `cargo test --workspace`
Expected: PASS, 166 total (165 + 1).

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/mcp.rs crates/videre/tests/mcp.rs
git commit -m "feat: find_duplicates mcp tool with keep/remove groups and optional similar clusters"
```

---

### Task 5: `search` tool

**Files:**
- Modify: `crates/videre/src/commands/mcp.rs`
- Test: `crates/videre/tests/mcp.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/videre/tests/mcp.rs`:

```rust
#[test]
fn search_person_tool_returns_document() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let mut client = McpClient::start(&db);
    let resp = client.call_tool(5, "search", json!({"person": "Alice"}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["schema_version"], 1, "full response: {resp}");
    assert_eq!(doc["query"]["kind"], "person");
    assert_eq!(doc["query"]["value"], "Alice");
    assert_eq!(doc["count"], 2);
    let results = doc["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for r in results {
        assert!(r["path"].as_str().unwrap().contains("alice"));
        assert!(r.get("hash").is_none(), "person hits omit hash: {r}");
        assert!(r.get("score").is_none(), "person hits omit score: {r}");
    }
    client.shutdown();
}

#[test]
fn search_text_without_embeddings_is_tool_error_and_server_survives() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path()); // embeddings table exists but is empty
    let mut client = McpClient::start(&db);

    let resp = client.call_tool(6, "search", json!({"query": "beach"}));
    assert_eq!(resp["result"]["isError"], true, "full response: {resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("no embeddings found"), "{text}");

    // the failure must not kill the server: a follow-up call still works
    let resp2 = client.call_tool(7, "stats", json!({}));
    assert_eq!(resp2["result"]["structuredContent"]["schema_version"], 1);
    client.shutdown();
}

#[test]
fn search_with_zero_or_two_query_modes_is_tool_error() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let mut client = McpClient::start(&db);

    let none = client.call_tool(8, "search", json!({}));
    assert_eq!(none["result"]["isError"], true, "{none}");
    assert!(none["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("exactly one"));

    let two = client.call_tool(9, "search", json!({"query": "x", "person": "Alice"}));
    assert_eq!(two["result"]["isError"], true, "{two}");

    client.shutdown();
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --test mcp search`
Expected: the 3 new tests FAIL (tool not found). The existing 5 still pass.

- [ ] **Step 3: Implement**

In `crates/videre/src/commands/mcp.rs`, add:

```rust
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
        let corpus = search_cmd::load_corpus(&conn, &db.display().to_string())?;

        let mut guard = embedder_cell
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(videre_ml::model::Embedder::load(
                videre_ml::device::best_device(),
            )?);
        }
        let embedder = guard.as_ref().expect("just initialized");

        let (query_vec, query) = if let Some(text) = &params.query {
            (
                embedder.embed_text(text)?,
                QueryJson { kind: "text", value: text.clone() },
            )
        } else {
            let img = std::path::PathBuf::from(params.image_path.as_ref().expect("mode checked"));
            (
                videre_ml::model::embed_image_file(embedder, &img)?,
                QueryJson { kind: "image", value: img.display().to_string() },
            )
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
```

and inside the `#[tool_router] impl VidereServer` block:

```rust
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
```

Notes:
- `crate::commands::search` paths assume mcp.rs sits next to search.rs under `commands/`; `super::search::...` is equivalent, use whichever compiles cleanly.
- If `Embedder` turns out not to be `Send` (the `spawn_blocking` closure will fail to compile), FALL BACK: drop the cache field usage and load the embedder inside the closure per call (correctness over speed), keep the `embedder` field in place, and report DONE_WITH_CONCERNS so the caching can be revisited deliberately.
- `SearchParams` moves into the closure; it must be `Send + 'static` (it is: owned Strings).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --test mcp`
Expected: PASS (8 tests).

Run: `cargo test --workspace`
Expected: PASS, 169 total (166 + 3). No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/mcp.rs crates/videre/tests/mcp.rs
git commit -m "feat: search mcp tool with person/text/image modes and cached embedder"
```

---

### Task 6: Documentation

**Files:**
- Modify: `README.md`, `CLAUDE.md`

- [ ] **Step 1: Update README.md**

Read the file first and match its style. Add `videre mcp` to the subcommands table (one line: "Serve read-only MCP tools for LLM agents over stdio"). Add a new section (near the "JSON output (agentic use)" section, matching heading style):

```markdown
## MCP server (agentic use)

`videre mcp` serves three read-only tools over stdio for MCP clients (Claude Code,
Cursor, and others): `search` (text/person/image), `find_duplicates` (keep/remove
groups; review-only similar clusters with include_similar), and `stats` (library
summary). It binds one database at startup, resolved like every reader:
`--db <path>`, else `default_db` from `~/.videre/config.toml`, else
`~/.videre/hashes.db`; the file must exist. Results reflect the last scan (keep it
fresh with `videre watch`), and tool documents reuse the same shapes and
`"schema_version": 1` as the CLI `--json` output. The first text/image search loads
the embedding model (slow once, then cached for the life of the server).

Client configuration:

```json
{
  "mcpServers": {
    "videre": {
      "command": "/path/to/videre",
      "args": ["mcp"]
    }
  }
}
```

Add `"--db", "/path/to/other.db"` to `args` to serve a non-default library.
```

- [ ] **Step 2: Update CLAUDE.md**

- Update the subcommand count (currently "nine": becomes ten) and add `mcp` to the subcommand enumeration in the "What it does" paragraph.
- Add `commands/mcp.rs` to the project structure listing, and `tests/mcp.rs` to the videre test-file list.
- Add a short `## videre mcp` section: what it serves (the three tools), stdio transport, db resolution identical to readers (must exist at startup), the always-alive tool-error behavior, the cached embedder note, and the client config snippet (condensed from README).
- Note in the key-crates list: `rmcp` (official MCP SDK) and `schemars` (tool parameter schemas).

- [ ] **Step 3: Verify and commit**

```bash
cargo run -q -p videre --bin videre -- mcp --help
cargo run -q -p videre --bin videre -- --help | grep mcp
```

Both must succeed and be consistent with the docs. Then:

```bash
git add README.md CLAUDE.md
git commit -m "docs: document the videre mcp server and its three tools"
```

---

### Task 7: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full suite**

Run: `cargo test --workspace`
Expected: PASS, 169 tests, 0 failed.

- [ ] **Step 2: Release-binary protocol smoke test**

```bash
cargo build --release
H=$(mktemp -d); D=$(mktemp -d); printf same > "$D/a.jpg"; printf same > "$D/b.jpg"
VIDERE_HOME=$H ./target/release/videre dedupe --silent "$D" > /dev/null

# Drive the server with raw JSON-RPC lines; expect an initialize result,
# a tools/list with 3 tools, and a stats result, then clean exit on EOF.
VIDERE_HOME=$H ./target/release/videre mcp <<'EOF'
{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"stats","arguments":{}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_duplicates","arguments":{}}}
EOF
echo "exit=$?"

# startup failure: no db
VIDERE_HOME=$(mktemp -d) ./target/release/videre mcp; echo "exit=$?"
rm -rf "$H" "$D"
```

Expected: the heredoc run prints one JSON line per response (initialize, 3-tool list, stats with `"total_files":2`, find_duplicates with one group), exits 0 on EOF; the no-db run prints the friendly error on stderr, nothing on stdout, exit 1.

- [ ] **Step 3: Record results**

PASS/FAIL per step; any FAIL loops back to the owning task.
