# Split `dedupe` into `scan` (ingest) and `dedupe` (query)

**Goal:** `dedupe` currently does two unrelated jobs: scan a directory into the database, and report duplicate groups from what it just scanned. Split these into `videre scan` (writer: filesystem to db) and `videre dedupe` (reader: db to duplicate report), matching the shape every other subcommand already has, and matching the vocabulary `watch --scan` already uses internally.

**Non-goals:** Changing `watch`'s own `--scan` stage (it stays a separate, simpler code path: no phash, no JSONL, always writes to the resolved db). Changing `report`, `search`, `faces`, `prune`, `fix-dates`, `embed`, `config`, or `mcp` beyond the mechanical error-hint string updates in section 5. A migration/compat shim for the old `dedupe <dir>` invocation (rejected during brainstorming: clean break, matches the project's three prior deliberate breaking changes).

---

## Why

`videre dedupe <dir>` is the one command in the project that doesn't fit the "subcommand = one job" pattern every other command follows (`report`/`search`/`faces`/`prune`/`fix-dates` all read the db; `watch` already separates its four stages by name, one of which is literally called `scan`). The MCP server's `find_duplicates` tool already proved the query half works standalone, reading `file_hashes` from the db with no scanning involved. This split makes `dedupe`'s CLI shape match what already exists elsewhere in the codebase, rather than introducing a new pattern.

## 1. `videre scan` (new, writer)

`crates/videre/src/commands/scan.rs`, replacing `dedupe.rs`'s current file. Absorbs, unchanged in behavior, everything today's `dedupe` does *except* printing duplicate groups:

```rust
#[derive(clap::Args)]
pub struct ScanArgs {
    /// Directory to scan recursively (default: 'path' from videre config)
    directory: Option<PathBuf>,

    /// JSONL output file (appended). Bare --output targets ~/.videre/hashes.jsonl.
    /// Note: place a bare --output AFTER the directory. Cannot be used with --output-sqlite
    #[arg(long, num_args = 0..=1, conflicts_with = "output_sqlite")]
    output: Option<Option<PathBuf>>,

    /// SQLite output file (upserted by path). When neither --output nor
    /// --output-sqlite is given, records go to the resolved default db
    #[arg(long)]
    output_sqlite: Option<PathBuf>,

    /// Also compute and store perceptual hashes for near-duplicate detection
    #[arg(long)]
    similar: bool,

    /// Suppress progress output on stderr
    #[arg(long)]
    silent: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}
```

Relocated verbatim from today's `dedupe.rs`, no behavior change:
- Directory resolution via `super::resolve_directory` (config `path` fallback).
- First-use adoption via `super::maybe_adopt_default_path` (fires exactly as it does today, just from `scan` instead of `dedupe`).
- `gather_records` (scan + parallel hash + optional phash), `output_target`/`OutputTarget` (SQLite vs JSONL resolution, parent-dir creation), all existing stderr progress lines (`"Scanning {:?}..."`, `"Found N file(s) to process"`, `"Warning: skipping..."`, `"Wrote N record(s) to {:?}"`), all existing error text and `process::exit(1)` sites for missing directory / write failure.

**Behavior change, deliberate:** `scan`'s text mode no longer prints REMOVE candidate paths to stdout, and no longer prints the "N duplicate group(s)" / "No exact duplicates found" / similar-groups summary lines to stderr — that reporting is `dedupe`'s job now, not scan's. Stdout is empty in text mode (all output is progress, on stderr, exactly like `watch`). `scan --json` returns a new, minimal document describing the scan itself, not duplicate data:

```json
{ "schema_version": 1, "total_files": 1200, "output": { "kind": "sqlite", "path": "/Users/you/.videre/hashes.db" } }
```

`output.kind` is `"sqlite"` or `"jsonl"`; `output.path` is the resolved destination (same value that would appear in the "Wrote N record(s) to {path}" stderr line). New type `ScanJson { schema_version: u32, total_files: usize, output: ScanOutputJson }`, `ScanOutputJson { kind: &'static str, path: String }`, defined in `videre::types` alongside the other `--json` types. Error behavior matches every other `--json` command: on failure, stdout carries `{"schema_version":1,"error":{"message":"..."}}` and exit is nonzero.

## 2. `dedupe` becomes a pure reader

`crates/videre/src/commands/dedupe.rs` shrinks to:

```rust
#[derive(clap::Args)]
pub struct DedupeArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Also report perceptual-hash near-duplicate clusters (review-only)
    #[arg(long)]
    similar: bool,

    /// Suppress progress output on stderr (duplicate paths are always written to stdout)
    #[arg(long)]
    silent: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}
```

