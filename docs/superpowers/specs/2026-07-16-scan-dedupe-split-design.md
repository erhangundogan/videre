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

**Behavior change, deliberate:** `scan`'s text mode no longer prints REMOVE candidate paths to stdout, and no longer prints the "N duplicate group(s)" / "No exact duplicates found" / similar-groups summary lines to stderr: that reporting is `dedupe`'s job now, not scan's. Stdout is empty in text mode (all output is progress, on stderr, exactly like `watch`). `scan --json` returns a new, minimal document describing the scan itself, not duplicate data:

```json
{ "schema_version": 1, "total_files": 1200, "output": { "kind": "sqlite", "path": "/Users/you/.videre/hashes.db" } }
```

`output.kind` is `"sqlite"` or `"jsonl"`; `output.path` is the resolved destination (same value that would appear in the "Wrote N record(s) to {path}" stderr line). `total_files` is `records.len()` (files successfully hashed and written), the same count that stderr's "Wrote N record(s)" line already reports, NOT `paths.len()` (files found before hashing) - the two differ whenever a file fails to hash and is skipped with a warning. New type `ScanJson { schema_version: u32, total_files: usize, output: ScanOutputJson }`, `ScanOutputJson { kind: &'static str, path: String }`, defined in `videre::types` alongside the other `--json` types. Error behavior matches every other `--json` command: on failure, stdout carries `{"schema_version":1,"error":{"message":"..."}}` and exit is nonzero.

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

No directory, no `--output`/`--output-sqlite`. `--similar` changes meaning: it is no longer "also compute phash while scanning," it is "also include near-duplicate clusters in this report if the db has phash data" (identical semantics to the MCP tool's `include_similar` parameter). If `--similar` is passed against a db that was never scanned with `scan --similar` (no phash data), `similar_groups` is present and empty, not omitted - `dedupe`'s existing "No visually similar images found." stderr line is dropped for the reader (it wrongly implied a comparison just ran); no replacement message is needed, the empty `similar_groups` array (or, in text mode, simply nothing printed) says enough.

**Db resolution, explicit path included:** `dedupe --db <path>` uses the same rule as `mcp`, not the looser rules `report`/`prune`/`search` currently have (see below) - the resolved db must exist, whether defaulted or explicit, else the friendly `"no database found at {}; run 'videre scan <dir>' first"` error (as an `ErrorJson` object under `--json`, per the existing `--json` error contract). This requires `resolve_reader_db` (`commands/mod.rs`) to gain an `always_check_exists: bool` parameter (or an equivalent second entry point) so `dedupe` and `mcp` can opt into checking explicit paths too, while `report`/`prune`/`search`/`embed`/`faces`/`fix-dates` keep today's per-command behavior unchanged (their explicit-path handling is out of scope for this slice: `report`/`prune`'s own `if !db.exists()` checks with their existing `"Error: {:?} does not exist"` text, and `search`'s current no-check-on-explicit-path behavior, all stay exactly as they are today). Only `dedupe` changes here, matching `mcp` because both are now db-only commands with no separate scan step to fall back on.

Behavior (text mode): loads `file_hashes` via `sqlite_output::load_records`, groups via `find_duplicate_groups`/`find_similar_groups` (only when `--similar`), prints REMOVE candidate paths to stdout one per line (pipe contract preserved: `videre dedupe | xargs trash`), stderr summary lines adapted to no longer imply scanning just happened (`"N duplicate group(s), N file(s) to remove."` stays: none of dedupe's existing stderr lines currently mention scanning, so there is nothing else to reword here). Behavior (`--json` mode): see section 3, which covers both `dedupe --json` and the MCP tool together.

## 3. Shared `FindDuplicatesJson`: one type, two callers

Today `mcp.rs` has a private `FindDuplicatesJson` / `build_find_duplicates` used only by the MCP `find_duplicates` tool. This slice promotes both to shared code so `dedupe --json` and the MCP tool return byte-identical documents:

- `FindDuplicatesJson { schema_version: u32, total_files: usize, duplicate_groups: Vec<DupGroupJson>, #[serde(skip_serializing_if = "Option::is_none")] similar_groups: Option<Vec<SimilarGroupJson>> }` moves into `videre::types` (next to the existing `DupGroupJson`/`SimilarGroupJson`, which are unchanged). The `skip_serializing_if` attribute is load-bearing: both the MCP test and the CLI `--json` tests assert the `similar_groups` key is absent, not `null`, when similar-groups weren't requested, and it must carry over unchanged.
- The old `DedupeJson` type (with its `scanned` field) is deleted. It is NOT unreferenced today: `crates/videre/src/types.rs`'s own test module has `dedupe_json_omits_similar_groups_when_none` and `dedupe_json_includes_similar_groups_when_some`, both constructing `DedupeJson` directly. These two tests are retargeted to construct `FindDuplicatesJson` instead (same assertions: `similar_groups` key absent when `None`, present with a `files` array with no `keep`/`remove` when `Some`), not deleted.
- `pub(crate) fn build_find_duplicates(db: &Path, include_similar: bool) -> anyhow::Result<FindDuplicatesJson>` moves from `mcp.rs` into `commands/mod.rs`, alongside `resolve_reader_db`/`resolve_directory`/`maybe_adopt_default_path`. Body unchanged: `load_records` then `find_duplicate_groups`/`find_similar_groups` mapped through `DupGroupJson::from`/`SimilarGroupJson::from`.
- `dedupe.rs`'s `run_json` calls `super::build_find_duplicates(&db, args.similar)` directly. `mcp.rs`'s `find_duplicates` tool method calls the same function (its own local copy is deleted, only the `#[tool]`-wrapping method and its `FindDuplicatesParams` struct stay in `mcp.rs`).
- `dedupe`'s text mode does NOT go through this JSON path (it prints plain paths, not JSON) - it calls `find_duplicate_groups`/`find_similar_groups` directly on records from `load_records`, same as today's text path already does on records from a scan. Net effect: text and json modes both start from `load_records` now, instead of text using freshly-scanned records and json using the same freshly-scanned records - they diverge only in what they do with the groups, exactly as before, just sourced from the db instead of a scan.

## 4. Error-hint and doc-string updates (mechanical, eight spots)

Every place instructing the user to populate the database updates from `dedupe` to `scan`:

| File | Old | New |
|------|-----|-----|
| `commands/mod.rs` (`resolve_reader_db`) | `"no database found at {}; run 'videre dedupe <dir>' first"` | `"no database found at {}; run 'videre scan <dir>' first"` |
| `commands/mod.rs` (`maybe_adopt_default_path` doc comment) | "future bare `videre dedupe` / `videre watch` calls" | "future bare `videre scan` / `videre watch` calls" |
| `mcp.rs` (startup check) | `"no database found at {}; run 'videre dedupe <dir>' first"` | same replacement |
| `mcp.rs` (`get_info` instructions) | `"...run 'videre dedupe'/'videre watch' to freshen."` | `"...run 'videre scan'/'videre watch' to freshen."` |
| `watch.rs` (missing-table hint) | `"run 'videre dedupe --output-sqlite <db> <dir>' or 'videre watch --scan ...' first"` | `"run 'videre scan --output-sqlite <db> <dir>' or 'videre watch --scan ...' first"` |
| `tests/prune.rs` (`missing_default_db_prints_friendly_error`) | asserts `stderr.contains("videre dedupe")` | asserts `stderr.contains("videre scan")` |
| `README.md` | `no database found at <path>; run 'videre dedupe <dir>' first` | `... run 'videre scan <dir>' first` |
| `CLAUDE.md` (×2 occurrences) | same string, twice | same replacement, twice |

`watch --scan`'s own internal stage (`run_scan_stage` in `watch.rs`) is untouched code; only the doc/error strings that *name* the top-level ingestion command change, not the stage flag itself (`--scan` stays `--scan`). A comment in `tests/watch.rs` also names `videre dedupe`; update it too for accuracy, though it's a comment with no assertion behind it.

**`main.rs` help text** (not previously called out but required by the split): the `Command` enum's doc comments become the two subcommands' one-line `--help` summaries. `Dedupe`'s current comment, "Scan a directory, hash every image, and print duplicate paths to stdout," is wrong for a reader and must change; `Scan` needs a new one. Suggested text: `Scan` = "Scan a directory, hash every image, and populate the database"; `Dedupe` = "Report duplicate files from the database and print paths to remove."

## 5. Documentation and breaking-changes note

README.md and CLAUDE.md: `scan` added to the subcommands table/enumeration (eleven subcommands total). Every `videre dedupe <dir>` example across both files becomes two steps: `videre scan <dir>` then `videre dedupe`. The quickstart's one-liner becomes:

```bash
videre scan ~/Photos && videre dedupe | xargs trash
```

(or, once a default path is adopted: `videre scan && videre dedupe | xargs trash`, or with `watch` running in the background, just `videre dedupe | xargs trash`). A fourth deliberate breaking change is documented alongside the existing three (positional-to-`--db`, bare-dedupe-writes-SQLite-by-default, `watch --output-sqlite` optional): **dedupe no longer scans a directory; run `videre scan` first.**

## Testing

- `crates/videre/tests/scan.rs` (new, replaces the directory-scanning parts of `tests/integration.rs`): every existing `tests/integration.rs` test that spawns `dedupe` with a directory argument moves here with `.arg("scan")` instead of `.arg("dedupe")`. This is the full list as it exists today, not an abbreviated one: `exact_duplicates_appear_in_output_file`, `missing_directory_exits_nonzero`, `exif_fields_populated_for_jpeg_with_exif`, `sqlite_output_writes_records_to_db`, `sqlite_output_upserts_on_repeated_run`, `sqlite_and_output_flags_conflict`, `bare_dedupe_writes_default_sqlite_db`, `bare_output_flag_writes_default_jsonl`, `first_explicit_dedupe_adopts_directory_as_default_path`, `second_explicit_dedupe_does_not_overwrite_adopted_default_path`, `silent_flag_suppresses_the_adoption_note`, `json_mode_adopts_default_path_without_polluting_stdout` (four adoption tests total, not three), `json_error_object_for_missing_directory`, `bare_dedupe_without_directory_or_config_path_errors`, `config_path_supplies_dedupe_directory`. Renamed to their `scan`-flavored names (e.g. `bare_scan_writes_default_sqlite_db`) where the old name said "dedupe." Stdout assertions on the tests that previously checked a REMOVE-path line (`exact_duplicates_appear_in_output_file`, `bare_dedupe_writes_default_sqlite_db`) drop that assertion (scan's stdout is now always empty in text mode; the duplicate-detection assertion itself moves to the corresponding new test in `tests/integration.rs`, described below). Tests that used `--json` and asserted the old `DedupeJson` shape (`json_output_reports_duplicate_groups`, `json_with_similar_flag_includes_similar_groups_key`) are replaced by new assertions against `ScanJson`'s shape (`schema_version`, `total_files`, `output.kind`, `output.path`) - they are no longer duplicate-group tests, since scan's `--json` output doesn't carry duplicate data anymore.
- `crates/videre/tests/integration.rs`: becomes dedupe-only. `exact_duplicates_appear_in_output_file` and `bare_dedupe_writes_default_sqlite_db`'s duplicate-detection assertions move here as new tests that run `scan <dir> --output-sqlite <db>` then `dedupe --db <db>`, asserting REMOVE paths on `dedupe`'s stdout. `json_output_reports_duplicate_groups` and `json_with_similar_flag_includes_similar_groups_key` move here too, retargeted to the same two-step flow, with their JSON assertions changed from the old `scanned` field to `total_files` (`FindDuplicatesJson`'s field name, not `DedupeJson`'s).
- New tests: `dedupe --similar` reads existing phash data from the db and reports `similar_groups` without touching the filesystem or the model; `dedupe --similar` against a db with no phash data (never `scan --similar`'d) returns `similar_groups: []`, not an omitted key and not an error; `dedupe` rejects a directory positional (clap error) to lock in the no-fallback decision; `dedupe --db <nonexistent-explicit-path>` fails with the friendly "no database found" error (locking in the explicit-path-must-exist decision from section 2, distinguishing `dedupe` from `report`/`prune`/`search`'s current looser behavior). One new test in `tests/integration.rs`, `dedupe_json_matches_mcp_find_duplicates_shape`, builds one fixture db (reusing `tests/mcp.rs`'s `make_db` fixture layout inline, since cross-test-binary imports aren't possible in Rust integration tests), runs `dedupe --db <db> --json` and a raw `videre mcp --db <db>` JSON-RPC `find_duplicates` call against it, and asserts the two JSON values are equal after stripping the MCP envelope down to `structuredContent`.
- `crates/videre/tests/mcp.rs`: unaffected in behavior (the tool's output doesn't change), but `src/commands/mcp.rs`'s `build_find_duplicates` now resolves via `commands::build_find_duplicates` instead of a private copy - existing `find_duplicates_tool_returns_keep_remove_groups` test is the regression guard that this refactor didn't change MCP's output.
- `crates/videre/src/types.rs`: `dedupe_json_omits_similar_groups_when_none` and `dedupe_json_includes_similar_groups_when_some` retarget from `DedupeJson` to `FindDuplicatesJson` (see section 3), same assertions, new type name and constructor fields (`total_files` instead of `scanned`).
