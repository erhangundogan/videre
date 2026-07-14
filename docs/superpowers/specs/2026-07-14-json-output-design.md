# `--json` Output for `videre search` and `videre dedupe` (Agentic Slice)

**Goal:** Give the two result-bearing subcommands a machine-consumable `--json` mode so an
LLM agent (or any script) can drive `videre` without scraping human-formatted text. This is
the first "agentic usage" slice. It is deliberately small: only `search` and `dedupe`, only a
`--json` flag, no MCP server (that is a separate future project).

**Non-goals:** JSON for other subcommands (`prune`, `fix-dates`, `faces`, `embed`, `watch`),
an MCP server, streaming/NDJSON, output formats other than JSON, changing any existing default
(text) output or the SQLite schema.

---

## Design principles (pure-agentic)

The consumer is a program, not a person. Two principles follow:

1. **Never make an agent's call fail over a harmless redundancy.** Flags that become
   meaningless in `--json` mode are silently ignored, not rejected.
2. **stdout in `--json` mode is *always* exactly one valid JSON object** - on success and on
   failure alike - so an agent can unconditionally `json.load(stdout)` and then branch. Errors
   do not leave stdout empty.

Output is **compact single-line JSON** (`serde_json::to_string`, not `to_string_pretty`): one
line, one object, the conventional machine format.

Every `--json` document (success or error) begins with `"schema_version": 1`. This lets a
pinned agent detect a future breaking change instead of silently mis-parsing. Additive changes
(new fields) do not bump the version; removals or renames would.

---

## Flag

Both commands gain:

```rust
/// Emit a single JSON object on stdout instead of human-readable text
#[arg(long)]
json: bool,
```

- `--json` is opt-in. Without it, output is byte-for-byte identical to today (the
  `videre dedupe <dir> | xargs trash` pipe contract and the `videre search` line output are
  untouched).
- `--json` only changes **stdout**. Progress and summary lines stay on **stderr** and remain
  governed by `--silent` (dedupe). `--json --silent` therefore yields pure JSON on stdout and
  nothing on stderr - the clean agent invocation.
- `--json` is orthogonal to dedupe's `--output` / `--output-sqlite`: those still persist records
  to a file / DB exactly as before. `videre dedupe <dir> --output-sqlite db.sqlite --json` both
  persists and prints the JSON structure to stdout.

### `search`: `--scores` interaction

In `--json` mode every result already carries a `score` field, so `--scores` (which prepends a
score to *text* lines) has nothing to do. `--scores` is a **silent no-op** when `--json` is set;
it is NOT a clap conflict. An agent that always passes `--scores` and adds `--json` must not have
its call rejected.

---

## `search --json` schema

One object on stdout. Results are **per-path** (one entry per on-disk path, faithful to the
current line-per-path output) with `hash` included so an agent can group by image itself. This
keeps a single uniform shape across all three query kinds.

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

- `query.kind`: `"text"` | `"image"` | `"person"`. `query.value`: the query text, the `--image`
  path (as given on the command line), or the `--person` name.
- `results`: array, one entry per matched path.
  - **text / image**: each entry has `path`, `hash`, `score` (`score` is the cosine score of the
    entry's hash, `f64`, same value repeated across the duplicate paths of one hash - mirrors the
    existing text behavior).
  - **person**: `search_by_person` returns bare paths with no hash and no score, so person
    entries have `path` only; `hash` and `score` are omitted (not null).
- `count`: number of entries in `results` (equals `results.len()`).
- No matches (empty corpus subset, or no confirmed person photos) is **not an error**:
  `"results": []`, `"count": 0`, exit code 0.

## `dedupe --json` schema

One object on stdout, built from the existing `find_duplicate_groups` logic. Within a group,
`keep` is the oldest file (the current KEEP rule: `exif_date` wins, else `min(created_at,
modified_at)`), and `remove` is every other file in that group. Full `FileRecord` metadata is
reused for each file via its existing `Serialize`.

```json
{
  "schema_version": 1,
  "scanned": 1200,
  "duplicate_groups": [
    {
      "hash": "abc123",
      "keep":   { "path": "/photos/a.jpg", "hash": "abc123", "size_bytes": 20481, "ext": "jpg", "exif_date": "2019-06-01T10:00:00" },
      "remove": [ { "path": "/photos/dup_a.jpg", "hash": "abc123", "size_bytes": 20481, "ext": "jpg" } ]
    }
  ],
  "similar_groups": [
    {
      "hash": "phash:00000000000000ff",
      "files": [
        { "path": "/photos/x.jpg", "hash": "111", "size_bytes": 9001, "ext": "jpg" },
        { "path": "/photos/x_edited.jpg", "hash": "222", "size_bytes": 9500, "ext": "jpg" }
      ]
    }
  ]
}
```

- `scanned`: total number of files hashed in this run (the length of the records vector).
- `duplicate_groups`: exact-hash duplicate groups. Each has `hash`, `keep` (one `FileRecord`),
  and `remove` (array of `FileRecord`, may be length >= 1). No duplicates -> `[]`. The
  `keep`/`remove` split is safe to act on: every file in an exact group is byte-identical, so
  deleting the `remove` set is lossless.
