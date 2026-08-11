# videre

A fast Rust CLI for managing a local media library: duplicate detection,
semantic search, and face recognition, all around a single SQLite database.

**User-facing documentation lives at <https://docs.videre.sh>, generated from
`docs/` in this repo.** This file is for working *on* videre: build and test
invariants, measured findings, and traps that are easy to reintroduce. It is
deliberately not a command reference. When you change behaviour, update the
relevant page under `docs/src/content/docs/` in the same commit.

## Build & run

```bash
cargo build --release
make fmt-check                 # what CI enforces
cargo test --workspace
```

One binary, `videre`, with fourteen subcommands. `main.rs` dispatches to one
module per subcommand under `src/commands/`.

## Project structure

```
crates/
  videre/          bin + lib (scanner, hasher, output, sqlite_output, types)
    src/commands/  one module per subcommand
    tests/         integration tests, spawn the binary; tests/common/ is shared
  videre-core/     shared db/cache/search helpers, used by videre and videre-ml
  videre-ml/       lib-only: all inference (SigLIP, ArcFace/SCRFD, preprocessing)
  videre-api/      lib-only: facade over face-labeling ops for the axum server
docs/              the Astro Starlight site published at docs.videre.sh
```

`videre-ml` and `videre-api` have no binaries. Every user-facing entry point is
a subcommand in `videre`.

Anything needed by two or more subcommands belongs in `videre-core`. Check there
for existing or adjacent helpers before adding to a command module.

## Key crates

`clap` (derive subcommands), `blake3`, `rayon`, `walkdir`, `rusqlite` (bundled),
`image` (decoding + inline dHash), `kamadak-exif`, `filetime`, `chrono`,
`candle-core`/`candle-nn`/`candle-transformers` (SigLIP, Metal on macOS),
`tokenizers`, `hf-hub`, `half`, `matrixmultiply` (GEMM candidate filter in face
clustering), `ort` (ONNX Runtime for face models), `axum` + `tokio` (labeling
server), `rmcp` + `schemars` (MCP), `ureq` (forward geocoding, the only network
call).

Face models are InsightFace buffalo_l, fetched from `WePrompt/buffalo_l` into
`~/.cache/huggingface/hub/` honouring `HF_HOME`. **Not** `~/.cache/ort/`, which
this file claimed for months and which has never existed.

## Platform support

Only the build-affecting parts are here; the user-facing matrix is at
<https://docs.videre.sh/reference/platforms/>.

**Intel macOS (`x86_64-apple-darwin`) does not build at all.** `ort-sys`
2.0.0-rc.13 ships no prebuilt ONNX Runtime for that target, so any build fails
with `no prebuilt binaries available for target x86_64-apple-darwin`. Not a CI
or cross-compilation problem: `cargo install videre` fails identically on an
Intel Mac. Found 2026-08-11. Revisit if `ort` ships Intel macOS binaries.

**ARM64 Linux needs FP16 enabled explicitly.** `gemm-f16` (via `candle-core`)
emits FP16 instructions outside the baseline `aarch64-unknown-linux-gnu` feature
set, failing with 11x `error: instruction requires: fullfp16`.
`.cargo/config.toml` sets `-C target-feature=+fp16` for that target, covering
every cargo invocation from inside this repo. It deliberately cannot help
`cargo install videre` from crates.io, which reads the installing user's own
config, so the flag is documented in the install docs.

:warning: **`cargo check --workspace` PASSES on ARM64 Linux even when
`cargo build` fails**, because `check` never runs codegen. Never treat a green
cross-platform `check` as evidence that `cargo install` works.

HEIC and video decoding go through macOS QuickLook (`qlmanage`). Both entry
points (`videre_core::heic::heic_via_quicklook`,
`videre_ml::preprocess::decode_via_quicklook`) short-circuit on a
`cfg!(target_os = "macos")` check and surface
`videre_core::heic::QUICKLOOK_UNAVAILABLE`, printed at most once per process.
The guards use `cfg!()` (a runtime-constant `if`) rather than `#[cfg]` so both
branches type-check on every platform.

## CI

`.github/workflows/ci.yml` runs `make fmt-check` plus the full suite on
`ubuntu-latest` and `macos-latest`, with `fail-fast: false` so one runner's
failure never hides the other's. Only four tests are macOS-gated.