No directory, no `--output`/`--output-sqlite`. `--similar` changes meaning: it is no longer "also compute phash while scanning," it is "also include near-duplicate clusters in this report if the db has phash data" (identical semantics to the MCP tool's `include_similar` parameter). Db resolution goes through the existing `super::resolve_reader_db` (explicit `--db` > `default_db` in config > `~/.videre/hashes.db`; the resolved db must exist, same friendly error as every other reader).

Behavior (text mode): loads `file_hashes` via `sqlite_output::load_records`, groups via `find_duplicate_groups`/`find_similar_groups` (only when `--similar`), prints REMOVE candidate paths to stdout one per line (pipe contract preserved: `videre dedupe | xargs trash`), stderr summary lines adapted to no longer imply scanning just happened (`"N duplicate group(s), N file(s) to remove."` stays; drop any wording that reads as "just scanned"). Behavior (`--json` mode): see section 3, which covers both `dedupe --json` and the MCP tool together.

## 3. Shared `FindDuplicatesJson`: one type, two callers

Today `mcp.rs` has a private `FindDuplicatesJson` / `build_find_duplicates` used only by the MCP `find_duplicates` tool. This slice promotes both to shared code so `dedupe --json` and the MCP tool return byte-identical documents:

- `FindDuplicatesJson { schema_version: u32, total_files: usize, duplicate_groups: Vec<DupGroupJson>, similar_groups: Option<Vec<SimilarGroupJson>> }` moves into `videre::types` (next to the existing `DupGroupJson`/`SimilarGroupJson`, which are unchanged). The old `DedupeJson` type (with its `scanned` field) is deleted; nothing else referenced it.
- `pub(crate) fn build_find_duplicates(db: &Path, include_similar: bool) -> anyhow::Result<FindDuplicatesJson>` moves from `mcp.rs` into `commands/mod.rs`, alongside `resolve_reader_db`/`resolve_directory`/`maybe_adopt_default_path`. Body unchanged: `load_records` then `find_duplicate_groups`/`find_similar_groups` mapped through `DupGroupJson::from`/`SimilarGroupJson::from`.
- `dedupe.rs`'s `run_json` calls `super::build_find_duplicates(&db, args.similar)` directly. `mcp.rs`'s `find_duplicates` tool method calls the same function (its own local copy is deleted, only the `#[tool]`-wrapping method and its `FindDuplicatesParams` struct stay in `mcp.rs`).
- `dedupe`'s text mode does NOT go through this JSON path (it prints plain paths, not JSON) - it calls `find_duplicate_groups`/`find_similar_groups` directly on records from `load_records`, same as today's text path already does on records from a scan. Net effect: text and json modes both start from `load_records` now, instead of text using freshly-scanned records and json using the same freshly-scanned records - they diverge only in what they do with the groups, exactly as before, just sourced from the db instead of a scan.

## 4. Error-hint and doc-string updates (mechanical, six spots)

Every place instructing the user to populate the database updates from `dedupe` to `scan`:

| File | Old | New |
|------|-----|-----|
| `commands/mod.rs` (`resolve_reader_db`) | `"no database found at {}; run 'videre dedupe <dir>' first"` | `"no database found at {}; run 'videre scan <dir>' first"` |
| `mcp.rs` (startup check) | same string | same replacement |
| `mcp.rs` (`get_info` instructions) | `"...run 'videre dedupe'/'videre watch' to freshen."` | `"...run 'videre scan'/'videre watch' to freshen."` |
| `watch.rs` (missing-table hint) | `"run 'videre dedupe --output-sqlite <db> <dir>' or 'videre watch --scan ...' first"` | `"run 'videre scan --output-sqlite <db> <dir>' or 'videre watch --scan ...' first"` |
| `README.md` | `no database found at <path>; run 'videre dedupe <dir>' first` | `... run 'videre scan <dir>' first` |
| `CLAUDE.md` (×2 occurrences) | same string, twice | same replacement, twice |

`watch --scan`'s own internal stage (`run_scan_stage` in `watch.rs`) is untouched code; only the doc/error strings that *name* the top-level ingestion command change, not the stage flag itself (`--scan` stays `--scan`).

## 5. Documentation and breaking-changes note

README.md and CLAUDE.md: `scan` added to the subcommands table/enumeration (eleven subcommands total). Every `videre dedupe <dir>` example across both files becomes two steps: `videre scan <dir>` then `videre dedupe`. The quickstart's one-liner becomes:

```bash
videre scan ~/Photos && videre dedupe | xargs trash
```

(or, once a default path is adopted: `videre scan && videre dedupe | xargs trash`, or with `watch` running in the background, just `videre dedupe | xargs trash`). A fourth deliberate breaking change is documented alongside the existing three (positional-to-`--db`, bare-dedupe-writes-SQLite-by-default, `watch --output-sqlite` optional): **dedupe no longer scans a directory; run `videre scan` first.**

## Testing

- `crates/videre/tests/scan.rs` (new, replaces the directory-scanning parts of `tests/integration.rs`): the existing dedupe-as-scanner tests (exact-duplicate JSONL write, EXIF population, SQLite write, upsert-on-repeat, `--output`/`--output-sqlite` conflict, bare-scan-writes-default-db, bare-`--output`-writes-default-jsonl, first-explicit-directory-adopts-default-path ×3, `--json` missing-directory error) move here with `.arg("scan")` instead of `.arg("dedupe")`, and stdout assertions updated to expect *no* REMOVE-path lines (scan's stdout is now empty in text mode) and the new `ScanJson` shape under `--json`.
- `crates/videre/tests/integration.rs`: keeps only the tests that are genuinely about duplicate detection, retargeted to a two-step `scan` then `dedupe --db <db>` flow, asserting REMOVE paths on `dedupe`'s stdout.
- New tests: `dedupe --similar` reads existing phash data from the db and reports `similar_groups` without touching the filesystem or the model; `dedupe` rejects a directory positional (clap error) to lock in the no-fallback decision. One new test in `tests/integration.rs`, `dedupe_json_matches_mcp_find_duplicates_shape`, builds one fixture db (reusing `tests/mcp.rs`'s `make_db` fixture layout inline, since cross-test-binary imports aren't possible in Rust integration tests), runs `dedupe --db <db> --json` and a raw `videre mcp --db <db>` JSON-RPC `find_duplicates` call against it, and asserts the two JSON values are equal after stripping the MCP envelope down to `structuredContent`.
- `crates/videre/tests/mcp.rs`: unaffected in behavior (the tool's output doesn't change), but its internal `build_find_duplicates` now resolves via `commands::build_find_duplicates` instead of a private copy - existing `find_duplicates_tool_returns_keep_remove_groups` test is the regression guard that this refactor didn't change MCP's output.