- `similar_groups`: perceptual-hash near-duplicate groups, **present only when `--similar` is
  passed**; the key is omitted entirely otherwise. These are **review-only**, mirroring the text
  mode (which never prints them as delete candidates and says "review with videre report before
  deleting"). To avoid signalling a safe deletion that does not exist, a similar group is a **flat
  cluster**, not a keep/remove split: `{ "hash": "phash:<hex>", "files": [FileRecord, ...] }`,
  ordered oldest-first like the text output. Files in a similar group have **different** content
  hashes (they are near-duplicates, not identical), so each file's own `hash` differs from the
  group's synthetic `phash:` label - an agent must not treat any of them as a duplicate to delete
  without further judgment.
- `FileRecord` fields follow the existing serialization (path, hash, size_bytes, created_at,
  modified_at, ext, and the `skip_serializing_if = "Option::is_none"` optionals: phash, exif_date,
  gps_lat, gps_lon, width, height).

## Error object (both commands)

When `--json` is set and the command fails after argument parsing (bad DB path, missing
embeddings, unreadable directory, etc.), stdout still receives exactly one JSON object and the
process exits nonzero:

```json
{ "schema_version": 1, "error": { "message": "no embeddings found in photos.db for model ...; run videre embed first" } }
```

- `error.message` is the human error string (the `anyhow` error chain rendered as it is on stderr
  today, e.g. `{e:#}`).
- Exit code is nonzero (1), matching today's failure exit.
- A success document never contains an `error` key; an error document never contains
  `results`/`duplicate_groups`. An agent distinguishes them by exit code or by the presence of
  `error`.

**Boundary:** clap-level failures (unknown flag, `--json` used with a genuinely conflicting flag,
`--help`) are handled by clap before our code runs and print clap's own text to stderr with exit
code 2. That is out of scope - the always-valid-JSON guarantee covers runtime failures inside the
command body, not argument parsing.

---

## Implementation

All changes are inside the `videre` crate; no new dependencies (`serde` / `serde_json` are already
present, and `FileRecord` already derives `Serialize`).

**Result types (new, `#[derive(Serialize)]`):**

- `crates/videre/src/types.rs` (next to `FileRecord` / `DuplicateGroup`): dedupe output types, e.g.
  `DedupeJson { schema_version, scanned, duplicate_groups: Vec<DupGroupJson>, similar_groups:
  Option<Vec<SimilarGroupJson>> }`, `DupGroupJson { hash, keep: FileRecord, remove: Vec<FileRecord> }`,
  and `SimilarGroupJson { hash, files: Vec<FileRecord> }` (flat review cluster, distinct from the
  keep/remove split). `similar_groups` uses `skip_serializing_if = "Option::is_none"` so the key is
  absent without `--similar`.
- `crates/videre/src/commands/search.rs` (local): search output types, e.g.
  `SearchJson { schema_version, query: QueryJson, count, results: Vec<SearchHitJson> }`,
  `QueryJson { kind, value }`, and `SearchHitJson { path, hash: Option<String>, score: Option<f64> }`
  with `skip_serializing_if = "Option::is_none"` on `hash`/`score`.
- A shared tiny `ErrorJson { schema_version, error: ErrorBody }` / `ErrorBody { message }`. Place it
  in `types.rs` so both commands reuse it.

`schema_version` is emitted as a constant `1` (a plain field set at construction, or `#[serde]`
default - either is fine; keep it explicit).

**Control flow per command:**

```rust
pub fn run(args: XArgs) -> anyhow::Result<()> {
    if args.json {
        match run_core(&args) {            // run_core builds and returns the success struct
            Ok(doc) => println!("{}", serde_json::to_string(&doc)?),
            Err(e)  => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                std::process::exit(1);
            }
        }
        Ok(())
    } else {
        run_text(args)                     // the existing code path, unchanged
    }
}
```

The existing text path is refactored minimally so the data-gathering it already does can also feed
`run_core` (the scan/hash for dedupe, the search for search). Keep the refactor small: extract just
enough to build the struct; do not restructure unrelated logic.

## Testing

Integration tests (spawning the `videre` binary), added to the existing `tests/integration.rs`
(dedupe) and a search test file:

- **dedupe `--json`**: run against a fixture dir with a known duplicate; assert stdout parses as
  JSON, `schema_version == 1`, `scanned` matches file count, exactly one `duplicate_groups` entry,
  `keep` is the oldest file, `remove` has the expected path, and `similar_groups` is **absent**
  without `--similar` and **present** with it.
- **search `--json`**: text query returns a JSON doc with `schema_version`, `query.kind == "text"`,
  a `results` array whose entries carry `path`/`hash`/`score`, and `count == results.len()`.
- **search `--person` `--json`**: entries have `path` only (no `hash`/`score` keys).
- **`--scores` + `--json`**: does not error (exit 0) and produces the same JSON as `--json` alone.
- **error-as-JSON**: `search <bad.db> "q" --json` (or a DB with no embeddings) exits nonzero AND
  emits a single JSON object on stdout with an `error.message` key and no `results` key.
- **default output unchanged**: existing non-`--json` tests continue to pass verbatim (regression
  guard on the pipe contract).

Unit tests may additionally cover the serialization of the new structs (person hit omits
`hash`/`score`; `similar_groups: None` omits the key) following the pattern already in `types.rs`.
