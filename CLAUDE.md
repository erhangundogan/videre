# videre

A fast Rust CLI tool for managing a local media library: duplicate detection, semantic search, and face recognition, all built around a single SQLite database.

## What it does

`videre` is a single binary with fourteen subcommands. `videre scan` scans a directory
recursively, hashes every image file (BLAKE3), and writes the results into the database
(or JSONL with `--output`). `videre dedupe` reads that database and writes REMOVE
candidates to stdout one per line: ready to pipe into `trash` or `rm`. Bare `videre scan
<dir>` writes SQLite to the resolved default database (see `~/.videre` below); JSONL
output requires `--output`. `videre report` reads the SQLite database and generates an
HTML review page (or serves a live web UI). The remaining subcommands (`fix-dates`,
`prune`, `embed`, `search`, `faces`, `classify`, `locations`, `watch`) operate on the same SQLite
database to fix timestamps, sync metadata, compute semantic embeddings, run
text/image/person/category/location search, detect/label faces, classify images as
photo/screenshot/document/meme, and cluster GPS coordinates into named places. `videre config` shows or edits the resolved paths and
`~/.videre/config.toml` settings. `videre mcp` serves read-only search/find_duplicates/
stats tools over stdio for LLM agents. `videre stats` prints library totals and
per-command pipeline run status (last run, success/failed/crashed, duration) in one shot.

Note: `docs/superpowers/` design specs and implementation plans predate the videre rename and refer to the old `dupe-*` binary names historically; they are not rewritten here.

## Usage

```
videre scan [OPTIONS] [directory]     # directory optional when 'path' is set in videre config

Options:
  --output [<path>]        JSONL output file (appended). Bare --output (no value) targets ~/.videre/hashes.jsonl; must come AFTER the directory positional, or clap consumes the directory as the flag's value. Mutually exclusive with --db
  --db <path>              SQLite database to write; upserts by path; mutually exclusive with --output. When neither is given, records go to the resolved default db (see ~/.videre below). Accepts --output-sqlite as an alias, the original name
  --similar                Also compute and store perceptual hashes for near-duplicate detection
  --silent                 Suppress progress output on stderr
  --json                   Emit a single JSON object on stdout instead of human-readable text
```

`--output` and `--db` cannot be used together: passing both is an error. `scan` and `watch` accept `--output-sqlite` as an alias for `--db`; it was the original name, from when JSONL and SQLite were peer output *formats* rather than one destination and one opt-out.

`--similar` for `.mov`/`.mp4` files needs macOS (`qlmanage`), the first platform dependency `videre scan` has ever had. Every other `scan` code path stays platform-agnostic. On non-macOS, or if `qlmanage` fails for any reason, the file simply gets no `phash` (same graceful-skip behavior as any other undecodable file) rather than a hard error. Existing databases scanned before this feature shipped have `phash = NULL` for every video; re-run `videre scan --similar` to populate it. The feature is purely additive and never invalidates existing rows.

```
videre dedupe [OPTIONS]               # reads the database; no directory argument

Options:
  --db <path>   SQLite database (default: resolved from ~/.videre; see 'videre config')
  --similar     Also report perceptual-hash near-duplicate clusters (review-only)
  --silent      Suppress progress output on stderr (duplicate paths are always written to stdout)
  --json        Emit a single JSON object on stdout instead of human-readable text
```

## Output behavior

- **stdout**: REMOVE candidate paths, one per line (pipe-ready)
- **stderr**: scan progress and summary (suppressed by `--silent`)

KEEP candidate within each group = oldest `exif_date`; falls back to `min(created_at, modified_at)` if absent. `exif_date` values of `0000-00-00T00:00:00` (cameras with unset clocks) are treated as absent.

With `--json`, stdout is instead one compact JSON object, always (an error object plus a nonzero exit code on failure), never the REMOVE-path lines above.

Bare `videre scan <dir>` writes SQLite to the resolved default database (no JSONL). JSONL output only happens when `--output` is passed, with or without a value.

## Publishing to crates.io

All four crates publish under flat names (`videre`, `videre-core`, `videre-ml`,
`videre-api`): crates.io has no namespaces, so the `videre-` prefix is
convention only and reserves nothing. Publish order follows the dependency
graph and each step must land on the registry before the next resolves:
`videre-core` -> `videre-api` + `videre-ml` -> `videre`. `cargo publish
--workspace` does this ordering automatically and verifies every crate by
building it from its own packaged tarball first, so a broken dependent is
caught before anything irreversible happens.

`crates/videre/Cargo.toml` excludes `tests/fixtures/*` from the package: those
are ~2.2MB of sample media nothing needs at build or run time, and shipping
them would more than quadruple the download for `cargo install videre`.

## Platform support

**Intel macOS (`x86_64-apple-darwin`) does not build at all.** `ort-sys`
2.0.0-rc.13 ships no prebuilt ONNX Runtime for that target, so any build fails
with `no prebuilt binaries available for target x86_64-apple-darwin`, wherever
it runs. This is not a CI or cross-compilation problem and cannot be fixed by
choosing a different runner: it affects `cargo install videre` on an Intel Mac
identically. Found 2026-08-11 when the release matrix first tried the target.
Revisit if `ort` starts shipping Intel macOS binaries.

macOS is the development platform and everything works there. The workspace
also builds and runs on Linux, verified 2026-08-03 by building and running it
inside the official `rust:1` Docker image on both architectures.

Two Linux caveats, both measured rather than assumed:

**ARM64 Linux needs FP16 enabled explicitly.** `gemm-f16` (a transitive
`candle-core` dependency) emits FP16 instructions outside the baseline
`aarch64-unknown-linux-gnu` feature set, so the build fails with 11x
`error: instruction requires: fullfp16`. `.cargo/config.toml` sets
`-C target-feature=+fp16` for that target, which handles every cargo
invocation run from inside this repo: `cargo build`, `cargo test`, and `make
install` (which uses `cargo install --path crates/videre`). It deliberately
cannot help `cargo install videre` from crates.io: that reads the installing
user's own `~/.cargo/config.toml`, so the flag must be passed by hand there,
as documented in README's Install section. x86_64 Linux is unaffected
(`gemm-f16` takes a different code path there; verified by a full emulated
amd64 build). **Note the trap: `cargo check --workspace` PASSES on ARM64 Linux
even when `cargo build` fails**, because `check` never runs codegen. Never
treat a green cross-platform `check` as evidence that `cargo install` works.

**HEIC and video are macOS-only,** the one functional gap: HEIC images and
video poster-frames are decoded via macOS QuickLook (`qlmanage`), which has no
equivalent elsewhere. On non-macOS, both QuickLook entry points
(`videre_core::heic::heic_via_quicklook` and
`videre_ml::preprocess::decode_via_quicklook`) fail fast rather than silently:
they short-circuit on a `cfg!(target_os = "macos")` check and surface
`videre_core::heic::QUICKLOOK_UNAVAILABLE`, printed at most once per process
via `warn_quicklook_unavailable_once`. The consequence is that `.heic`/`.mov`/
`.mp4` get no thumbnails, embeddings, face detection, or `--similar` phash on
those platforms. They are still scanned, hashed, EXIF-extracted, and exactly
deduped. The guards use `cfg!()` (a runtime-constant `if`) rather than `#[cfg]`
so both branches type-check on every platform.

Per-command support matrix:

| | macOS | Linux |
|-|-------|-------|
| `dedupe`, `report`, `fix-dates`, `prune`, `locations`, `stats`, `mcp` | yes | yes |
| `embed`, `search` | yes (Metal GPU) | yes (CPU only) |
| `faces` | yes (CPU via ONNX Runtime) | yes (CPU via ONNX Runtime) |
| `watch` | yes | yes (`--heic` stage unavailable) |
| HEIC decoding (report, faces, embed, watch) | yes (`qlmanage`) | no |
| Video frame extraction (embed, `--similar` phash) | yes (`qlmanage`) | no |
| HEIC/video scanning, hashing, EXIF | yes | yes |
| `created_at` field | yes | always null |

## Build & run

```bash
cargo build --release
./target/release/videre scan ~/Photos                                    # populate the default db
./target/release/videre dedupe | xargs trash                             # delete duplicates
./target/release/videre scan --db ~/photos.db ~/Photos        # scan to an explicit SQLite db
./target/release/videre dedupe --db ~/photos.db                          # read from an explicit db
./target/release/videre report                                           # generate HTML report from the default db
./target/release/videre report --db ~/photos.db                          # generate HTML report from an explicit db
./target/release/videre fix-dates --dry-run                              # preview date fixes on the default db
./target/release/videre fix-dates                                        # apply date fixes
./target/release/videre prune --dry-run                                  # preview prune
./target/release/videre prune                                            # prune stale rows + sync metadata
./target/release/videre embed                                            # embed all images (resumable)
./target/release/videre search "sunset on beach"                         # text search
./target/release/videre search --image query.jpg                         # find similar images
./target/release/videre faces                                            # detect, embed, and cluster faces
./target/release/videre report --faces                                   # label faces in browser UI (localhost:7878)
./target/release/videre search --person "Alice"                          # find photos of Alice
./target/release/videre classify                                         # classify images as photo/screenshot/document/meme
./target/release/videre search --category screenshot                     # find images classified as screenshots
./target/release/videre report --by-date                                 # static Year/Month/Day drill-down gallery
./target/release/videre report --show-faces                              # live report with face/location lightbox metadata
./target/release/videre watch ~/Photos                                   # background: scan + faces + HEIC cache + location, looping, default db
./target/release/videre watch --db ~/photos.db ~/Photos       # same, against an explicit db
./target/release/videre config                                           # show resolved home dir, config.toml, and db paths
./target/release/videre config set db ~/photos.db                        # persist a default db in config.toml
./target/release/videre config set path ~/Photos                         # persist a default directory for dedupe/watch
./target/release/videre mcp                                              # serve MCP tools over stdio, default db
./target/release/videre mcp --db ~/photos.db                             # same, against an explicit db
./target/release/videre stats                                            # library totals + pipeline run status, default db
```

## CI

`.github/workflows/ci.yml` runs one job on every push to `main` and every pull
request: `make fmt-check` (`cargo fmt --all -- --check`). It needs no cargo
build and no dependency cache, since rustfmt parses the source directly, so it
finishes in under a minute despite the workspace pulling in candle and ONNX
Runtime.

