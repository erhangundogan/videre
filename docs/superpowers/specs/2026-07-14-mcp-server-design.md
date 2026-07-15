# `videre mcp`: MCP Server for Agentic Use (Road 2, First Slice)

**Goal:** Expose videre's query operations as MCP tools so LLM agents (Claude Code, Cursor,
and any other MCP client) can drive the library directly: a local stdio server bound to one
SQLite database, serving three read-only tools (`search`, `find_duplicates`, `stats`).

**Non-goals (this slice):** Mutating tools (prune, fix-dates, or anything that deletes or
modifies files/rows), long-running job tools (embed, faces, watch), report/UI surfaces, MCP
resources or prompts (tools only), HTTP transport, multi-database serving, authentication.
Later slices can add dry-run preview tools and more; nothing here forecloses that.

---

## Design principles

1. **Read-only, DB-backed, fast.** No tool call scans the filesystem, hashes files, or writes
   to the database. Every tool answers from the existing SQLite tables in milliseconds
   (except the first text/image search, which pays a one-time model load). Freshness is the
   job of `videre watch` or manual CLI scans, and the tool descriptions say so.
2. **One result contract across surfaces.** Tool results reuse the CLI `--json` document
   shapes (same field names, same `schema_version: 1` convention, same keep/remove semantics)
   so an agent that learned one surface can read the other.
3. **stdout belongs to the protocol.** In stdio MCP, stdout carries JSON-RPC. All server
   logging (startup line, per-call errors) goes to stderr, which MCP clients surface as
   server logs.
4. **The agent acts; videre informs.** `find_duplicates` returns keep/remove candidates but
   deletes nothing. The agent uses its own file tools (and its own judgment/confirmation
   flow) to act on paths. Tool descriptions instruct the agent to verify paths still exist
   before acting, since results reflect the last scan.

## CLI shape and lifecycle

New subcommand (amended 2026-07-14 by `2026-07-14-home-dir-defaults-design.md`, which
implements first):

```
videre mcp [--db <path>]
```

- `--db` (optional): SQLite database path. When omitted, resolved exactly like every other
  reader command: `default_db` from `$VIDERE_HOME/config.toml`, else `$VIDERE_HOME/hashes.db`
  (`VIDERE_HOME` defaults to `~/.videre`).
- Serves MCP over stdio until the client closes stdin (normal client shutdown), then exits 0.
- Startup validation: the resolved db file must already exist (mcp is a reader). SQLite
  creates missing files on open, so a typo'd or empty-library path would otherwise silently
  serve an empty library. If the file does not exist: one stderr line, exit 1, nothing on
  stdout.
- On successful startup, one stderr line (e.g. `videre mcp: serving <db>`), then the protocol
  loop. The pre-dispatch `migrate_legacy_dupe_cache()` call in `main.rs` runs as usual.

Client configuration (documented in README):

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

(Add `"--db", "/path/to/other.db"` to `args` to serve a non-default library.)

Server identity: name `videre`, version from `CARGO_PKG_VERSION`.

## Tools

Three tools. Parameter schemas are derived with `schemars`; every tool returns a single JSON
document as structured content (and the same JSON serialized in the text content for clients
that ignore structured content). All documents start with `"schema_version": 1`, the same
constant the CLI `--json` mode uses (`videre::types::SCHEMA_VERSION`).

### `search`

Semantic/person search over the indexed library. Params (exactly one of the three query
modes; enforced in the handler, invalid combinations are a tool error):

| Param | Type | Notes |
|-------|------|-------|
| `query` | string, optional | Text query, e.g. "sunset on beach" (needs prior `videre embed`) |
| `person` | string, optional | Person name; confirmed faces only (needs `videre faces` + labeling) |
| `image_path` | string, optional | Path to a local example image (needs prior `videre embed`) |
| `top_k` | integer, optional, default 20 | Max hashes returned (paths may exceed this when duplicates share a hash) |

Result: identical shape to `videre search --json`:

```json
{
  "schema_version": 1,
  "query": { "kind": "text", "value": "sunset on beach" },
  "count": 2,
  "results": [
    { "path": "/photos/a.jpg", "hash": "abc123", "score": 0.8312 },
    { "path": "/photos/dup_a.jpg", "hash": "abc123", "score": 0.8312 }
  ]
}
```

Person hits carry `path` only (no `hash`/`score` keys), matching the CLI. Empty results are a
success (`count: 0`), not an error.

Model lifecycle: the SigLIP embedder loads lazily on the first `query`/`image_path` call and
stays warm in server memory for subsequent calls (the big latency win over the CLI, which
reloads per invocation). `person` searches never touch the model. Model weights download from
Hugging Face on first ever use, exactly as the CLI does; the tool description warns the first
text/image search may be slow.

### `find_duplicates`

Exact-duplicate groups from the database. Params:

| Param | Type | Notes |
|-------|------|-------|
| `include_similar` | boolean, optional, default false | Also return perceptual-hash near-duplicate clusters |

Result: the CLI `dedupe --json` shape with one field renamed: `scanned` (files hashed this
run) becomes `total_files` (rows read from `file_hashes`), since no scan happens here.

```json
{
  "schema_version": 1,
  "total_files": 1200,
  "duplicate_groups": [
    { "hash": "abc123", "keep": { "path": "...", "...": "..." }, "remove": [ { "path": "...", "...": "..." } ] }
  ],
  "similar_groups": [ { "hash": "phash:00ff...", "files": [ "..." ] } ]
}
```

- `duplicate_groups`: same keep/remove split and KEEP rule as the CLI (oldest by `exif_date`,
  else `min(created_at, modified_at)`), computed by the existing `find_duplicate_groups` over
  `FileRecord`s loaded from `file_hashes`.