:warning: **`cargo fmt` has no per-file mode.** Arguments after `--` are rustfmt
options, not a file filter, so `cargo fmt -p videre -- path/to/one.rs` silently
formats the entire package. To format one file, invoke `rustfmt <file>`
directly. The workspace was reformatted to zero drift on 2026-08-09 and the
`fmt` job keeps it there; before that, a stray `cargo fmt` swept 20 unrelated
`src/` files into a test-only commit and nothing caught it, because formatting
changes are invisible to the test suite.

**Tests never download model weights.** That is the application's job. Three
tests need weights (`faces_resumability`, plus `embed.rs`'s two on macOS); each
calls `common::skip_without_models` and returns early on a cold cache.
`faces_pipeline` is deliberately *not* gated, because `commands/faces.rs`
returns at the `to_process.is_empty()` branch before loading anything; a
regression guard runs the binary against a fresh `HF_HOME` and asserts nothing
was downloaded, so moving that model load earlier fails loudly instead of
quietly adding a 200MB download to every cold CI run.

Rust has no native skip, so a skipped test passes. Two things stop that becoming
silent coverage loss. The skip message writes to fd 2 directly rather than via
`eprintln!`, because libtest captures the print macros for passing tests and the
message would otherwise appear only under `--nocapture`. And
`VIDERE_TEST_REQUIRE_MODELS=1` turns a cold-cache skip into a panic; CI sets it
after restoring its cache, so a cache that silently stops working fails the
build rather than disabling those tests.

CI caches `~/.cache/huggingface` keyed on `face_models.rs` and `embeddings.rs`,
so changing the model invalidates it rather than reusing weights for a different
one.

Clippy is not in CI yet: it reports 18 warnings, so a lint job would need
`--allow`-ing them or a cleanup pass first.

## Testing conventions

`crates/videre/tests/common/mod.rs` is shared by every integration-test file.

**Every test that touches a lock must point `VIDERE_HOME` at a temp directory.**
Lock paths resolve through `VIDERE_HOME` rather than sitting beside the
database, so without this a test writes into the developer's real
`~/.videre/locks` and leaves permanent litter. `videre_bin()` calls
`isolated_home()`, which sets the env var once per test binary inside a
`OnceLock` (tests share a process and run in parallel, so a per-test `set_var`
would race every concurrent `getenv`); spawned children inherit it. Calling it
from `videre_bin` rather than from each test is what makes it impossible for a
new file to forget, which is how `faces_pipeline.rs` and `person_search.rs` once
came to be missing it.

**Every test spawning `videre embed` or `videre faces` must hold
`shared_cache_guard()`.** Those children resolve weights through the same
Hugging Face cache, which is not safe for two simultaneous first-time readers.
It has to be a *file* lock: cargo runs each test file as its own process in
parallel, so the racing readers are usually in different processes, which no
`Mutex` can see. Only contended on a cold cache.

`stderr_without_library_noise` filters library chatter before asserting on
stderr. ONNX Runtime initialises at startup even for subcommands that never
infer, and on a host whose CPU it cannot identify it prints
`onnxruntime cpuid_info warning: Unknown CPU vendor` before `main` runs, which
broke a `--silent` assertion on ARM64 Linux.

A test that makes a file unreadable with `chmod 000` is meaningless as root, the
default in a stock Docker image, so it probes whether permissions are enforced
and skips when they are not.

## Test coverage

`cargo-llvm-cov` must be invoked through the rustup-managed toolchain
explicitly, not plain `cargo llvm-cov`. This machine's default `cargo`/`rustc`
on `PATH` are a separate Homebrew Rust install with no rustup component support,
while `llvm-tools-preview` only installs into a rustup toolchain; mixing them
pairs an LLVM-22 rustc with LLVM-21 coverage tools and produces incompatible
profile data.

```bash
rustup run stable-aarch64-apple-darwin cargo llvm-cov --workspace --summary-only
```

Read the per-file table as **unit-test coverage only**. Integration tests that
spawn `videre_bin()` as a child process are not instrumented, so command modules
show artificially low numbers despite being well covered.

## Invariants and measured findings

The expensive-to-rediscover things. Each was measured, not assumed.

### `videre embed --batch` silently corrupts above ~121

`videre_ml::model::MAX_SAFE_BATCH` is 96. Above a threshold measured between 121
and 127, the batched inference path returns embeddings that do not match a
one-at-a-time baseline: no error, no NaN, just wrong vectors, with only the
trailing partial batch correct. Values over 96 are reduced with a warning.

**Checking output for zero or NaN vectors does NOT detect this**, since
`--batch 256` produces neither and is still fully corrupt. Do not raise the
constant without re-running the ignored `batched_embeddings_match_one_at_a_time`
test in `videre-ml`. Root cause is candle's Metal backend, proven against MLX
and PyTorch on the same GPU; larger batches buy nothing anyway (31.0 ms/img at
96 against 39.1 at 768).

### Embeddings live in per-library, per-model databases

`~/.videre/embeddings/<db stem>-<hash16>/<owner>--<model>.db`, attached to the
main connection under the alias `emb`. `videre_core::embeddings_db` is the only
module that knows this layout.

- **`sqlite_master` is per database.** A probe for the table must say
  `emb.sqlite_master`; the unqualified form returns 0, and every caller reads 0
  as "nothing embedded yet", so getting this wrong makes search return no
  results and report success.
- **No atomic commit across attached databases in WAL mode.** No transaction may
  write to both `main` and `emb` and depend on both landing. Nothing needs that
  today; do not merge the two for tidiness.
- Per library rather than one global file per model, because `videre prune`
  cannot see another library's `file_hashes`, so a global layout would let one
  library's orphan sweep delete vectors another still needs. An embedding costs
  hours; a thumbnail costs milliseconds, which is why the thumbnail cache can
  tolerate the same flaw.
- Created with `page_size = 16384`, set on the empty file before WAL and before
  any table exists, since SQLite silently ignores it afterwards and needs a full
  `VACUUM` to apply.

### Locks are keyed by a hash of the canonical database path

`<videre home>/locks/<db stem>-<hash of canonical path>.<command>.lock`. The
hash is load-bearing: two libraries can both be named `photos.db` in different
directories, and keying on the stem alone would make them share a lock, silently
serializing unrelated libraries. Canonicalizing first also collapses a symlink
and a relative path to one lock.

### WAL everywhere

Every subcommand opens through `videre_core::db::open_wal`. WAL persists in the
file once set, so it is idempotent and safe on every open. This is what lets
`videre watch` write while a `report --show-faces` server reads.

### Resumability uses "already processed", not "has a result"

Two tables exist purely for this, and both record work that produced no rows:

- `faces_scanned` records every hash face detection processed, **including
  images where zero faces were found**. Without it, every landscape photo is
  re-detected on every run.
- An unrecognised file records `mime = 'application/octet-stream'` rather than
  NULL, so `mime IS NULL` means only "never scanned". `scan --retry-incomplete`
  uses that. `effective_mime` treats the sentinel exactly as NULL and falls back
  to the extension, so a merely-unidentified file is still processed.

Both encode the same lesson: the skip set has to be "already tried", or work
that legitimately produces nothing repeats forever.

### DNG must be vetoed explicitly

`.dng` reports `image/tiff`, because DNG genuinely is a TIFF variant. TIFF is
embeddable and DNG is not decodable, so `ext = 'dng'` explicitly vetoes
embeddability. Routing on mime alone revives the bug fixed 2026-08-01, where
every DNG was queried as pending and failed to decode on every single run.

### Probe videos before invoking QuickLook

`qlmanage -t` does not fail on a container with no video track, it **hangs**, so
videre paid the full 20s `QLMANAGE_TIMEOUT` per such file on every run.
`videre_core::video_probe` walks ISO-BMFF boxes for a `vide` handler and fails
open, so any parse error proceeds to QuickLook as before. Measured on a real
70,601-file library: three audio-only Live Photo companions cost 60s per `embed`
run and another 60s per `scan --similar`.

### qlmanage concurrency is capped per process, and that is not enough

All `qlmanage` launches share one process-wide semaphore
(`videre_core::heic::qlmanage_semaphore`), 6 by default. Raising it from 3 to 6
gave a further ~1.23x on top of a ~3.23x from the worker pool, for ~4.48x over
the serial baseline. Beyond 6 the gains are 1.3-4.4% and per-image detect time
creeps up, so the bottleneck shifts into CPU contention.

**The cap is per-process, so two videre commands at once permit 12 against one
shared QuickLook agent.** Measured with `faces` and `embed` running together:
HEIC load averaged 16,339ms against ~7.6s uncontended, and one file blew past
the 20s timeout that converted in 0.39s standalone. Impact is bounded, since the
skipped file is correctly not marked scanned and self-heals next run, but the
intended `watch` + manual-command workflow makes overlap normal.

### Face clustering is O(n^2) in both memory and time, and both were fixed

The heap's unconditional `with_capacity(n*(n-1)/2)` preallocation demanded ~41GB
at n=58,555 before any work began; it is now seeded only with pairs already
within `--eps`. Correctness-preserving, because a pair that later becomes
eligible via a merge is still picked up by the distance-update step, which reads
the dense matrix directly rather than the heap. See
`one_bad_pair_does_not_block_an_otherwise_strong_merge` in `face_cluster.rs`.

Time was then fixed by an exact algebraic reformulation (average-linkage
distance for L2-normalized embeddings decomposes as
`1 - (sum_A . sum_B)/(|A|*|B|)`) plus a `matrixmultiply` GEMM candidate filter.
Every GEMM-flagged pair is re-verified against the exact scalar distance, since
GEMM's FMA accumulation does not bit-match the scalar path the staleness check
depends on. Verified on 58,555 real faces: 19m27s to 8m48s with a
**byte-for-byte identical** cluster partition.

### `watch --prune` can override neither prune guard

`PruneArgs::for_watch_stage` pins both the bulk-deletion and repeated-failure
guards to false, and a unit test asserts it. It runs unattended and cannot ask.

### `videre locations` is a full recompute that is not cheap

The clustering maths is sub-second, but the per-coordinate `file_hashes` UPDATE
(matched via the unindexable `ROUND(gps_lat, 6) = ROUND(?, 6)` against the whole
table) measured ~8 minutes on a 70k-file library. The whole recompute runs in
one transaction, so it holds the single WAL writer lock for that entire window
and a concurrent `watch` write blocks.

### `pipeline_runs` tracks exactly 8 commands

`videre_core::pipeline_runs::TRACKED_COMMANDS`. `status` is stored as
`running`/`success`/`failed`/`interrupted`; `crashed` is **never written**, only
computed at read time when a `running` row's lock is not held by a live process.
Per-item errors do not mark a run failed, so `fix-dates`/`faces` can exit
nonzero while recording success.

### HEIC conversion uses `qlmanage`, never `sips`

Some HEIC files (iPhone photos where rotation is encoded via the HEIF `irot`
box rather than an EXIF Orientation tag) come out sideways with `sips`, which
copies raw sensor-buffer pixels unrotated. This affects face detection,
embedding preprocessing, and every thumbnail path.

## Release and publishing

`.github/workflows/release.yml` runs on a `v*` tag: **create draft -> build ->
smoke -> publish -> tap**. The ordering is load-bearing.
`taiki-e/upload-rust-binary-action` uploads to an *existing* release and a tag
does not create one, so the draft comes first. The smoke job downloads each
archive onto a runner that never compiled it and runs `--version`, a real
`scan`, and `stats`. A build or smoke failure leaves the release a draft, so
nobody is offered a download that was never executed. That fired for real on
2026-08-11 when the Intel macOS build failed.

Both jobs set `timeout-minutes`. The default is six hours, which is how a job
queued against a retired `macos-13` runner ran for ten hours without executing a
single step. A hang is worse than a failure because nothing tells you.

Three targets: `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, at 21-24MB compressed from a 62MB binary. ONNX
Runtime is statically linked and is most of that size.

crates.io has no namespaces, so the `videre-` prefix reserves nothing. Publish
order follows the dependency graph: `videre-core` -> `videre-api` + `videre-ml`
-> `videre`. `cargo publish --workspace` does this automatically and verifies
each crate from its own packaged tarball first.

`crates/videre/Cargo.toml` excludes `tests/fixtures/*`: ~2.2MB of sample media
nothing needs at build or run time, which would more than quadruple the download
for `cargo install videre`.

## The docs site

`docs/` is an Astro Starlight site published at <https://docs.videre.sh>,
deployed automatically from `main` by Cloudflare.

```bash
yarn --cwd docs install
yarn --cwd docs dev       # http://localhost:4321
yarn --cwd docs build
```

Yarn 4 with `nodeLinker: node-modules`. **PnP does not work here**: Astro
resolves virtual module specifiers such as `astro:toolbar:internal`, which are
not real packages, and PnP rejects them as unsound. Node is pinned by
`docs/.node-version`, since Astro 7 requires >= 22.12.0.

:warning: **An unknown Starlight icon name renders an empty `<svg>` rather than
failing the build.** Validate icon names against the installed package, and
check for path content, not for the element's presence.

See `docs/README.md` for layout and the content split between README, the site,
and this file.
