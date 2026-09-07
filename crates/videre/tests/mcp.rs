mod common;
use common::TestLibrary;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

/// Fixture: 4 files, one exact-duplicate pair (hash1: alice1 older KEEP, dup
/// newer), 3 confirmed faces (Alice x2, Bob x1), no model store, no GPS/exif.
/// Every path sits under the library root so the containment guard accepts it.
fn make_db(lib: &TestLibrary) {
    let root = lib.context().paths.root;
    let p = |name: &str| root.join(name).to_string_lossy().into_owned();
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, size_bytes, modified_at, ext) VALUES
           (?1, 'hash1', 10, '2020-01-01T00:00:00+00:00', 'jpg'),
           (?2, 'hash1', 10, '2024-01-01T00:00:00+00:00', 'jpg'),
           (?3, 'hash2', 10, '2021-01-01T00:00:00+00:00', 'jpg'),
           (?4, 'hash3', 10, '2022-01-01T00:00:00+00:00', 'jpg')",
        rusqlite::params![
            p("alice1.jpg"),
            p("alice1_copy.jpg"),
            p("alice2.jpg"),
            p("bob.jpg")
        ],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO people (name, full_name) VALUES ('alice','Alice'),('bob','Bob');
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) VALUES
           ('hash1', '0,0,50,50', X'0000', 'alice', 1),
           ('hash2', '0,0,50,50', X'0000', 'alice', 1),
           ('hash3', '0,0,50,50', X'0000', 'bob', 1);",
    )
    .unwrap();
}

/// Fixture: four dated png files, three classified as documents (two May, one
/// June), plus a May photo, seeded under the root.
fn make_dated_db(lib: &TestLibrary) {
    let root = lib.context().paths.root;
    let p = |name: &str| root.join(name).to_string_lossy().into_owned();
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, size_bytes, modified_at, exif_date, ext) VALUES
           (?1, 'd1', 10, '2025-05-02T00:00:00', '2025-05-02T00:00:00', 'png'),
           (?2, 'd2', 10, '2025-05-20T00:00:00', '2025-05-20T00:00:00', 'png'),
           (?3, 'd3', 10, '2025-06-01T00:00:00', '2025-06-01T00:00:00', 'png'),
           (?4, 'd4', 10, '2025-05-11T00:00:00', '2025-05-11T00:00:00', 'png')",
        rusqlite::params![
            p("may-a.png"),
            p("may-b.png"),
            p("june.png"),
            p("may-photo.png")
        ],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) VALUES
           ('d1', '0,0,50,50', X'0000', 'alice', 1),
           ('d3', '0,0,50,50', X'0000', 'alice', 1);",
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
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
}

impl McpClient {
    /// Spawn `videre mcp` inside this library and complete the handshake.
    fn start(lib: &TestLibrary) -> Self {
        Self::spawn(lib.cmd())
    }

    /// Spawn `videre mcp --library <lib> ` launched from `cwd`, to prove the
    /// server binds to its selected library rather than its launch directory.
    fn start_in(lib: &TestLibrary, cwd: &Path) -> Self {
        Self::spawn(lib.from(cwd))
    }

    fn spawn(mut cmd: std::process::Command) -> Self {
        let mut child = cmd
            .arg("mcp")
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
fn mcp_search_rejects_an_outside_path_and_keeps_its_library() {
    let a = TestLibrary::new();
    let c = TestLibrary::new();
    a.copy_fixture("tiny.jpg", "a.jpg");
    a.scan();
    let mut client = McpClient::start_in(&a, &c.root);
    let bad = client.call_tool(
        1,
        "search",
        json!({"path": [c.root.to_string_lossy()], "missing": ["gps"]}),
    );
    assert_eq!(
        bad["result"]["isError"], true,
        "an outside path must be a tool error: {bad}"
    );
    let good = client.call_tool(2, "stats", json!({}));
    assert_eq!(good["result"]["structuredContent"]["total_files"], 1);
    client.shutdown();
}

#[test]
fn initialize_lists_exactly_three_tools() {
    let lib = TestLibrary::new();
    make_db(&lib);
    let mut client = McpClient::start(&lib);
    let resp = client.request(1, "tools/list", json!({}));
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["find_duplicates", "search", "stats"]);
    client.shutdown();
}

#[test]
fn stats_tool_returns_counts() {
    let lib = TestLibrary::new();
    make_db(&lib);
    let mut client = McpClient::start(&lib);
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
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let text_doc: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(&text_doc, doc);
    client.shutdown();
}

#[test]
fn stats_tool_zero_counts_without_optional_tables() {
    // Only file_hashes populated: stats degrades to zero counts, not an error.
    let lib = TestLibrary::new();
    let path = lib.context().paths.root.join("only.jpg");
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext) VALUES (?1, 'h1', 5, 'jpg')",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();

    let mut client = McpClient::start(&lib);
    let resp = client.call_tool(2, "stats", json!({}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["total_files"], 1, "full response: {resp}");
    assert_eq!(doc["embedded_count"], 0);
    assert_eq!(doc["faces_count"], 0);
    assert_eq!(doc["people"], json!([]));
    client.shutdown();
}

#[test]
fn startup_fails_on_an_uninitialized_library() {
    // A library with no database: mcp is a reader, so it must refuse to start
    // and write nothing to the protocol channel.
    let lib = TestLibrary::new();
    let out = lib.cmd().arg("mcp").output().expect("run videre mcp");
    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "nothing may be written to the protocol channel"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("not initialized"));
}

