mod common;
use common::videre_bin;
use rusqlite::Connection;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use tempfile::tempdir;

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
    videre_core::db::ensure_file_hashes_columns(&conn);
    db
}

/// Fixture: four dated files, three of them classified as documents (two in
/// May 2025, one in June), plus a May photo. Enough for a composed
/// category + date query to have something to both narrow and order.
fn make_dated_db(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("dated.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         CREATE TABLE classifications (model_id TEXT NOT NULL, hash TEXT NOT NULL,
         category TEXT NOT NULL, confidence REAL NOT NULL, classified_at TEXT NOT NULL,
         PRIMARY KEY (model_id, hash));
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
         bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
         cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
         is_primary INTEGER DEFAULT 0);
         INSERT INTO file_hashes (path, hash, size_bytes, modified_at, exif_date, ext) VALUES
           ('/tmp/may-a.png', 'd1', 10, '2025-05-02T00:00:00', '2025-05-02T00:00:00', 'png'),
           ('/tmp/may-b.png', 'd2', 10, '2025-05-20T00:00:00', '2025-05-20T00:00:00', 'png'),
           ('/tmp/june.png',  'd3', 10, '2025-06-01T00:00:00', '2025-06-01T00:00:00', 'png'),
           ('/tmp/may-photo.png', 'd4', 10, '2025-05-11T00:00:00', '2025-05-11T00:00:00', 'png');
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) VALUES
           ('d1', '0,0,50,50', X'0000', 'Alice', 1),
           ('d3', '0,0,50,50', X'0000', 'Alice', 1);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO classifications VALUES
           (?1, 'd1', 'document', 0.9, '2025-01-01T00:00:00'),
           (?1, 'd2', 'document', 0.9, '2025-01-01T00:00:00'),
           (?1, 'd3', 'document', 0.9, '2025-01-01T00:00:00'),
           (?1, 'd4', 'photo',    0.9, '2025-01-01T00:00:00')",
        [videre_core::embeddings::DEFAULT_MODEL_ID],
    )
    .unwrap();
    videre_core::db::ensure_file_hashes_columns(&conn);
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
        let mut client = McpClient {
            child,
            stdin,
            reader,
        };
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

    fn call_tool(
        &mut self,
        id: u64,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        self.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )
    }

    fn shutdown(mut self) {
        drop(self.stdin); // EOF: normal client shutdown
        let _ = self.child.wait();
    }
}

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
    assert!(
        doc.get("exif_date_range").is_none(),
        "no exif dates in fixture"
    );
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
    videre_core::db::ensure_file_hashes_columns(&conn);
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
    assert!(
        out.stdout.is_empty(),
        "nothing may be written to the protocol channel"
    );
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
    assert_eq!(
        groups[0]["keep"]["path"], "/tmp/alice1.jpg",
        "oldest is KEEP"
    );
    let remove = groups[0]["remove"].as_array().unwrap();
    assert_eq!(remove.len(), 1);
    assert_eq!(remove[0]["path"], "/tmp/alice1_copy.jpg");
    assert!(
        doc.get("similar_groups").is_none(),
        "absent without include_similar"
    );

    // with include_similar: key present (empty here, fixture has no phashes)
    let resp2 = client.call_tool(4, "find_duplicates", json!({"include_similar": true}));
    let doc2 = &resp2["result"]["structuredContent"];
    let similar = doc2["similar_groups"]
        .as_array()
        .expect("similar_groups present");
    assert!(similar.is_empty());

    client.shutdown();
}

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
    // Alice's confirmed faces span hash1 (2 duplicate paths: alice1.jpg,
    // alice1_copy.jpg) and hash2 (alice2.jpg): 3 distinct paths total.
    assert_eq!(doc["count"], 3);
    let results = doc["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
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
    // The message now names the model and where its database was expected,
    // rather than a bare "no embeddings found".
    assert!(text.contains("no embeddings"), "{text}");
    assert!(
        text.contains(videre_core::embeddings::DEFAULT_MODEL_ID),
        "the error should name the model it looked for: {text}"
    );

    // the failure must not kill the server: a follow-up call still works
    let resp2 = client.call_tool(7, "stats", json!({}));
    assert_eq!(resp2["result"]["structuredContent"]["schema_version"], 1);
    client.shutdown();
}