The workspace was reformatted to zero drift on 2026-08-09 and this job is what
keeps it there. Before that it carried 370 hunks of drift, which made any stray
`cargo fmt` produce a large, plausible-looking, semantically empty diff: one
swept 20 unrelated `src/` files into a test-only commit, and nothing caught it
except reading `git show --stat`, because formatting changes are invisible to
the test suite.

Note that **`cargo fmt` has no per-file mode**. Arguments after `--` are
rustfmt options, not a file filter, so `cargo fmt -p videre -- path/to/one.rs`
silently formats the entire package. To format a single file, invoke
`rustfmt <file>` directly.

A `test` job runs the full suite on `ubuntu-latest` and `macos-latest`, with
`fail-fast: false` so one runner's failure never hides the other's result.
Linux is a first-class test target, not an afterthought: only four tests are
macOS-gated (video poster-frames via QuickLook) and everything else runs on
both.

**Tests never download model weights.** That is the application's job, done
when someone runs `videre embed` or `videre faces`, not a side effect of `cargo
test` on a fresh machine. Three tests need weights (`faces_resumability`, plus
`embed.rs`'s two on macOS); each calls `common::skip_without_models` and
returns early when the Hugging Face cache is cold. `faces_pipeline` is
deliberately *not* gated, because `commands/faces.rs` returns at the
`to_process.is_empty()` branch before loading anything; a regression guard
there runs the binary against a fresh `HF_HOME` and asserts nothing was
downloaded, so moving that model load earlier fails loudly instead of quietly
adding a 200MB download to every cold CI run.

Rust has no native skip, so a skipped test passes. Two things stop that from
becoming silent coverage loss. The skip message writes to fd 2 directly rather
than via `eprintln!`, because libtest captures the print macros for passing
tests and the message would otherwise only appear under `--nocapture`. And
`VIDERE_TEST_REQUIRE_MODELS=1` turns a cold-cache skip into a panic; CI sets it
after restoring its cache, so a cache that silently stops working fails the
build rather than disabling those tests.

CI caches `~/.cache/huggingface` keyed on `face_models.rs` and `embeddings.rs`,
so changing the buffalo_l repo or `DEFAULT_MODEL_ID` invalidates it instead of
reusing weights for a different model. On a miss an explicit step populates the
cache (~200MB Linux, ~1.6GB macOS, once per key) by seeding a database with a
real fixture image first, since warming against an empty database would
download nothing.

Two tests used to pass only on macOS, found by actually running the suite in
the `rust:1` image. ONNX Runtime is linked into every `videre` binary and
initialises at startup even for subcommands that never infer; on a host whose
CPU it cannot identify it prints `onnxruntime cpuid_info warning: Unknown CPU
vendor` before `main` runs, which broke an assertion that `--silent` produces
no stderr. That reproduces on ARM64 Linux, a supported platform, so
`common::stderr_without_library_noise` filters library chatter before such
checks. Separately, a test that makes a file unreadable with `chmod 000` is
meaningless as root (root bypasses permission bits), which is the default in a
stock Docker image, so it now probes whether permissions are enforced and skips
when they are not.

Clippy is deliberately not in CI yet: it reports 18 warnings, so a lint job
would need either `--allow`-ing them or a cleanup pass first.

## Test coverage

`cargo-llvm-cov` (installed via `cargo install cargo-llvm-cov`, plus the
`llvm-tools-preview` rustup component) measures unit-test line/region/function
coverage across the workspace. It must be invoked through the rustup-managed
toolchain explicitly, not plain `cargo llvm-cov`. This machine's default
`cargo`/`rustc` on `PATH` are a separate Homebrew Rust install (no rustup
component support), while `llvm-tools-preview` only installs into a
rustup-managed toolchain; mixing the two would pair an LLVM-22 rustc with
LLVM-21 coverage tools and produce incompatible profile data.

```bash
rustup run stable-aarch64-apple-darwin cargo llvm-cov --workspace --summary-only   # per-file table
rustup run stable-aarch64-apple-darwin cargo llvm-cov --workspace --html           # HTML report at target/llvm-cov/html/index.html
```

Coverage only reflects code exercised by unit tests (`#[cfg(test)]` modules)
running in-process. Integration tests under `crates/*/tests/` that spawn
`videre_bin()` as a child process (most CLI subcommand tests) are NOT
instrumented, so command modules like `fix_dates.rs`/`classify.rs`/`watch.rs`
show artificially low numbers here despite being covered by those integration
tests; read the per-file table as "unit-test coverage only", not overall test
coverage.

## Poking at the database directly

```bash
# Duplicate groups with file counts
sqlite3 ~/.videre/hashes.db "SELECT hash, COUNT(*) n FROM file_hashes GROUP BY hash HAVING n > 1"

# Total wasted space in MB
sqlite3 ~/.videre/hashes.db "SELECT SUM(size_bytes*(cnt-1))/1048576.0 FROM (SELECT size_bytes, COUNT(*) cnt FROM file_hashes GROUP BY hash HAVING cnt > 1)"

# Which model produced the stored embeddings (and how many are stale after a model change)
sqlite3 ~/.videre/hashes.db "SELECT model_id, COUNT(*), LENGTH(embedding)/2 AS dims FROM embeddings GROUP BY model_id"

# Filter the JSONL output by extension
jq 'select(.ext == "heic")' ~/.videre/hashes.jsonl
```

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic` `.mp4` `.dng`

## ~/.videre home directory

Every subcommand shares a home directory at `~/.videre` (override with the `VIDERE_HOME` env var), created lazily by writers (`scan`, `watch`, `config set`). Readers never create it. It holds `hashes.db` (default SQLite database), `hashes.jsonl` (default JSONL output, only written when `--output` is used bare), `config.toml` (optional overrides, currently just `default_db`), `locks/` (per-database-per-command `flock` files; see the `pipeline_runs` notes below), and `embeddings/` (per-model embedding databases; see below).

Database resolution order for every subcommand: explicit path (`--db`, accepted by every subcommand that takes one; `scan` and `watch` also accept `--output-sqlite` as an alias) > `default_db` in `config.toml` > `~/.videre/hashes.db`. Readers never create a database; if the resolved path doesn't exist they print `no database found at <path>; run 'videre scan <dir>' first` and exit 1 (arrives as the JSON error object under `search --json`).

`videre config` shows the resolved home dir, `config.toml` path, the `db` and `path` settings (labeled by their settable keys, with a set-command hint when unset), resolved db, and jsonl path. `videre config set db <path>` writes an absolute path to `config.toml` as `default_db`; `videre config set path <dir>` writes `default_path`, which `videre scan` and `videre watch` use when their directory positional is omitted (no built-in fallback: without it, the directory is required). Both setters preserve any other keys already present; `videre config unset db|path` removes a key. `videre scan <dir>` also adopts `<dir>` as `default_path` automatically the first time it is run with no `default_path` already set (a one-time convenience for the common case of a single photo library); it prints a one-line stderr note when it does (suppressed by `--silent`), and never overwrites an already-configured `default_path` on later runs.

## Project structure

```
crates/
  videre/
    Cargo.toml
    src/main.rs
    src/commands/{mod.rs,dedupe.rs,report.rs,scan.rs,fix_dates.rs,prune.rs,embed.rs,search.rs,faces.rs,classify.rs,watch.rs,config.rs,mcp.rs,stats.rs,locations.rs}
    src/{lib.rs,scanner.rs,hasher.rs,output.rs,sqlite_output.rs,types.rs}
    tests/{integration.rs,report.rs,prune.rs,watch.rs,multi_model.rs,faces_pipeline.rs,faces_server.rs,faces_resumability.rs,person_search.rs,mcp.rs,scan.rs,config.rs,embed.rs,fix_dates.rs,locations.rs,stats.rs,search.rs,fixtures/}
  videre-core/
    Cargo.toml
    src/lib.rs
    src/vectors.rs
    src/embeddings.rs
    src/classify.rs
    src/face_db.rs
    src/face_cluster.rs
    src/person_search.rs
    src/db.rs
    src/heic.rs
    src/hf_cache.rs
    src/location.rs
    src/location_cluster.rs
    src/geocode.rs
    src/thumb_cache.rs
    src/home.rs
    src/progress.rs
    src/library_stats.rs
    src/pipeline_runs.rs
    src/io_timeout.rs
    src/semaphore.rs
  videre-ml/
    Cargo.toml (lib-only, no binaries)
    src/lib.rs
    src/{device.rs,model.rs,preprocess.rs,search.rs,pipeline.rs,classify.rs}
    src/{face_models.rs,face_detect.rs,face_align.rs,face_embed.rs}
  videre-api/
    Cargo.toml (lib-only, no binaries)
    src/lib.rs
    src/{error.rs,types.rs,label.rs,faces.rs,images.rs,stats.rs,pipeline_status.rs}
```

The `videre` crate builds a single `[[bin]]` (`videre`, from `src/main.rs`) plus a lib target (`src/lib.rs`) exposing `scanner`, `hasher`, `output`, `sqlite_output`, and `types` to both the binary and the integration tests under `tests/`. `main.rs` dispatches to one module per subcommand under `src/commands/`. `videre-core` holds shared SQLite/db/cache/search helpers used by both `videre` and `videre-ml`. `videre-ml` is lib-only: all inference logic lives there, but every user-facing entry point is a subcommand in `videre`. `videre-api` is a lib-only facade over faces-labeling operations (list/assign/rename/dissolve/etc. plus face image bytes), called by the axum `--faces`/`--show-faces` server in `videre`.

## Key crates

- `clap`: CLI parsing (derive-based subcommands)
- `blake3`: fast exact hashing
- `rayon`: parallel hashing across CPU cores
- `walkdir`: recursive traversal
- `serde_json`: JSONL output
- `chrono`: date formatting
- `image`: image decoding and dHash perceptual hashing for `--similar` (implemented inline, no img_hash crate); the same dHash algorithm also runs against a QuickLook-decoded poster-frame for `.mov`/`.mp4` files, reusing `videre-ml`'s `decode_via_quicklook`
- `kamadak-exif`: EXIF metadata extraction (always on for jpg/jpeg/tiff/heic/dng)
- `rusqlite` (bundled): SQLite output for `--db` and `videre report`
- `filetime`: set file `mtime` portably for `videre fix-dates`
- `candle-core` / `candle-nn` / `candle-transformers`: SigLIP inference, Metal on macOS
- `tokenizers`: text tokenization for SigLIP
- `hf-hub`: Hugging Face model weight downloads
- `half`: f16 storage for embeddings
- `matrixmultiply`: pure-Rust blocked GEMM, used as a fast candidate filter in `videre faces`'s clustering step
- `ort`: ONNX Runtime bindings for face detection and embedding
- InsightFace buffalo_l: SCRFD-10GF face detector + ArcFace w600k_r50 embedder (ONNX weights, auto-downloaded via hf-hub to `~/.cache/huggingface/hub/models--WePrompt--buffalo_l/`, honouring `HF_HOME`; **not** `~/.cache/ort/`, which this file claimed for months and which has never existed)
- `rmcp`: official Rust MCP SDK, stdio server for `videre mcp`
- `schemars`: JSON-schema generation for MCP tool parameters
- `ureq`: blocking HTTP client for forward geocoding (`videre search --location`), no async runtime needed, matching this project's fully-synchronous architecture

## SQLite schema

```sql
CREATE TABLE file_hashes (
    path        TEXT PRIMARY KEY,
    hash        TEXT NOT NULL,
    size_bytes  INTEGER,
    created_at  TEXT,
    modified_at TEXT,
    ext         TEXT,
    mime        TEXT,
    phash       INTEGER,
    exif_date   TEXT,
    gps_lat     REAL,
    gps_lon     REAL,
    width       INTEGER,
    height      INTEGER,
    location_name TEXT,
    location_cluster_id INTEGER
);

CREATE TABLE IF NOT EXISTS faces (
    id            INTEGER PRIMARY KEY,
    hash          TEXT NOT NULL,
    bbox          TEXT NOT NULL,
    landmark      TEXT,
    embedding     BLOB NOT NULL,
    cluster_id    INTEGER,
    person_label  TEXT,
    confirmed     INTEGER DEFAULT 0,
    is_primary    INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS classifications (
    model_id      TEXT NOT NULL,
    hash          TEXT NOT NULL,
    category      TEXT NOT NULL,
    confidence    REAL NOT NULL,
    classified_at TEXT NOT NULL,
    PRIMARY KEY (model_id, hash)
);

CREATE TABLE IF NOT EXISTS pipeline_runs (
    command      TEXT PRIMARY KEY,
    started_at   TEXT NOT NULL,
    finished_at  TEXT,
    status       TEXT NOT NULL,
    duration_ms  INTEGER,
    summary      TEXT
);

CREATE TABLE IF NOT EXISTS location_clusters (
    id            INTEGER PRIMARY KEY,
    centroid_lat  REAL NOT NULL,
    centroid_lon  REAL NOT NULL,
    name          TEXT,
    photo_count   INTEGER NOT NULL,
    radius_km     REAL NOT NULL,
    created_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS geocode_cache (
    query       TEXT PRIMARY KEY,
    lat         REAL NOT NULL,
    lon         REAL NOT NULL,
    resolved_at TEXT NOT NULL
);
```

Re-scanning the same folder with the same SQLite file upserts (overwrites) existing rows via `INSERT OR REPLACE`. `phash` is stored as signed `INTEGER` (cast from `u64`). The algorithm is dHash: grayscale, resize to 9x8 (Lanczos3), compare horizontally adjacent pixels into 64 bits. `videre dedupe --similar` groups pairs within Hamming distance 10 by greedy single-linkage clustering; that output is review-only and never reaches stdout, which is what keeps `videre dedupe | xargs trash` safe. HEIC is deliberately absent from `PHASH_EXTENSIONS`, so `.heic` files get no near-duplicate hash at all. For `.mov`/`.mp4` files, `phash` is a dHash of the same QuickLook poster-frame `videre embed` decodes for SigLIP, not a byte-identical/video-content hash, so it only catches videos whose poster-frame looks alike (same-source re-encodes and trims that keep the opening frame; it will not catch a trim that cuts the opening frame). A flat or near-flat opening frame (a fade-in, a letterboxed dark frame, or any shot QuickLook grabs before it resolves) dHashes to all-zero or near-all-zero bits, same as it would for a flat-colored photo, and such videos would group with any other flat-opening video regardless of content. **Measured on a real 4,323-file library (2026-08-04): zero occurrences**. No `phash` was all-zero or all-ones, and no value repeated more than 3 times across 3,702 hashed files including 819 videos. The concern came from a synthetic solid-color test fixture; real camera video apparently doesn't open on a truly flat frame often enough to matter. Treat it as a theoretical edge case that has not been observed, not an expected failure mode. Output stays review-only regardless, so the cost of any false positive is a noisy group in `videre report`, never a wrong deletion. See `docs/superpowers/TECH_DEBT.md` for follow-ups (a size-proximity gate, and true multi-frame fingerprinting).

`faces` rows are keyed by `id` (auto-increment). `hash` links to `file_hashes`. `bbox` and `landmark` are JSON strings. `embedding` is a raw f16 BLOB (512-dim ArcFace, 1024 bytes). `cluster_id` is assigned by the two-stage clustering (average-linkage, then a centroid-merge pass); `person_label` and `confirmed` are set via `videre report --faces`.

A companion `faces_scanned` table (`hash TEXT PRIMARY KEY, scanned_at TEXT`) records every hash that face detection has processed, **including images where zero faces were found** (which produce no `faces` row). This is what makes `videre faces` resumable: the skip set is "already scanned", so no-face images are detected once rather than every run. Created by `create_faces_table` alongside `faces`; written per hash as detection proceeds.

`pipeline_runs` holds one row per tracked command (`scan`, `faces`, `embed`, `classify`, `dedupe`, `fix-dates`, `prune`, `locations`; `command` is the primary key, upserted on every run, not an append-only log). `status` is `running`/`success`/`failed`/`interrupted` as stored; a `crashed` status is never written to this column. It's computed only when reading (a `running` row whose per-db-per-command `flock` file isn't currently held by a live process is reported as `crashed` at read time). Those locks live at `<videre home>/locks/<db stem>-<hash of the canonical db path>.<command>.lock` (see `videre_core::home::locks_dir` and `pipeline_runs::lock_path_for`). The hash is load-bearing, not decoration: two libraries can both be named `photos.db` in different directories, and keying on the stem alone would make them share a lock, silently serializing unrelated libraries and making `videre stats` report one as running because the other is. Canonicalizing first also means a symlink and a relative path to the same database resolve to one lock. Locks moved here on 2026-08-03 from `<db path>.<command>.lock` sidecars, which scattered lock files into `~/.videre` and into whatever directory a `--db` database lived in; `acquire_lock` sweeps and deletes those legacy sidecars for all commands (only when it can exclusively `flock` one, proving no live process holds it), so a single command run after upgrading clears them. `videre watch` itself takes the same kind of lock for liveness but has no row here, since it has no "finished" moment during normal operation.

Because lock paths now resolve through `VIDERE_HOME` rather than sitting beside the database, any test that touches a lock **must** point `VIDERE_HOME` at a temp directory. Otherwise it writes into the developer's real `~/.videre/locks` and leaves permanent litter (test database names are random, so files accumulate rather than being reused). Both `pipeline_runs`' unit tests and every integration-test file that spawns the binary do this via an `isolated_home()` helper called from `videre_bin()`, which sets the env var once per test binary inside a `OnceLock` (tests share a process and run in parallel, so a per-test `set_var` would race every concurrent `getenv`); spawned children inherit it. See `videre stats` below for how this is surfaced.

That helper lives in `crates/videre/tests/common/mod.rs` and is shared by every integration-test file. It was previously copy-pasted into each one, which is how `faces_pipeline.rs` and `person_search.rs` came to be missing it entirely: a latent hazard rather than an observed leak, since neither happened to run a lock-taking command, but exactly the kind of omission a per-file copy invites. Calling `isolated_home` from `videre_bin` rather than from each test is what makes it impossible for a new file to forget.

`common` also provides `shared_cache_guard()`, which every test spawning `videre embed` or `videre faces` must hold. Those children resolve both SigLIP and InsightFace weights through the same Hugging Face cache (`~/.cache/huggingface/hub/`, honouring `HF_HOME`), which is not safe for two simultaneous first-time readers. It has to be a **file** lock: cargo runs each test file as its own process and runs those processes in parallel, so the racing readers are typically in different processes, which no `Mutex` can see. `faces_pipeline.rs` and `faces_resumability.rs` are two such binaries. Only contended on a cold cache, so the cost on a warm machine is negligible.

`classifications` is populated by `videre classify` (zero-shot photo/screenshot/document/meme classification, scoring `embeddings` rows already computed by `videre embed` against 4 fixed text prompts via cosine similarity, no new model, no image re-decoding) and queried via `videre search --category <name>`. Rows below the configurable `--margin` similarity gap between the best and second-best category are stored as `category = "unknown"` rather than a low-confidence guess.

`location_name` is a nullable TEXT column added by an idempotent `ALTER TABLE file_hashes ADD COLUMN location_name TEXT` migration (run on every `videre report` startup; harmless if the column already exists). It is not populated by the initial `videre scan`. It is populated lazily, one GPS coordinate at a time, by the `/api/location` endpoint when `--show-faces` is used: the first lightbox view of a photo at a given `(gps_lat, gps_lon)` triggers a reverse-geocode lookup, and the result is cached back into this column so later lookups for the same coordinate are free. `file_hashes.location_cluster_id` is added the same way, by `videre locations` (not populated by `videre scan`).

Every subcommand opens the database via `videre_core::db::open_wal`, which switches the connection to SQLite's WAL journal mode (`PRAGMA journal_mode = WAL`). WAL mode persists in the database file itself once set, so `open_wal` is idempotent, safe to call on every connection open, not just the first. This allows one writer plus many concurrent readers without "database is locked" errors, which matters now that `videre watch` can run in the background writing to the same file that a `videre report --show-faces` server has open for reading (and occasional writes, e.g. `/api/location`).

## EXIF fields

EXIF extraction runs automatically for `jpg`, `jpeg`, `tiff`, `heic`, and `dng` files. Fields are `null`/absent when the file has no EXIF data.

| Field | Type | Notes |
|-------|------|-------|
| `exif_date` | string | `DateTimeOriginal` formatted as `YYYY-MM-DDTHH:MM:SS`, camera-local time, no timezone; `0000-*` values from cameras with unset clocks are discarded (stored as null) |
| `gps_lat` | float | Decimal degrees, negative = South |
| `gps_lon` | float | Decimal degrees, negative = West |
| `width` | integer | From `PixelXDimension` |
| `height` | integer | From `PixelYDimension` |

## videre report

Reads `file_hashes` from a SQLite database and writes a self-contained HTML file. Two usage phases:

**Phase 1 (pre-deletion):** run without `--all` to review duplicate groups with KEEP/REMOVE badges before deleting anything.

**Phase 2 (post-deletion):** run with `--all` to browse the full cleaned collection with in-page similarity search. Files recorded in the database but no longer on disk are automatically excluded (checked at generation time; the database is not modified). `videre prune` removes stale rows permanently.

```bash
videre report                         # default db, output: <db>_report.html
videre report --db <db>               # explicit db
videre report -o <out>                # explicit output path
videre report --heic                  # embed HEIC thumbnails as base64 JPEG (macOS/qlmanage)
videre report --heic-original         # embed HEIC thumbnails + 1200px lightbox version
videre report --all                   # all-files gallery + in-page similarity search
videre report --faces                 # face labeling UI on localhost:7878 (requires videre faces)
videre report --by-date               # static Year/Month/Day drill-down gallery over KEEP files
videre report --show-faces            # live server: report with labeled-face + location metadata in the lightbox
```

`--by-date` is fully static: it writes an HTML file just like the default report or `--all` (same additive model, since it can be combined with `--all`/`--heic`/`--heic-original`), grouping KEEP files into a clickable Year > Month > Day hierarchy. No server is involved.

`--show-faces` is different: it switches `videre report` into server mode (the same `axum` server on `localhost:7878` that `--faces` starts), because the lightbox now shows each photo's labeled faces (clicking one navigates to `/person/<name>`) and a reverse-geocoded location name, both of which need a live backend. Labeled faces are queried from the `faces` table per request, and location names are resolved on demand via `/api/location` (see the `location_name` column below) rather than baked into a static file. Route split when combining with `--faces`:
- `--faces` alone: `/` serves the labeling UI (unchanged, no live report route).
- `--show-faces` alone: `/` serves the live report (with face/location metadata); no `/faces` route.
- `--faces --show-faces` together: `/` serves the live report, `/faces` serves the labeling UI.

Thumbnails and the lightbox also switch URL scheme in server mode: browsers refuse to load a `file://` subresource from an `http://`-served page, so `--show-faces` serves image/video bytes through `GET /api/raw?path=<path>` instead (a `LIVE_SERVER` flag baked into the page picks the URL scheme). `/api/raw` only serves paths already present in `file_hashes.path`. It's a deliberate allowlist, not a general file server. Static reports (no `--show-faces`) keep `file://` links, since the report itself is opened via `file://` there.

Report includes:

- Stats header (files scanned always shown; duplicate groups/files/wasted-space tiles and the toolbar only appear when there's at least one duplicate group)
- Toolbar: Expand all / Collapse all / Sort dropdown (wasted space, date kept oldest-first, date kept newest-first)
- Duplicate groups sorted by wasted space by default; sorting is instant DOM reorder
- Per-file: thumbnail preview, KEEP/REMOVE badge, filename, path + copy button, size, created, modified, EXIF date, GPS link, dimensions
- Image thumbnails via `file://` URL in static mode, or `/api/raw?path=...` in server mode (lazy-loaded, force-loaded on group expand)
- `.mov` and `.mp4` files shown as `<video>` thumbnail; click opens lightbox with playback controls
- `.heic` files: in static mode, "HEIC" text by default; `--heic` embeds a 240px JPEG thumbnail; `--heic-original` also embeds a 1200px lightbox version (macOS only, requires `qlmanage`, part of Quick Look/CoreServices). In server mode (`--show-faces`), HEIC always renders automatically, and `--heic`/`--heic-original` are ignored there, since thumbnails are converted lazily per request via `/api/raw?path=...&size=N`, checking `videre watch`'s thumbnail cache first before falling back to a live `qlmanage` conversion (eagerly converting every HEIC file before responding made server mode take minutes on a collection with many HEIC files)
- Lightbox overlay for full-size image/video viewing; Escape or backdrop click closes
- `--all`: gallery of files that exist on disk (200-card pages, lazy thumbnails) + "Similar" button per file; click opens a results panel with top-24 cosine matches using inline SigLIP f16 embeddings (requires prior `videre embed` run)

HEIC conversion (`--heic`/`--heic-original`, face thumbnails, and the original-image
endpoint) uses `qlmanage -t` (QuickLook), not `sips -s format jpeg`. Some HEIC files
(notably iPhone photos where iOS encodes rotation via the HEIF `irot` transform box
rather than a classic EXIF Orientation tag) come out sideways with plain `sips`
conversion because it copies the raw sensor-buffer pixels unrotated; `qlmanage`
applies the same rotation Finder/Preview/Photos do. This affects `videre faces`
detection, `videre embed`/`videre search` preprocessing, and every HEIC thumbnail path
in `videre report`. All of them shell out to `qlmanage`, not `sips`, for this reason.

## videre fix-dates

Reads `file_hashes` from a SQLite database and sets `modified_at` on each file to its `exif_date`. Only files with `exif_date` present are touched. Operates on all such files (KEEP and REMOVE alike: REMOVE files will be deleted afterward anyway).

```bash
videre fix-dates                 # default db; prompts for confirmation, then sets mtime = exif_date for all files with EXIF
videre fix-dates --db <db>       # explicit db
videre fix-dates --dry-run       # preview without modifying anything (never prompts)
videre fix-dates --yes           # skip the confirmation prompt (also: -y)
videre fix-dates --silent        # suppress per-file output (errors always shown; confirmation prompt is unaffected)
```

- `exif_date` is camera-local time with no timezone; treated as local system time when computing the UNIX timestamp
- Only `modified_at` is set (`created_at` / birth time requires a macOS-only syscall and is not supported)
- Files that no longer exist on disk (e.g. trashed duplicates still in the DB) are silently skipped and reported in the summary as "no longer on disk (skipped)"
- Exits with code 1 if any file could not be updated (missing files are not counted as errors)
- Before mutating anything, prints the count of files that will be touched and asks `[y/N]` on stderr; anything other than `y`/`yes` (including EOF, e.g. stdin piped from `/dev/null`) aborts with no changes and exit code 0. `--yes`/`-y` skips the prompt for scripted/non-interactive use. `--dry-run` never prompts, since it makes no changes regardless. The prompt is skipped entirely when there are zero files to update.

## videre prune

Syncs the SQLite database with the current filesystem state. Run after deleting duplicates and fixing dates.

```bash
videre prune                 # default db; apply
videre prune --db <db>       # explicit db
videre prune --dry-run       # preview without modifying the database
videre prune --silent        # apply without per-file output
videre prune --prune-unreachable  # also remove rows whose directory is missing
videre prune --force         # proceed despite the bulk-deletion guard
```

In a single pass:
- Deletes `file_hashes` rows for files no longer on disk, **unless the file's
  parent directory is also missing**
- Refreshes `modified_at` for surviving files from their current filesystem mtime
- Deletes orphan embedding rows whose hash has no remaining `file_hashes` entry, across **every** model database for that library, attaching and detaching each in turn and reporting a count per model
- Deletes thumbnail-cache files (240/1200px thumbnails, face crops, full-res originals) from the resolved cache directory whose hash has no remaining `file_hashes` entry (orphan cleanup). This is the only bound on that cache's otherwise-unlimited growth (see the `videre faces`/`videre watch` HEIC-caching notes above); `.tmp*` scratch files from an in-flight write are never touched

Shared-hash safety (applies to both embeddings and cache files): if two paths share the same hash and one file is deleted, the embedding/cache entry is only removed if no `file_hashes` row for that hash survives. Dry-run orphan counts are a lower bound (pre-existing orphans only; does not account for orphans created by the would-be deletions). Exits with code 1 if any row update or cache-file removal fails.

### Unreachable volumes, and why prune refuses

A missing file is only treated as deleted when its **parent directory still
exists**. If the parent is missing too, the directory or the whole volume is
gone, and the row is kept and reported as unreachable.

This is a data-safety guard, not tidiness. `prune` used to treat every
`metadata()` failure as a deletion, so running it with a drive unplugged deleted
every row for that drive. The rows are the cheap part: once they are gone their
hashes look orphaned, and the orphan sweeps then delete the embeddings and
cached thumbnails too. That is hours of recompute (a HEIC full-resolution decode
is ~7.6s) against minutes to re-scan rows. `videre watch --prune` runs the same
code unattended on a loop, so it could happen with nobody watching.

Deliberately not a mount-table lookup: on macOS `/Volumes` reports the same
filesystem as `/` when nothing is mounted there, so an unmounted volume leaves
nothing to query. Telling it apart from a deleted directory exactly would need
platform-specific enumeration or state recorded at scan time. The parent check
needs neither and behaves identically on Linux. The probe is bounded by
`io_timeout`, since a stale NFS/SMB mount can hang `metadata` forever, and a
timeout counts as *not* trustworthy: an unanswerable question must not authorise
a deletion.

Consequences worth knowing:

- **Skipped rows keep their hashes live**, so the embedding and cache sweeps
  never consider them orphaned. No change was needed in either sweep; protecting
  the rows protects everything downstream.
- **The skip count prints even under `--silent`**, which otherwise suppresses
  the summary, and names up to 5 missing directories. A run that quietly skips
  thousands of rows is exactly the silence this fixes.
- `--prune-unreachable` removes them anyway, for a folder that really is gone.
- A deliberately deleted subfolder is skipped too, and its rows linger until
  that flag is used. The conservative direction is the safe one.

Two further guards:

- **Bulk deletion.** A run removing more than 20% of the library *and* at least
  100 rows stops before deleting anything and requires `--force`. Both
  conditions: a percentage alone blocks a five-row fixture where three files
  were legitimately deleted, a count alone never trips on a small library. This
  catches what the parent check misses, such as a volume that remounts empty.
- **Repeated failure.** After 10 consecutive errors the run aborts, printing the
  first error verbatim rather than emitting one near-identical line per row.
  Consecutive, not cumulative, so scattered unreadable files do not abort a good
  run. Earlier changes stay committed; `prune` is idempotent, so a re-run after
  fixing the cause continues safely.

`videre watch --prune` can override **neither** guard: it runs unattended and
cannot ask, so `PruneArgs::for_watch_stage` pins both to false and a unit test
asserts it.

`videre prune`'s runs are tracked in `pipeline_runs` (added 2026-08-01), visible via `videre stats`.

## videre locations

Clusters existing GPS coordinates (`file_hashes.gps_lat`/`gps_lon`, already
extracted by `videre scan`'s EXIF parsing) by geographic proximity, using
average-linkage agglomerative clustering over haversine (great-circle)
distance, the same *philosophy* as face clustering (repeatedly merge the two
closest clusters by size-weighted average distance), a separate
purpose-built implementation in `videre_core::location_cluster` since face
clustering's cosine-distance/ArcFace-specific implementation doesn't apply.
Unlike face clustering, there is no quality gate and no held-out
singletons. Every GPS coordinate is valid data, so a single photo taken
somewhere unique still gets its own one-member cluster.

```bash
videre locations                  # cluster + persist + print summary, default db, radius=15km
videre locations --radius 25      # override clustering granularity
videre locations --json           # single JSON object (mutually exclusive with --geojson)
videre locations --geojson        # GeoJSON FeatureCollection
videre locations --db <path>      # explicit db
videre locations --silent         # suppress the per-run summary
```

Every run is a **full recompute**: truncates `location_clusters` and clears
`file_hashes.location_cluster_id`, then reclusters from scratch over every
distinct `(gps_lat, gps_lon)` pair (rounded to 6 decimals, same unit
`videre watch`'s location stage uses). There's no expensive detection step
to make this incremental/resumable (GPS already sits in `file_hashes`), and
the clustering math itself (haversine distances over ~5,500 coordinates) is
sub-second, but real-library measurement found the per-coordinate
`file_hashes` UPDATE (one per distinct coordinate, matched via the
unindexable `ROUND(gps_lat, 6) = ROUND(?, 6)` predicate against the whole
table) dominates: ~8 minutes on a 70k-file library. The whole recompute runs
inside one transaction, so it holds the single WAL writer lock for that
entire window, so a concurrent `videre watch` write blocks until it finishes.
Not fixed yet (see TECH_DEBT.md); tolerable for a manually-invoked command,
but noted here since "full recompute" reads as cheaper than it measures at
real scale. Cluster IDs are **not stable across reruns**, only stable within one
run's output, mirroring the face-clustering precedent (durable state,
`person_label`, lives on individual face rows, not the numeric cluster ID;
any future per-cluster customization would presumably follow suit).
`--radius` (default 15km, "which city was I in" granularity) is the one
tunable parameter.

Cluster names are resolved via the **existing offline** reverse-geocoder
(`videre_core::location::location_name`, no network calls) called once per
cluster centroid (the unweighted mean of its member coordinates),
independent of the per-coordinate `location_name` column `videre watch
--location` populates, so this command doesn't depend on that stage ever
having run. `photo_count` counts `file_hashes` *rows* (physical files,
including duplicate paths sharing a hash+coordinate), not distinct hashes or
coordinates.

Tracked in `pipeline_runs` as an 8th tracked command, `"locations"`
(`videre_core::pipeline_runs::TRACKED_COMMANDS` grows to 8 entries).
Resolves its db via `resolve_reader_db` (writes clusters + assigns
`location_cluster_id`, so it matches `embed`/`classify`/`faces`/`prune`, not
`dedupe`/`mcp`/`stats`'s must-exist readers).

`--json` emits `{"schema_version": 1, "radius_km": 15.0, "clusters": [...]}`,
each cluster carrying `id`/`name`/`centroid_lat`/`centroid_lon`/`photo_count`.
`--geojson` emits a standard `FeatureCollection` of `Point` features, with
`coordinates: [lon, lat]` per the GeoJSON spec's own (reversed from this
project's usual lat-then-lon) convention, so the output can be dropped
directly into geojson.io, QGIS, or any other GeoJSON-consuming tool (a live
map view is deliberately not built in this repo; see the design spec's
Non-goals section for why, including OpenStreetMap's tile-usage-policy
objection).
`--json` and `--geojson` are mutually exclusive (`conflicts_with`, same
mechanism as `scan`'s `--output`/`--output_sqlite`).

Zero GPS-bearing rows in the library is not an error: prints "0 location
cluster(s) found" (or an empty `clusters`/`features` array) and exits 0.
Centroid-as-unweighted-mean is a known, accepted limitation near the
antimeridian (+/-180 longitude) or poles, not solved for a
15km-granularity feature.

## videre embed / videre search

`videre embed` (optionally `--db <db>`) embeds every unique image hash (SigLIP siglip2-base/16-384, 768-dim,
L2-normalized f16 BLOB) into an `embeddings` table keyed by content hash. Resumable:
re-running processes only missing hashes. `--batch` (default 32, **clamped to
`videre_ml::model::MAX_SAFE_BATCH` = 96**: above a threshold measured between 121 and
127, the batched inference path silently returns embeddings that don't match a
one-at-a-time baseline: no error, no NaN, just wrong vectors, with only the trailing
partial batch correct. Values over 96 are reduced with a warning. Do not raise the
constant without re-running the ignored `batched_embeddings_match_one_at_a_time` test in
`videre-ml`; checking output for zero/NaN vectors does NOT detect this, since `--batch
256` produces neither and is still fully corrupt. See
`docs/superpowers/2026-08-04-embed-batch-corruption-investigation.md`), `--chunk` (rows per
transaction, default 500), `--silent`. HEIC via `qlmanage` (see videre report HEIC note
above); `.mov`/`.mp4` are embedded too, via one representative frame extracted the same
way (`qlmanage -t`, macOS only) rather than decoding the full video, a single-frame,
not-motion-aware embedding, so video search quality is weaker than photo search (see
`docs/superpowers/TECH_DEBT.md` for the open follow-ups on this). Video hashes are
excluded from `videre classify` (none of its four categories fit a video frame). DNG is
still skipped (the `image` crate has no DNG decoder), excluded from `EMBEDDABLE_EXTS`
up front (fixed 2026-08-01; previously it was queried as pending and failed to decode
every single run), so a library with DNG files no longer wastes a decode attempt on
each of them every time `videre embed` runs. EXIF metadata is still available for DNG
files from the scan, independent of embedding support.

`videre search "query"` or `videre search --image photo.jpg` (optionally `--db <db>`) prints matching
paths to stdout (all duplicate paths per matched hash). `-k` top-k (default 20),
`--scores` prepends cosine score. Brute-force exact scan; no ANN index at this scale.
`videre search ... --json` emits a single JSON document (`schema_version`, `query`,
`count`, `results` with per-path `hash`/`score`; `--person` hits carry `path` only;
`--category` hits carry `path`+`hash` but no `score`, since it's set membership, not a ranked query)
instead of the printed paths above; `--scores` is a no-op under `--json` since the
score is always included.

`videre search --person "Alice"` queries the `faces` table for confirmed rows whose `person_label` matches (case-insensitive prefix) and prints matching image paths. Requires a prior `videre faces` run and labels applied via `videre report --faces`.

`videre search --location "<place>" [--radius <km>]` forward-geocodes an
arbitrary place name (e.g. "Berlin, Germany", not limited to places already
in your library, unlike `videre locations`' persisted clusters) via the
Nominatim (OpenStreetMap) free public geocoding API, the first network
call this CLI ever makes. It then finds photos within `--radius` km (default
20) of that point, sorted by distance ascending and truncated to `-k`
(unlike `--person`/`--category`, which ignore `-k` entirely: `--location` is
a ranked "k nearest" query, closer in spirit to text/image mode). Results
are cached locally in a new `geocode_cache` table keyed by the normalized
query string, so a repeated query never repeats the network call. This
makes `--location` the one `videre search` mode that writes to the
database (every other mode stays read-only) and the one mode **not**
exposed via `videre mcp` (same precedent as `--category`, which
`videre mcp`'s search tool also excludes). `--json` hits carry
`path`/`hash`/`distance_km` (no `score`, since this isn't a ranked semantic
match); `--scores` in text mode prepends `distance_km` instead of a cosine
score for this one mode.

`mime` holds the type identified by the file's magic bytes, independent of its
name (`videre_core::mime_probe`). It is detected during the scan's existing
BLAKE3 read, so it costs no extra I/O: `hash_file_inner` already fills a 64KB
buffer and the signature is in the first 12 bytes. Decoding, perceptual
hashing, EXIF extraction, and the photo/video split all route on `mime` when
set and fall back to `ext` when NULL, which is the state of every row written
before this column existed until the library is re-scanned. The idempotent
`ALTER` runs in `db::open_wal`, so every command migrates the database on open
rather than only `scan`; readers query the column, so migrating on write alone
would break `dedupe` and `stats` on an un-rescanned library.

Two details worth knowing. Classic QuickTime `.mov` files carry no `ftyp` box,
beginning with `wide` or `mdat` instead; 2.5% of a real library is this shape,
and detection accepts those top-level boxes. And `.dng` reports `image/tiff`,
because DNG genuinely is a TIFF variant, so `ext = 'dng'` explicitly vetoes
embeddability: TIFF is embeddable, DNG is not decodable, and routing on mime
alone would revive the bug fixed 2026-08-01.

An unrecognised file records `application/octet-stream` rather than NULL, so
`mime IS NULL` means only "never scanned". `videre scan --retry-incomplete`
uses that to process just the files a previous scan did not finish, which
matters because a full scan re-reads everything: measured at roughly 460GB and
9m50s on a 70,601-file library, against 1.8s to walk it. The sentinel is
bookkeeping only; `effective_mime` treats it exactly as NULL and falls back to
the extension, so a file whose bytes are merely unidentified is still
processed. This mirrors `faces_scanned`, which records images where zero faces
were found for the same reason.

`.mov`/`.mp4` files are checked for a video track before QuickLook is invoked
(`videre_core::video_probe`). `qlmanage -t` does not fail on a container with
no video track, it hangs, so videre used to pay the full 20s
`QLMANAGE_TIMEOUT` per such file on every run: nothing marks a file
permanently unembeddable, so `pending_images` keeps returning it. Measured on
a real 70,601-file library, three audio-only Live Photo companions cost 60s
per `videre embed` run and another 60s per `videre scan --similar`. The probe
walks ISO-BMFF boxes for a `vide` handler and fails open, so any parse error
or unrecognised layout proceeds to QuickLook exactly as before.

Model choice lives in `config.toml` (`videre config set model <id>`), not an
environment variable; see "Choosing a model" below. The measurements that drove
the current default are recorded there and in
`docs/superpowers/2026-08-04-embed-batch-corruption-investigation.md`.

One env override remains:

- `VIDERE_EMBED_DTYPE=f16` switches inference to half precision: ~11% faster on
  pure jpg/png, ~7% on a realistic mix, no memory saving, no meaningful quality
  change (worst f16-vs-f32 cosine 0.999794 over 190 images, inside the f16
  *storage* quantization already applied). Opt-in because 7% didn't justify
  perturbing an existing library.

Two things measured and rejected, recorded so they aren't retried: raising
`--batch` (silently corrupts; see `MAX_SAFE_BATCH`) and collapsing the
per-image GPU readback into one transfer (0.988x, no effect).

Model weights auto-download from Hugging Face on first use of a command that
needs them, not at install or on first run of any command. Measured on disk
2026-08-10: the default `google/siglip-base-patch16-224` is **778MB**, and
`videre faces`'s InsightFace `buffalo_l` is a separate **182MB**. Non-default
models are larger (`siglip2-base-patch16-384` 1.4GB, `siglip-so400m-patch14-384`
3.3GB) and are only fetched if selected with `--model` or `config set model`.

`scan`, `dedupe`, `fix-dates`, `prune`, `stats`, `locations`, and `report`
without similarity search need **no model at all**.

### Where embeddings live

Embeddings are **not** in the main database. Each (library, model) pair gets its
own SQLite file:

```
~/.videre/embeddings/<db stem>-<hash16>/<owner>--<model>.db
```

for example `~/.videre/embeddings/hashes-3f9a1c04e7b25d68/google--siglip2-base-patch16-384.db`.
The model segment replaces `/` with `--`, matching the Hugging Face cache
convention. The library segment reuses `pipeline_runs::lock_path_for`'s scheme:
the canonical path's file stem plus 16 hex digits of a hash of that path. The
hash is load-bearing, not decoration, for the same reason it is there: two
libraries can both be named `photos.db` in different directories, and keying on
the stem alone would silently merge their embeddings. Canonicalizing first also
collapses a symlink and a relative path to one directory.

Why per library rather than one global file per model, given that content
hashes would allow sharing: `videre prune` cannot see another library's
`file_hashes`, so a global layout would let one library's orphan sweep delete
vectors another library still needs. The thumbnail cache has the same latent
flaw and it does not matter there, because a thumbnail regenerates in
milliseconds; an embedding costs hours.

`videre_core::embeddings_db` is the only module that knows this layout. It
attaches the chosen model's file to the main connection under the alias `emb`,
so every query reads `emb.embeddings`. Two consequences worth knowing:

- **`sqlite_master` is per database.** A probe for the table must say
  `emb.sqlite_master`; the unqualified form returns 0 and every caller reads 0
  as "nothing embedded yet", so getting this wrong makes search return no
  results and report success.
- **No atomic commit across attached databases in WAL mode.** No transaction may
  write to both `main` and `emb` and depend on both landing. Nothing here needs
  that, but do not merge the two for tidiness.

Files are created with `page_size = 16384` rather than SQLite's 4096 default,
set on the empty file before WAL and before any table exists, since SQLite
silently ignores it afterwards and needs a full `VACUUM` to apply. Measured
2026-08-05 over 20,000 synthetic rows extrapolated to 70,587: at 4096 a
1152-dimension model takes 282MB and a 768-dimension one 143MB; at 16384 they
take 189MB and 128MB. 8192 was the first proposal and helps only the
1152-dimension case.

Schema inside each model database:

```sql
CREATE TABLE embeddings (
    hash        TEXT PRIMARY KEY NOT NULL,
    model_id    TEXT NOT NULL,
    embedding   BLOB NOT NULL,
    embedded_at TEXT NOT NULL
);
```

`model_id` is redundant with the filename but retained: it makes a stray file
self-describing and lets every `WHERE model_id = ?1` clause work unchanged.

### Choosing a model

`embed`, `search`, `classify`, `report`, and `mcp` all take `--model <id>`,
resolved as `--model` > `default_model` in `config.toml` > `DEFAULT_MODEL_ID`.
`videre config set model <id>` writes that key, and `videre config` shows the
resolved value whether or not it is set. The resolved
id is computed once per invocation and handed to both the database layer and
the weight loader, so the vectors written can never come from a different
checkpoint than the file they are written into.

`videre embed` is the only command that creates a model database. Readers error
instead, naming the models that do exist, so a typo produces a clear message
rather than an empty result set. `report` is the exception: a missing model
disables its in-page similarity search with a note rather than failing a report
that works fine without vectors, and `mcp` warns but still serves, since
`find_duplicates` and `stats` never touch embeddings.

Switching models no longer invalidates anything. The previous model's vectors
stay intact and queryable via `--model`; the new one simply starts from zero.

### Upgrading from before the split

Embeddings written by 0.9.x sit in an `embeddings` table in the main database.
**That fallback was removed in 0.11.0 and no longer exists in the code**
(`LEGACY_FALLBACK_REMOVE_IN` is gone with it). A library last embedded on 0.9.x
now gets the same clear error as any other missing model, naming what does
exist and the command to run, rather than silently returning zero hits.

Nothing is deleted: the old `embeddings` table sits untouched in the main
database and can be dropped by hand. Re-running `videre embed` builds the
per-model database from scratch.

## videre classify

`videre classify` (optionally `--db <db>`) classifies every embedded hash as `photo`,
`screenshot`, `document`, or `meme` via zero-shot classification: each stored embedding
is scored by cosine similarity against 4 fixed text prompts (`crates/videre-ml/src/classify.rs`'s
`CATEGORY_PROMPTS`), embedded once via the same SigLIP text tower `videre search` uses.
No new model, no image re-decoding. This runs entirely over vectors `videre embed`
already computed. Resumable: re-running only classifies hashes not yet in
`classifications`, unless `--reprocess`. `--margin` (default 0.05) is the min similarity
gap between the best and second-best category to accept a result; below that, the row
is stored as `unknown` rather than a low-confidence guess.

```
videre classify                     # classify all embedded-but-unclassified hashes, default db
videre classify --db <path>         # explicit db
videre classify --model <id>        # classify a specific model's vectors (default: resolved model)
videre classify --reprocess         # re-classify everything, including already-classified hashes
videre classify --silent            # suppress per-image progress
videre classify --margin <f32>      # min similarity gap to accept a category (default: 0.05)
```

`videre search --category <name>` queries the `classifications` table for rows matching
`category` and prints matching image paths (or, under `--json`, a `results` array with
`path`+`hash` per entry, with no `score`, since this is set membership, not a ranked query).

## videre faces

Detects faces in every image recorded in the database, embeds each face with ArcFace, and clusters detected faces into identity groups using a two-stage pipeline: average-linkage agglomeration, then a centroid-merge pass that reunites one person's fragmented sub-clusters (see below).

```
videre faces                            # default db; scan not-yet-scanned images (resumable)
videre faces --db <db>                  # explicit db
videre faces --limit <n>                # scan at most N not-yet-scanned images, then stop (resumable; skips clustering)
videre faces --reprocess                # re-detect and re-embed all hashes
videre faces --recluster                # skip detection; re-run clustering on existing embeddings
videre faces --dry-run                  # detect and embed but do not write to db
videre faces --batch <n>                # images per ONNX batch (default: 8)
videre faces --silent                   # suppress per-image progress
videre faces --eps <f32>                # average-linkage cosine-distance radius (default: 0.6)
videre faces --min-cluster-size <n>     # minimum faces per cluster (default: 3)
videre faces --merge-sim <f32>          # centroid-merge similarity threshold (default: 0.35; 1 disables)
videre faces --min-face-size <px>       # min face bbox side (px) to cluster; smaller held out (default: 80; 0 disables)
videre faces --max-generic-sim <f32>    # distinctiveness gate: hold out faces too similar to the average face (default: 0.4; 1 disables)
videre faces --workers <n>              # worker threads for detection/embedding (default: 2x available core count)
videre faces --profile                  # print per-stage timing (load/detect/align/embed/db_write) after the run
videre faces --qlmanage-concurrency <n> # max concurrent qlmanage (HEIC decode) subprocesses, process-wide (default: 6)
```

Uses InsightFace buffalo_l: SCRFD-10GF for detection, 5-point landmark alignment, ArcFace w600k_r50 for 512-dim L2-normalized embeddings. Weights are downloaded from `hf-hub` on first run. ONNX Runtime (`ort`) runs inference on CPU (an explicit per-worker intra-op thread cap; see the concurrency note below; the macOS CoreML execution provider was measured to give no speedup for these models and is not used). HEIC images are converted via `qlmanage` (see videre report HEIC note above) before detection, unless a cached full-resolution decode already exists in the thumbnail cache as `<hash>_original.jpg` (written by `videre watch --heic`, or lazily by `videre report --show-faces`'s original-image endpoint), in which case that cached JPEG is read directly instead of paying for another `qlmanage` subprocess. Detection's bbox coordinates are stored relative to whatever image detection ran on, so this cache must be full resolution (not the 240/1200px thumbnail sizes), and `videre watch --heic` decodes at full resolution specifically to feed both this cache and its own thumbnails from one decode. Real measurement on a real library: ~108ms per cached HEIC load vs. ~7.6s for a live decode, roughly 70x faster once the cache is warm; a single-pixel bbox rounding difference (JPEG recompression noise) was observed in 1 of 20 checked coordinates against live-decode ground truth, not a correctness issue. Falls back to a fresh live decode when the cache hasn't been populated for a hash yet, so detection works correctly even if `videre watch --heic` has never run.

Detection is **resumable**. Every processed hash is recorded in a `faces_scanned` table, including images where zero faces were detected, which leave no `faces` row. The skip set for a run is "already scanned" (unioned with "already has faces", so a first run after upgrading doesn't redo prior work), not merely "has a face", so a no-face image is detected exactly once ever rather than re-detected on every run. Faces and the scanned marker are committed per hash as the run proceeds, so an interrupt (Ctrl-C) loses at most the in-flight image and a rerun continues where it left off. `--limit <n>` processes at most N not-yet-scanned images then stops (for chipping away at a large library in bounded chunks); a limited run skips the final clustering step (it is an O(n^2) whole-library pass not worth repeating after every chunk). Run `videre faces --recluster` once scanning is complete.

`videre faces` runs `--workers` worker threads concurrently (default: 2x the machine's available core count; see below for why), each with its own ONNX sessions (intra-op-thread-capped so they don't collectively oversubscribe the machine) processing a round-robin-assigned slice of the work, not contiguous chunks, so one worker doesn't inherit a disproportionately HEIC-heavy (slower) subset. All database writes happen on a single coordinator thread that receives results from workers over a channel; workers never touch the connection directly. The 2x-cores default (rather than a flat 1:1 mapping) comes from real profiling data: HEIC file loading (via a `qlmanage` subprocess) averaged ~52x longer than non-HEIC loading in one measurement, and since that wait is I/O-bound rather than CPU-bound, oversubscribing keeps cores busy with other workers' CPU-bound detect/embed work while some workers are blocked on the subprocess. A real A/B measurement on the full pipeline (not just the profiling estimate) found a ~3.23x wall-clock speedup with default workers vs. `--workers 1` on a 10-core machine. See docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md for the full design and why this approach (a symmetric worker pool) was chosen over a producer/consumer split with separate loader/inference pools. HEIC decoding itself is further capped independently of `--workers`: all `qlmanage` subprocess launches, across every subcommand, share one process-wide semaphore (`videre_core::heic::qlmanage_semaphore`) limiting concurrent conversions, 6 by default, raised from 3 after the 3.23x measurement above showed CPU sitting at only 477% of a possible 1000%, a hint that HEIC-heavy runs were bottlenecked on this cap rather than on cores. That hint was confirmed by a real re-measurement (same 300-image sample, default workers): `--qlmanage-concurrency 3` (the old default) ran in 75.10s at 498% CPU, `--qlmanage-concurrency 6` (the new default) ran in 60.86s at 663% CPU, a further ~1.23x wall-clock improvement on top of the earlier 3.23x, for a combined ~4.48x over the original fully-serial baseline (272.78s -> 60.86s). `videre faces --qlmanage-concurrency <n>` overrides the default for a single run. Measured 2026-07-31 (same 300-image sample/methodology): raising further to 8 or 10 only buys 1.3%/4.4% more wall-clock with diminishing returns (cap=6: 51.59s/728% CPU; cap=10: 49.42s/762% CPU). CPU still isn't fully saturated even at cap=10, but per-image detect time crept up alongside it, meaning the extra concurrency mostly shifts the bottleneck into CPU contention rather than delivering free parallelism. Default stays 6; `--qlmanage-concurrency 10` remains available as a manual opt-in for a small win, not something worth changing the shipped default for.

**The cap is per-process, not system-wide, and that matters when two videre commands overlap.** Two processes therefore permit up to 12 concurrent `qlmanage` conversions against macOS's single shared per-user QuickLook agent, exactly the pile-up the cap exists to prevent. Measured 2026-08-04 running `videre faces` and `videre embed` simultaneously on a real library: HEIC load averaged **16,339ms** against ~7.6s uncontended, and one file blew past `QLMANAGE_TIMEOUT` (20s) entirely, a file that converted in **0.39s** standalone immediately afterwards, so ~51x degradation rather than ~2x. Impact is bounded: the skipped file was correctly *not* written to `faces_scanned`, so it was retried and self-healed on the next run. It is a throughput and predictability problem, not data loss. But the intended `videre watch` + manual-command workflow makes overlap the normal case, not an edge case. See `docs/superpowers/TECH_DEBT.md` for the options considered (a cross-process token, raising the timeout, or having `watch` yield its budget).

Resumability's correctness is unchanged: workers never touch the database, so a hash can never end up marked scanned without its faces being durably written first, no matter how many workers are running. Restart always correctly continues from the true set of completed hashes. What does change is how much gets re-done after a kill: even the single-threaded pipeline already defers marking a face-bearing image as scanned until its whole `batch`-sized chunk's single embed call resolves (only zero-face images are marked immediately), so an interrupt today can already cost up to `batch` (default 8) images of reprocessing, not 1. With `--workers` workers each independently chunk-batching their own partition, that window becomes up to `workers * batch` images, e.g. 160 with the defaults on a 10-core machine (20 workers x 8 batch). Still fully correct on resume, just a larger bounded "wasted work" window than before; `--limit` remains the lever for users who want tighter control per invocation.

Faces below `--min-cluster-size` are left as unassigned singletons rather than forming
a small cluster. `--recluster` re-runs clustering with new `--eps`/`--min-cluster-size`/`--merge-sim`/`--min-face-size`
values without re-detecting or re-embedding, useful for tuning cluster tightness
after an initial `videre faces` run.

Before clustering, a two-signal quality gate holds low-quality faces out of clustering
entirely (they come back as unassigned singletons, still visible and manually labelable).
Low-quality faces embed into near-degenerate ArcFace vectors that all point the same
generic direction regardless of who they are, so if clustered they pile into one large
*mixed* junk cluster (and then get centroid-merged into an even bigger one). A face is
held out when it fails **either** signal:

- **Size** (`--min-face-size`, default 80px): faces whose smaller bbox side is under this
  many pixels. Tiny crops (e.g. distant faces in group shots) upscale to ArcFace's 112px
  input as mostly blur. On real data, genuine person clusters are essentially all >100px
  per side while a degenerate junk cluster sat at ~60px median.
- **Distinctiveness** (`--max-generic-sim`, default 0.4): faces whose embedding cosine
  similarity to the population-average embedding exceeds this. Occluded (sunglasses,
  masks), non-frontal (profile), blurry, or false-positive detections (e.g. a carved
  statue face) carry little identity information, so ArcFace maps them close to the
  generic average. This catches low-quality faces the size gate misses (a large but
  sunglassed or profile face). On real data, 0.4 removed ~78% of a mixed junk cluster
  while touching 0% of confirmed real-person clusters; a genuinely distinctive person's
  own occluded shots survive because his many high-quality frontal photos anchor a
  centroid far from the generic average.

`--min-face-size 0` and `--max-generic-sim 1` each disable their signal. Residual junk
that slips through both gates (faces in the ambiguous overlap zone) is best cleared with
the labeling UI's "Dissolve cluster" button rather than by tightening the gates further,
which would start removing real faces.

Clustering is two-stage. First, average-linkage agglomeration groups faces by
average pairwise cosine distance (`--eps`). Average-linkage alone still fragments a
single person into several clusters, because one person's photos legitimately spread
wide in embedding space (pose, lighting, age) and the average cross-cluster distance
can exceed `--eps` even for the same identity. So a second centroid-merge pass then
joins any two clusters (each already at least `--min-cluster-size`) whose L2-normalized
mean embeddings are at least `--merge-sim` cosine-similar. Deciding on centroids rather
than raw pairs cancels per-face spread: on real data, confirmed *different* people never
exceed ~0.29 centroid similarity while one person's fragments run 0.37-0.76, so the 0.35
default reunites fragments with a safe margin. Only established clusters take part in the
merge, never lone singletons. A single bad crop can sit within `--merge-sim` of a
different person's centroid, whereas a whole cluster's averaged centroid cannot.

Clustering runs after every full (non-`--limit`, non-`--dry-run`) `videre faces`
invocation, and every `videre watch --faces` cycle, with no progress output of its own
before 2026-07-29. On a real library with tens of thousands of faces this looked
identical to a hang (the detection progress bar clears, then nothing prints until the
whole pass finishes). The average-linkage stage is O(n^2) in the number of faces that
pass the quality gate: it now prints `Clustering N face(s) (eps=X)...` and ticks a
progress bar over the O(n^2) pairwise-distance stage, so a long clustering pass is
visibly progressing rather than silent. The initial candidate-merge heap is now seeded
only with pairs already within `--eps` (not every one of the `n*(n-1)/2` pairs
unconditionally), correctness-preserving, since a pair currently outside `--eps` that
later becomes eligible via a merge is still picked up by the existing distance-update
step, which reads the dense distance matrix directly rather than the heap (see
`one_bad_pair_does_not_block_an_otherwise_strong_merge` in `face_cluster.rs`, which
specifically exercises this). Before this fix, the heap's unconditional
`with_capacity(n*(n-1)/2)` preallocation alone could demand tens of GB on a real
library (~41GB at n=58,555, more than many machines have) before any clustering work
began; it now scales with the number of eps-eligible pairs instead.

**Updated 2026-08-03:** the O(n^2) *memory* fix above only addressed
storage, not the O(n^2) *time* cost of the initial all-pairs scan and the
per-merge distance-update sweep. Both are now computed via an exact
algebraic reformulation (average-linkage cluster distance for L2-normalized
embeddings decomposes as `1 - (sum_A . sum_B)/(|A|*|B|)`, computable from
running per-cluster sums instead of a stored matrix) plus a
`matrixmultiply`-based blocked GEMM used as a fast candidate filter for the
initial scan (every GEMM-flagged pair is re-verified against the exact
scalar distance before being trusted, since GEMM's FMA-based accumulation
doesn't bit-match the scalar path the merge loop's staleness check depends
on). Verified on the real library (58,555 faces, 31,397 clustered into 644
people): 19m27s (old) -> 8m48s (new), ~2.2x wall-clock, with a byte-for-byte
IDENTICAL resulting cluster partition, a pure performance fix, zero change
in output. See
`docs/superpowers/specs/2026-08-03-face-clustering-performance-design.md`
for the full design and why an earlier KD-tree-based proposal was rejected.

`videre report --faces` starts an `axum` web server on `localhost:7878` serving a face-labeling UI:
- **People** (blue), **Unassigned Clusters** (green), **Singletons** (orange) sections, each color-coded consistently across cards, badges, and titles
- Drag a cluster/singleton card's handle onto a person card to assign it, or click "New Person" to create one
- Each unassigned cluster/singleton card links to a detail page (`/cluster/{id}` or via the card thumbnail) showing every face at full size with per-face remove/assign
- "Dissolve cluster" on the cluster detail page ungroups a wrongly-merged cluster back into singletons (faces are not deleted)
- Each person links to `/person/{name}`, listing their confirmed faces with per-face remove
- Click any face thumbnail to open the full-resolution original photo via `/api/original-image/{id}` (a live server request, not a `file://` link, since browsers block navigating from `http://` to `file://` for security)
- Labels are written back to `faces.person_label` and `faces.confirmed`; close the browser tab or press Ctrl-C (or use the "Save & Close" button, which calls `/api/quit`) to stop the server

`videre search --person "Alice"` queries the `faces` table for confirmed rows with the given label and prints the paths of all matching images.

## videre watch

Long-running background process that keeps the pipeline populated so `videre report --show-faces` (or any other reader) always sees fresh data, without anyone manually re-running `videre scan`, `videre faces`, or waiting on lazy HEIC/location conversions. No server, no UI: it loops in the foreground, logging progress to stderr, until killed with Ctrl-C.

```bash
videre watch [directory]                                             # default db; original four stages, every 300s; directory optional when 'path' is set in videre config
videre watch <directory> --scan --faces                              # only these stages
videre watch <directory> --interval 60                                # custom cycle interval (seconds)
videre watch <directory> --silent                                    # suppress per-cycle stderr output
videre watch --db <db> <directory>                        # explicit db instead of the default
videre watch <directory> --prune                                     # opt-in: also reclaim stale rows/cache each cycle
```

Five independent stages, selected with `--scan` / `--faces` / `--heic` / `--location` / `--prune`. If none of `--scan`/`--faces`/`--heic`/`--location` are passed, all four of those run (the common case is "just keep everything up to date", not memorizing four flags). `--prune` is the exception, opt-in only, and never defaults on even when no stage flags are passed at all (added 2026-08-01; kept out of the default set so existing `videre watch` invocations don't change behavior):

- `--scan`: re-runs the same scan/hash/EXIF pipeline as `videre scan`, upserting `file_hashes` for the given directory
- `--faces`: incremental face detection, which queries hashes not yet in the `faces` table, runs detection/embedding/clustering only on those, then re-runs the two-stage clustering (average-linkage + centroid-merge, with the same size + distinctiveness quality gate) over all existing embeddings (same defaults as `videre faces`: `eps` 0.6, `min-cluster-size` 3, `merge-sim` 0.35, `min-face-size` 80, `max-generic-sim` 0.4)
- `--heic`: pre-converts and caches HEIC thumbnails (240px and 1200px) for every HEIC file's content hash, skipping hashes already cached; one full-resolution `qlmanage` conversion per hash, downscaled in memory for each missing size rather than re-converting per size. That same full-resolution decode is also cached as `<hash>_original.jpg` (skipped if already present), and `videre faces` reads this cache instead of running its own `qlmanage` decode when detecting faces on a HEIC file, so running `--heic` ahead of (or alongside) `videre faces`/`--faces` avoids a second full decode per HEIC file. Real measurement: ~108ms to read the cache vs. ~7.6s for a live decode. This full-res cache has a real disk cost at library scale (tens of GB for a HEIC-heavy library) not yet gated behind any size limit or flag.
- `--location`: reverse-geocodes every distinct `(gps_lat, gps_lon)` pair with `location_name IS NULL` and writes the result back to `file_hashes`, the same lookup `--show-faces`'s `/api/location` endpoint performs on demand
- `--prune`: runs the same cleanup as `videre prune` (stale `file_hashes` row removal, `modified_at` sync, orphan embedding/cache cleanup) against the already-open connection, via `PruneArgs::for_watch_stage` and the shared `run_prune` helper, and never deletes real files, only stale db rows and cache entries for files already gone from disk. Its runs are tracked in `pipeline_runs` under `"prune"`, same as a standalone `videre prune` invocation.

`--interval <seconds>` (default 300) is the sleep between cycles; each cycle runs the selected stages once, logs a per-stage summary to stderr (unless `--silent`), then sleeps. There's no daemonization or systemd unit. Run it in a terminal, tmux/screen pane, or your own process supervisor, and stop it with Ctrl-C.

Thumbnails land in `<videre home>/cache/thumbnails/` when `VIDERE_HOME` is set, and `~/.cache/videre/thumbnails/` otherwise (see the thumbnail-cache entry in TECH_DEBT for why that branch still exists and what should replace it). Keyed by content hash rather than file path (`<hash>_240.jpg`, `<hash>_1200.jpg`), mirroring the convention hf-hub already uses for cached model weights under `~/.cache/huggingface/`, and means the same photo scanned into a different database only needs converting once. On first run of any `videre` subcommand, if the pre-rename cache at `~/.cache/dupe/thumbnails/` still exists and `~/.cache/videre/thumbnails/` doesn't, it's migrated automatically (a plain directory rename, atomic on the same filesystem, and a no-op on any error since the cache regenerates lazily). `videre report`'s `/api/raw?path=...&size=N` endpoint (server mode, `--show-faces`) checks this cache first for HEIC requests and serves the cached JPEG directly if present, falling back to a live `qlmanage` conversion otherwise, so running `videre watch --heic` alongside `videre report --show-faces` eliminates the per-request HEIC conversion cost for anything already warmed.

`videre watch` and `videre report --show-faces` are designed to run concurrently against the same SQLite file (see the WAL-mode note in the SQLite schema section above).

## videre mcp

Serves three read-only tools over stdio (line-delimited JSON-RPC, the standard MCP client transport) using the official `rmcp` SDK: `search` (text/person/image, a subset of `videre search`'s modes; `--category` is CLI-only, not exposed here), `find_duplicates` (keep/remove groups, plus review-only similar clusters via `include_similar`), and `stats` (library summary, no params).

```bash
videre mcp                # default db
videre mcp --db <path>    # explicit db
```

Database resolution is identical to every other reader (`--db` > `default_db` in `config.toml` > `~/.videre/hashes.db`), but `mcp` binds the resolved path once at startup for the life of the process rather than per-invocation, so the resolved db must already exist. Even an explicit `--db` to a nonexistent path fails at startup with `no database found at <path>; run 'videre scan <dir>' first` on stderr, nothing on stdout, exit 1.

Once serving, a failing tool call returns `isError: true` with the rendered anyhow error chain as the result text; the server itself stays alive and keeps serving subsequent calls. All three tools' result documents share `"schema_version": 1` with the CLI's `--json` output and reuse the same shapes (`duplicate_groups`/`keep`/`remove`, `similar_groups`, `results` with `hash`/`score`, omitted for person hits).

The SigLIP embedding model loads lazily on the first text/image search and stays cached in server memory for the life of the process, unlike the CLI which reloads it per invocation. Person search never touches the model.

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

## videre stats

Prints library totals and per-command pipeline run status in one shot, a CLI
window into `videre-core`'s `library_stats` and `pipeline_runs` modules.

```bash
videre stats                # default db
videre stats --db <path>    # explicit db
videre stats --json         # single JSON object instead of text
videre stats --check        # exit nonzero if any tracked command last failed or crashed
```

Text mode prints library totals (files/size, photo/video split, duplicate
groups/files/wasted space, faces detected/people named), then one line per
embedding model present for this library (id, row count, dimensions, file
size, with dimensions derived from the stored blob length rather than a
hardcoded table so an unfamiliar model still reports honestly), then one line
per tracked command (`scan`, `faces`, `embed`, `classify`, `dedupe`, `fix-dates`,
`prune`, `locations`) showing its last-run timestamp, status, and duration. `never run` /
`-` marks a command that hasn't executed against this db yet, and `(running
now)` appended when its lock is currently held by a live process. Uses
`resolve_reader_db_must_exist` like `dedupe`/`mcp` (not `resolve_reader_db`
like `embed`/`classify`), so an explicit `--db` to a nonexistent path fails
cleanly rather than silently creating an empty database.

`--json` emits `{"schema_version": 1, "library": {...}, "pipelines": [...]}`,
directly reusing `videre-core`'s `LibraryStats` and `PipelineRunStatus` serde
types rather than redeclaring their fields. `library.embeddings` is an array
with one entry per model; it is additive, so `schema_version` stays 1. The `pipelines` array always has
exactly eight entries (`videre_core::pipeline_runs::TRACKED_COMMANDS`) in a
fixed command order, with `status`/`last_run_at`/`duration_ms` all `null` for
a command that has never run. `report`, `search`, `mcp`, and `config` are
deliberately not tracked here; see `TRACKED_COMMANDS`'s doc comment for why
each was left out.

`--check` (added 2026-08-01) doesn't change either output format. It only
adds an exit code, via `has_problem()` checking whether any tracked command's
last recorded status is `"failed"` or `"crashed"` (a clean `"interrupted"`
Ctrl-C is deliberately not treated as a problem). Composes with both text and
`--json` mode, so `videre stats --check` (or `--json --check`) can drive
cron/launchd failure handling without parsing either output.

Per-item errors within a run (a few unreadable files, one corrupted image) do
not mark a `pipeline_runs` row `failed`. Only an unhandled exception during
the run does. `fix-dates`/`faces` can legitimately exit nonzero (bad EXIF
dates, detection failures) while still recording `status: "success"`, since
`track()` only observes the operation's returned `Result`, and both commands
return `Ok` with an error count rather than propagating those as `Err`.

## UI

The user-facing UI in this repository is entirely `videre report`'s HTML
output; see the `videre report` section above for full detail. Two forms:
static HTML (`videre report`, `--all`, `--by-date`) that can be opened
directly as a `file://` page, and a live local server mode (`--faces`,
`--show-faces`) for the parts that need a backend, in particular the face
labeling UI (assign/rename/dissolve clusters, tag people) served on
`localhost:7878`.