- `similar_groups`: present only when `include_similar` is true; same flat review-only
  `{hash, files}` clusters as the CLI (never keep/remove; near-duplicates are not safe to
  auto-delete). Empty if the library was never scanned with `--similar` (no phash values).
- No filesystem existence check on returned paths: results reflect the db. The tool
  description tells the agent to verify paths before acting and to run `videre prune` /
  `videre watch` to freshen a stale db.

### `stats`

Library orientation summary; no params. One cheap SQL pass per field:

```json
{
  "schema_version": 1,
  "total_files": 1200,
  "total_size_bytes": 8123456789,
  "unique_hashes": 1100,
  "embedded_count": 1080,
  "faces_count": 3400,
  "people": ["Alice", "Bob"],
  "files_with_gps": 900,
  "exif_date_range": { "min": "2014-03-01T09:00:00", "max": "2026-06-30T18:12:44" }
}
```

- `embedded_count`: rows in `embeddings`. `faces_count`: rows in `faces`. `people`: distinct
  confirmed `person_label` values, sorted. `exif_date_range`: min/max non-null `exif_date`
  (the `0000-` unset-clock values are already stored as null by the scanner); the field is
  omitted when no file has an `exif_date`.
- Missing tables (e.g. a db scanned before `videre embed`/`videre faces` ever ran) are not
  errors: the corresponding counts are 0 and `people` is empty.

## Architecture

- **New file:** `crates/videre/src/commands/mcp.rs` (plus `McpArgs { db: PathBuf }` and the
  enum/dispatch additions in `main.rs` following the established pattern). Split into a
  `commands/mcp/` directory only if it outgrows one file.
- **Dependencies added to the videre crate:** `rmcp` (official Rust MCP SDK, server + stdio
  transport features) and `schemars` (parameter schema derivation). `tokio` is already a
  dependency via the report server. Exact versions pinned in the implementation plan.
- **Handlers are thin.** They call the same underlying functions the CLI uses:
  `videre_core::person_search::search_by_person`, `videre_core::embeddings::{load_embeddings,
  paths_for_hash}`, `videre_ml::search::top_k`, `videre_ml::model`/`device` for the embedder,
  and `videre::output::{find_duplicate_groups, find_similar_groups}` over `FileRecord`s
  loaded from `file_hashes`.
- **Struct reuse, not duplication.** Search's result structs (`SearchJson`, `QueryJson`,
  `SearchHitJson`, currently private in `commands/search.rs`) are promoted to `pub(crate)`
  so `mcp.rs` reuses them; search's query pipeline is refactored just enough that the
  embedding-based path can accept a pre-loaded embedder (the CLI keeps loading per
  invocation; the server passes its cached one). `DupGroupJson`/`SimilarGroupJson`/
  `FileRecord` come from `videre::types` as today. `find_duplicates` and `stats` get small
  new `Serialize` result structs (the former differs from `DedupeJson` only by
  `total_files`).
- **A `FileRecord` loader from SQLite** (SELECT over `file_hashes` mapping rows to
  `FileRecord`) is added to `videre::sqlite_output`, next to the existing writer (it cannot
  live in `videre-core`, which does not depend on the `videre` lib that owns `FileRecord`);
  `find_duplicates` uses it.
- **Server state:** the db path plus `Mutex<Option<Embedder>>` (lazy init). SQLite
  connections open per tool call via `videre_core::db::open_wal` - cheap under WAL, keeps
  handlers stateless, and coexists with a concurrently running `videre watch` writer exactly
  like the report server does. Model inference and other heavy compute run inside
  `tokio::task::spawn_blocking` so the protocol loop stays responsive.

## Error handling

- Per-call failures (db unreadable, missing `embeddings` table, empty corpus, bad
  `image_path`, zero or multiple query modes supplied) become MCP tool errors whose message
  is the rendered anyhow chain (`{e:#}`), the same text the CLI would print. No panics cross
  the protocol boundary.
- Startup failures (db file missing) print to stderr and exit 1 before serving; nothing is
  written to stdout.
- Missing optional tables degrade gracefully in `stats` (zero counts) but are real errors in
  `search` (text/image search without embeddings is an error, matching the CLI's message
  "no embeddings found ...; run videre embed first").

## Testing

- **Unit tests** (in `commands/mcp.rs` or alongside the loaders): `stats` and
  `find_duplicates` result-building against a fixture db (same `make_db` style as
  `tests/person_search.rs`), including: keep/remove split correctness, `similar_groups`
  presence gated on the param, zero-count stats on a db without embeddings/faces tables,
  `total_files` counting.
- **Integration tests** (`crates/videre/tests/mcp.rs`): spawn `videre mcp <db>` as a child
  process and speak raw line-delimited JSON-RPC over stdin/stdout with no client library:
  1. `initialize` handshake succeeds; server reports name `videre`.
  2. `tools/list` returns exactly `search`, `find_duplicates`, `stats`.
  3. `tools/call find_duplicates` on a fixture db returns the expected group.
  4. `tools/call stats` returns expected counts.
  5. `tools/call search` with `person` returns the person document (no model needed).
  6. `tools/call search` with `query` ("text search") on a db without embeddings returns a
     tool error (not a crash, not a dead server: a subsequent `stats` call still works).
  7. Startup with a nonexistent db path exits nonzero with empty stdout.
- **Model-path policy** (same as the `--json` slice): the text/image search success path
  needs SigLIP weights and is not integration-tested; its plumbing is covered by unit-level
  struct tests and by reusing the already-tested CLI pipeline.
- **Docs:** README and CLAUDE.md gain an "MCP server" section: what it serves, the three
  tools, the client config snippet, the freshness caveat, and the first-search model-load
  warning.