#[test]
fn find_duplicates_tool_returns_keep_remove_groups() {
    let lib = TestLibrary::new();
    make_db(&lib);
    let mut client = McpClient::start(&lib);

    let resp = client.call_tool(3, "find_duplicates", json!({}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["schema_version"], 1, "full response: {resp}");
    assert_eq!(doc["total_files"], 4);
    let groups = doc["duplicate_groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["hash"], "hash1");
    assert!(
        groups[0]["keep"]["path"]
            .as_str()
            .unwrap()
            .ends_with("alice1.jpg"),
        "oldest is KEEP: {doc}"
    );
    let remove = groups[0]["remove"].as_array().unwrap();
    assert_eq!(remove.len(), 1);
    assert!(remove[0]["path"]
        .as_str()
        .unwrap()
        .ends_with("alice1_copy.jpg"));
    assert!(
        doc.get("similar_groups").is_none(),
        "absent without include_similar"
    );

    let resp2 = client.call_tool(4, "find_duplicates", json!({"include_similar": true}));
    let doc2 = &resp2["result"]["structuredContent"];
    assert!(doc2["similar_groups"]
        .as_array()
        .expect("similar_groups present")
        .is_empty());
    client.shutdown();
}

#[test]
fn search_person_tool_returns_document() {
    let lib = TestLibrary::new();
    make_db(&lib);
    let mut client = McpClient::start(&lib);
    let resp = client.call_tool(5, "search", json!({"person": "Alice"}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["schema_version"], 1, "full response: {resp}");
    assert_eq!(doc["query"]["kind"], "person");
    assert_eq!(doc["query"]["value"], "Alice");
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
    let lib = TestLibrary::new();
    make_db(&lib);
    let mut client = McpClient::start(&lib);

    let resp = client.call_tool(6, "search", json!({"query": "beach"}));
    assert_eq!(resp["result"]["isError"], true, "full response: {resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("no embeddings"), "{text}");
    assert!(
        text.contains(videre_core::embeddings::DEFAULT_MODEL_ID),
        "the error should name the model it looked for: {text}"
    );

    let resp2 = client.call_tool(7, "stats", json!({}));
    assert_eq!(resp2["result"]["structuredContent"]["schema_version"], 1);
    client.shutdown();
}

#[test]
fn search_with_no_input_or_two_rankers_is_tool_error() {
    let lib = TestLibrary::new();
    make_db(&lib);
    let mut client = McpClient::start(&lib);

    let none = client.call_tool(8, "search", json!({}));
    assert_eq!(none["result"]["isError"], true, "{none}");
    assert!(none["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("at least one"));

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
    let lib = TestLibrary::new();
    make_dated_db(&lib);

    let mut client = McpClient::start(&lib);
    let resp = client.call_tool(
        10,
        "search",
        json!({"category": "document", "date": "2025-05", "sort": "date:asc"}),
    );
    assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
    let doc = resp["result"]["structuredContent"].clone();
    client.shutdown();

    let cli = lib
        .cmd()
        .args([
            "search",
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

    assert_eq!(cli_doc["count"], 2, "two May 2025 documents: {cli_doc}");
    assert!(
        cli_doc["results"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("may-a.png"),
        "date:asc puts the earlier document first: {cli_doc}"
    );
    assert_eq!(
        doc, cli_doc,
        "the MCP tool and the CLI must run the same query"
    );
}

#[test]
fn search_treats_person_as_a_composable_filter() {
    let lib = TestLibrary::new();
    make_dated_db(&lib);
    let mut client = McpClient::start(&lib);

    let resp = client.call_tool(11, "search", json!({"person": "Alice", "date": "2025-05"}));
    assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(doc["count"], 1, "{doc}");
    assert!(
        doc["results"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("may-a.png"),
        "{doc}"
    );
    client.shutdown();
}

#[test]
fn search_top_k_truncates_a_filter_only_query() {
    let lib = TestLibrary::new();
    make_dated_db(&lib);
    let mut client = McpClient::start(&lib);

    let resp = client.call_tool(12, "search", json!({"category": "document", "top_k": 1}));
    let doc = &resp["result"]["structuredContent"];
    assert_eq!(
        doc["count"], 1,
        "top_k must apply to a filter-only query: {doc}"
    );
    client.shutdown();
}

/// The CLI and the MCP tool must resolve the same filters identically.
#[test]
fn media_filters_agree_between_the_cli_and_the_mcp_tool() {
    let lib = TestLibrary::new();
    make_dated_db(&lib);

    let cli = lib
        .cmd()
        .args(["search", "--ext", "png", "-k", "100", "--json"])
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

    let mut client = McpClient::start(&lib);
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

#[test]
fn mcp_search_accepts_presence_filters() {
    let lib = TestLibrary::new();
    make_dated_db(&lib);
    lib.conn()
        .execute(
            "UPDATE file_hashes SET gps_lat = 52.5, gps_lon = 13.4 WHERE hash IN ('d1', 'd4')",
            [],
        )
        .unwrap();

    let mut client = McpClient::start(&lib);
    let resp = client.call_tool(21, "search", json!({"missing": ["gps"], "top_k": 100}));
    assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
    let doc = &resp["result"]["structuredContent"];
    let mut names: Vec<String> = doc["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            Path::new(r["path"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    client.shutdown();

    assert_eq!(names, vec!["june.png", "may-b.png"]);
}