#[test]
fn search_with_no_input_or_two_rankers_is_tool_error() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let mut client = McpClient::start(&db);

    // Nothing at all would mean "the whole library", which is a mistake, not a
    // query.
    let none = client.call_tool(8, "search", json!({}));
    assert_eq!(none["result"]["isError"], true, "{none}");
    assert!(none["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("at least one"));

    // Two rankers cannot both order the results. Filters compose; rankers do not.
    let two = client.call_tool(
        9,
        "search",
        json!({"query": "x", "image_path": "/tmp/example.jpg"}),
    );
    assert_eq!(two["result"]["isError"], true, "{two}");
    assert!(
        two["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("at most one"),
        "{two}"
    );

    client.shutdown();
}

#[test]
fn search_composes_filters_and_matches_the_cli() {
    let dir = tempdir().unwrap();
    let db = make_dated_db(dir.path());

    let mut client = McpClient::start(&db);
    let resp = client.call_tool(
        10,
        "search",
        json!({"category": "document", "date": "2025-05", "sort": "date:asc"}),
    );
    assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
    let doc = resp["result"]["structuredContent"].clone();
    client.shutdown();

    let cli = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--category",
            "document",
            "--date",
            "2025-05",
            "--sort",
            "date:asc",
            "--json",
        ])
        .output()
        .expect("run videre search");
    let cli_doc: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();

    assert_eq!(
        cli_doc["count"], 2,
        "fixture should have exactly two May 2025 documents: {cli_doc}"
    );
    assert_eq!(
        cli_doc["results"][0]["path"], "/tmp/may-a.png",
        "date:asc puts the earlier document first: {cli_doc}"
    );
    assert_eq!(
        doc, cli_doc,
        "the MCP tool and the CLI must run the same query"
    );
}

#[test]
fn search_treats_person_as_a_composable_filter() {
    let dir = tempdir().unwrap();
    let db = make_dated_db(dir.path());
    let mut client = McpClient::start(&db);

    // Alice appears on d1 (May) and d3 (June); the date filter keeps only May.
    let resp = client.call_tool(11, "search", json!({"person": "Alice", "date": "2025-05"}));
    assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["count"], 1, "{doc}");
    assert_eq!(doc["results"][0]["path"], "/tmp/may-a.png", "{doc}");

    client.shutdown();
}

#[test]
fn search_top_k_truncates_a_filter_only_query() {
    let dir = tempdir().unwrap();
    let db = make_dated_db(dir.path());
    let mut client = McpClient::start(&db);

    let resp = client.call_tool(12, "search", json!({"category": "document", "top_k": 1}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(
        doc["count"], 1,
        "top_k must apply to a filter-only query: {doc}"
    );

    client.shutdown();
}

/// The CLI and the MCP tool must resolve the same filters identically. They are
/// two surfaces over one vocabulary, and they have drifted before, which is why
/// CLAUDE.md carries a rule about it.
#[test]
fn media_filters_agree_between_the_cli_and_the_mcp_tool() {
    let dir = tempdir().unwrap();
    let db = make_dated_db(dir.path());

    let cli = std::process::Command::new(common::videre_bin())
        .args(["search", "--ext", "png", "-k", "100", "--json", "--db"])
        .arg(&db)
        .output()
        .expect("failed to run videre search");
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_doc: serde_json::Value = serde_json::from_slice(&cli.stdout).unwrap();
    let mut cli_paths: Vec<String> = cli_doc["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap().to_string())
        .collect();
    cli_paths.sort();
    assert!(!cli_paths.is_empty(), "fixture must have png rows");

    let mut client = McpClient::start(&db);
    let resp = client.call_tool(20, "search", json!({"ext": ["png"], "top_k": 100}));
    assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
    let doc = &resp["result"]["structuredContent"];
    let mut mcp_paths: Vec<String> = doc["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap().to_string())
        .collect();
    mcp_paths.sort();
    client.shutdown();

    assert_eq!(cli_paths, mcp_paths, "one vocabulary, two surfaces");
}
