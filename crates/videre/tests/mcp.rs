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
