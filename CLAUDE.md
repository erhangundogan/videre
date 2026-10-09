# videre

A fast Rust CLI for managing a local media library: duplicate detection,
semantic search, and face recognition, all around a single SQLite database.

**User-facing documentation lives at <https://docs.videre.sh>, generated from
`docs/` in this repo.** This file is for working *on* videre, in two parts kept
deliberately separate: the **working methodology** just below (how to work
here), then the **measured facts** (build and test invariants, findings, and
traps easy to reintroduce). It is deliberately not a command reference. When you
change behaviour, update the relevant page under `docs/src/content/docs/` in the
same commit.

## How to work on videre

Methodology, kept separate from the measured facts that follow. These are
working practices, not project truths.

**Discovery before spec.** For any change touching shared logic, several
subcommands, cross-cutting arguments or common behaviour, search code, tests,
git history and docs exhaustively first. Ask rather than guess.

**Shared code first.** The same thing implemented twice is the worst outcome; the
second caller triggers extraction and refactoring of the first. Where it lives is
the `videre-core`-versus-shared-module rule under Project structure.

**Test first when fixing a bug.** Write it, run it, watch it fail. A test that has
never failed has not been shown to test anything.

**Find the cause before mitigating.** `cargo test` has four target kinds
(`--lib`, `--bins`, `--test`, `--doc`), so `tests/` is not the set of tests.

**Verify before claiming done.** Run the command and read its real output; if
tests fail, say so with the output.

**Challenge inherited constraints.** The model, the schema and the stored data
are variables. Ask what recompute cost is acceptable before optimising within
assumed limits.

**Read the docs before advising.** Grep videre's own docs for a flag before
recommending a value.

**New settings go in `config.toml` and `videre config set`,** never a new
environment variable. Do not design for backwards compatibility; users install
latest.

**Fixtures must look like the real library.** It is Turkish, and many names are
non-ASCII; `Alice`/`Bob` cannot expose an ASCII-only SQLite `LOWER()`, and did
not.

**Getting a change in.** Read `git status --short` before staging and stage by
name, never blanket-stage. `main` is branch-protected: PR with green checks,
merge commits not squash so the reasoning survives. PR descriptions stay short;
the reasoning lives in commit messages and this file.

A feature or fix PR does **not** bump the version and does **not** touch the
changelog. Releases are batched and **written at release**: a separate release
PR bumps the version in all four crates and stages `Cargo.lock` in one commit
(below 1.0 the minor number is the compatibility boundary), and writes that
version's `CHANGELOG.md` section, composed from the PRs merged since the last
tag (`git log <last-tag>..main`, `--first-parent` for just the merges). So a
`[Unreleased]` section normally sits empty between releases; every version
heading still needs its compare link at the foot of the file or
`tests/changelog.rs` fails. See "Release and publishing" below for the
publish mechanics.

## Build & run

```bash
cargo build --release
make fmt                       # not bare `cargo fmt`; see below
cargo test --workspace
```

One binary, `videre`, with twenty-one subcommands. `main.rs` dispatches to one
module per subcommand under `src/commands/`.

### The Rust version is pinned, in one place

`rust-toolchain.toml` decides the version, for this repo and for CI. **Change it
there and nowhere else.** CI installs exactly that toolchain rather than using
whatever the runner ships.

Locally it works because `cargo` is a **rustup shim**, which reads the file and
dispatches to the right toolchain. Confirm with `cargo --version` inside the
repo against `cargo --version` somewhere else.

:warning: **The shims are not in `~/.cargo/bin`.** rustup came from Homebrew,
which puts them in `/opt/homebrew/opt/rustup/bin` and links nothing into
`/opt/homebrew/bin`. `~/.cargo/bin` holds only cargo *extensions*
(`cargo-llvm-cov`, `cargo-audit`, `cargo-watch`), which still work because
`cargo foo` dispatches to `cargo-foo` on `PATH`. Looking for a `cargo` shim in
`~/.cargo/bin` and concluding rustup is broken is the wrong conclusion.

:warning: **The Makefile says `cargo +$(TOOLCHAIN)`, not bare `cargo`.** The
channel is read out of `rust-toolchain.toml`, so the version is still written
once. The `+` makes it explicit, so a non-shim `cargo` fails loudly rather than
silently building with the wrong compiler.

:warning: **Do not "fix" that to `rustup run <toolchain> cargo`.** It looks
equivalent and is not: `rustup run` execs the right cargo but does **not** put
the toolchain's bin on `PATH`, so cargo subcommands are not found and
`rustup run 1.96.0 cargo fmt` dies with `no such command: fmt`. The history is
in `rust-toolchain.toml`'s own comment.

:warning: **Format with `make fmt` and verify with `make fmt-check`, not bare
`cargo fmt`.**

`cargo fmt` visits only the `.rs` files reachable through `mod` from a target
root, so it silently skips any file not wired into the module tree and still
exits 0. That is a real false pass, reproduced directly: a misformatted `src/`
module that nothing `mod`-declares passes `cargo fmt --all -- --check` cleanly.
The long-repeated mitigation "just run `cargo fmt --all`, it cannot report a
false pass" was wrong for exactly this reason: it does not format an unreachable
file either, it just never looks at it.

`make fmt` and `make fmt-check` enumerate the sources with `find crates` and hand
each file to rustfmt directly, so every file under `crates/` is checked
whatever the module tree or cargo's target discovery says. Never wrap the check
in something that asserts success from an exit code alone.

:information_source: `make fmt-check` runs `rustup run $(TOOLCHAIN) rustfmt`.
That is safe where `rustup run <toolchain> cargo fmt` is not (the warning above):
`rustfmt` is a first-class binary in the toolchain's bin, so `rustup run` finds
it, whereas a cargo *subcommand* is not on that PATH.

Coverage is the one command that still names a toolchain of its own; see the
coverage section for why.

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

Anything needed by two or more subcommands belongs somewhere shared. Check for an
existing or adjacent helper before adding to a command module.

:warning: **Shared does not automatically mean `videre-core`.** It is the root of
the dependency graph: `videre-api` and `videre-ml` both depend on it, and all
four crates are published. So a dependency added there is compiled by the
inference crate too, and anything public becomes API that a version bump has to
respect.

| the thing is | put it in |
|---|---|
| needed by another **crate** | `videre-core` |
| needed by several **subcommands**, and nothing outside the binary | a shared module under `crates/videre/src/` |

The second case keeps heavy dependencies out of crates that have no use for
them, and leaves the type free to change shape without a semver event. Promoting
a module to a crate later is easy; demoting a published crate is not.

## Key crates

`clap` (derive subcommands), `blake3`, `rayon`, `walkdir`, `rusqlite` (bundled),
`image` (decoding + inline dHash), `kamadak-exif`, `filetime`, `chrono`,
`candle-core`/`candle-nn`/`candle-transformers` (SigLIP, Metal on macOS),
`tokenizers`, `hf-hub`, `half`, `matrixmultiply` (GEMM candidate filter in face
clustering), `ort` (ONNX Runtime for face models), `axum` + `tokio` (labeling
server), `askama` (gallery templates), `rmcp` + `schemars` (MCP),
`reverse_geocoder` (offline place names from a bundled GeoNames extract),
`tracing` (logging), `ureq` (forward geocoding, the only network call).

Face models are InsightFace buffalo_l, fetched from `WePrompt/buffalo_l` into
the Hugging Face hub cache (normally `~/.cache/huggingface/hub/`, with
`HF_HUB_CACHE`, `HUGGINGFACE_HUB_CACHE`, `HF_HOME`, and `XDG_CACHE_HOME`
overrides). **Not** `~/.cache/ort/`, which this file claimed for months and
which has never existed.

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
failure never hides the other's, and `cargo test --no-fail-fast` so one failing
test *binary* never hides the later ones. Both were learned the same way: a
Linux-only failure in `videre-core`'s lib tests stopped the run before the
`videre` integration tests, whose Linux result was then unknown rather than
green. Some tests are gated on `cfg(target_os = "macos")`, so a green Linux
run says nothing about them.

:warning: **`cargo fmt` has no per-file mode.** Arguments after `--` are rustfmt
options, not a file filter, so `cargo fmt -p videre -- path/to/one.rs` silently
formats the entire package. To format one file, invoke `rustfmt <file>`
directly. The workspace was reformatted to zero drift on 2026-08-09 and the
`fmt` job keeps it there; before that, a stray `cargo fmt` swept 20 unrelated
`src/` files into a test-only commit and nothing caught it, because formatting
changes are invisible to the test suite.

**Normal tests and CI are model-free.** `VIDERE_TEST_MODELS` unset or `0`
visibly skips real inference even when a developer's cache is warm. Run
`VIDERE_TEST_MODELS=1 make test` locally to exercise the model-backed tests;
missing weights may download into the developer's Hugging Face hub cache.
Download, load, inference, and assertion failures then fail the suite. CI
neither restores nor warms model weights, and points Hub requests at an
unreachable local endpoint so an ungated fetch fails instead of downloading.
CI's green result does not prove real inference works.

Rust has no native skip, so the gate writes its reason directly to fd 2, where
it remains visible under libtest capture. An invalid `VIDERE_TEST_MODELS` value
is an error, not an implicit skip or opt-in. `faces_pipeline` remains in the
normal suite because its empty-work path returns before loading a model;
its regression guard asserts that no weights appeared in the private cache.

Each real-model test holds one cross-process cache lock through model use.
`TestLibrary::model_cmd(&guard)` passes only the resolved hub-cache path to a
model-backed child; normal `cmd()` keeps the child cache private. The cache path
matches the pinned loader's precedence: `HF_HUB_CACHE`, then
`HUGGINGFACE_HUB_CACHE`, `HF_HOME/hub`, `XDG_CACHE_HOME/huggingface/hub`, then
`HOME/.cache/huggingface/hub`. This allows one local download while preventing
another test process from reading a half-written file.

:warning: **A model-free test must remain model-free even during opt-in.** The
`.dng` fixture in `argument_robustness.rs` is scanned but vetoed as
non-embeddable, so the command returns before loading weights. The test's
private `HF_HOME` and zero-byte assertion guard that path. This fixture fixed
the historical CI download that pushed Ubuntu past 35 minutes; the normal
suite no longer wakes other model tests merely by warming a cache.

:warning: **Do not set `[profile.dev.package."*"] opt-level = 3` to speed up the
test build.** It was tried and reverted: release-grade codegen in every test
build made Ubuntu's Build step ~4x worse overall, and deleting the cached model
that caused the slow test beat optimizing the work. The measurements and the
`do not add this back` reasoning live in the root `Cargo.toml` comment.

Clippy is gated in CI, on its own `clippy` job that runs
`cargo clippy --workspace --all-targets -- -D warnings` (`make lint-check`;
`make lint` is the same without `-D warnings`, for listing while you work). The
job installs the pinned toolchain the same way the others do. A warning now
fails the build, so the lint count cannot climb: every lint is either
fixed or given a justified `#[allow]` with a reason at its site. Two lints are
allowed workspace-wide in the root `Cargo.toml` because they are subjective
structural lints, not correctness ones (`type_complexity`,
`too_many_arguments`); that manifest's comment says why.

The job lints the **Linux** configuration, and that asymmetry is load-bearing.
videre has real platform-gated code: the `cfg(target_os = "macos")` branches
(HEIC and video via QuickLook, the Metal test probes) are compiled out on Linux,
and the Linux/CPU branches are compiled out on macOS. So a green `make
lint-check` on a macOS dev machine does not prove the CI gate passes: a helper,
import, or variable used only by macOS-gated code is live on macOS but dead on
Linux, and only the Linux build flags it. That is exactly what the gate caught
when it first landed (dead helpers in `videre-ml`'s model/pipeline tests, plus
two gallery/embed test helpers), none of which a macOS run reports.

`make lint-linux` runs the identical gate against the Linux target inside a
`rust:<pinned>` Docker container (mirrored from CI), so those lints surface in
seconds instead of one CI round-trip at a time: `-D warnings` stops at the first
error per target, so a push-and-wait loop reveals them one by one, while a plain
`cargo clippy` (no `-D`) run in the same container lists them all at once. Reach
for `make lint-linux` whenever a change touches `cfg(target_os = ...)` code or
the helpers and tests around it. The reverse gap, a macOS-only lint, is covered
by running `make lint` on the dev machine.

## Testing conventions

`crates/videre/tests/common/mod.rs` is shared by every integration-test file.

**Isolation is per-library, not per-home.** videre selects its library from
`--library` or the invocation directory, and locks live in the library's own
`.videre/locks`; nothing reads a global home variable, so setting one is a
silent no-op. `TestLibrary` gives each spawned child its own library root,
`HOME` and `HF_HOME`, and strips ambient environment so what a child sees is
exactly what was configured. Writing a test against a copy of a real library
by path requires re-scanning the copy: a database whose rows point outside
the library root is refused by the library guard.

**Every test that can load a real model must hold the shared `ModelCacheGuard`
through use.** A model-backed child must use `model_cmd(&guard)`; model-free
children stay on `cmd()`. It has to be a *file* lock: cargo runs test files in
parallel processes, which no `Mutex` can coordinate. The conservative guard
serializes model tests even on a warm cache.

`stderr_without_library_noise` filters library chatter before asserting on
stderr. ONNX Runtime initialises at startup even for subcommands that never
infer, and on a host whose CPU it cannot identify it prints
`onnxruntime cpuid_info warning: Unknown CPU vendor` before `main` runs, which
broke a `--silent` assertion on ARM64 Linux.

A test that makes a file unreadable with `chmod 000` is meaningless as root, the
default in a stock Docker image, so it probes whether permissions are enforced
and skips when they are not.

## Test coverage

`make coverage` (or `make coverage-html`) runs `cargo-llvm-cov` on the pinned
toolchain, which needs `llvm-tools-preview` installed into it. A different
toolchain pairs mismatched LLVM versions and produces incompatible profile
data; the Makefile comment has the details.

```bash
rustup component add llvm-tools-preview --toolchain <channel in rust-toolchain.toml>
make coverage
```

Integration tests that spawn `videre_bin()` **are** counted: cargo-llvm-cov
instruments the child, which writes its profile when it exits. So a child must
exit, not be killed. :warning: SIGKILL skips that write, and when the gallery
tests stopped their server with `kill()`, `gallery/server.rs` read 77% although
the tests request nearly every route; stopping it through `/api/quit`
(`common::stop_gallery`) read 89%. The Playwright suite (`e2e/`) is not part
of `make coverage`.

## Invariants and measured findings

The expensive-to-rediscover things. Each was measured, not assumed.

### File identity ignores metadata

`file_hashes.hash` is the content key: BLAKE3 over the file with its
metadata blocks left out (`videre::content_key`), so an EXIF rotate or a date
fix in another app keeps a photo's faces, names, marks and tags attached.
`meta_hash` covers the metadata alone. A format the splitter cannot parse, or
a file with no image or media data in it, falls back to the whole-file hash;
without that, files of metadata alone would share one key and dedupe would
offer to delete them. Libraries built before this (schema below 3) are
refused and rebuilt by a fresh scan; there is no migration.

Schema 4 is the one exception to "no migration": it makes `faces.id`
`AUTOINCREMENT`, so a face ID is never reused. The learning journal and
identity questions keep face IDs as history with no foreign key, and a reused
ID would silently point that history at a different face. A writable open
upgrades schema 3 once (`library_db::upgrade_v3_to_v4`), rebuilding `faces`
with every ID and reserving the highest ID the journal or questions mention;
a read-only open refuses. Prune removes faces whose content has no indexed
path and marks journal events that lost a source face ineligible
(`face_learning::prune`), keeping the journal itself.

:warning: **Changing the splitter's spans changes stored keys.** Any change to
which bytes `content_key` counts as content, including teaching it a format
that used to fall back, gives the affected files new keys the next time they
are hashed, and orphans their faces, names, tags and embeddings. Treat it like
a schema version bump, and re-run the real-tool fixtures in
`tests/content_key.rs`. Files on the fallback today are the rows with
`meta_hash IS NULL`.

**A hash change is a breaking release.** The switch to the content key shipped
in 0.41.0 as schema 3, with a "Breaking changes" changelog section: older
libraries are refused, versions cannot share a library, and hashes saved from
`--json`, JSONL or gallery URLs stop matching. Any later change to what `hash`
covers needs the same schema bump and the same changelog section.

### Every filter goes through `videre_core::selection`

One layer, two shapes. `RowSelection` filters rows that exist in the database
and resolves to a hash set; `PathSelection` filters a filesystem walk and is
pure. A command declares its vocabulary by flattening only the clap groups in
`commands/selection_args.rs` it can answer, so an unanswerable request fails to
parse rather than at runtime.

The gaps are load-bearing, not unfinished:

- **`watch` takes path-side flags only, and `scan` none.** A walk has not opened the file,
  so it cannot answer `--date` or `--location`. Offering them would mean
  accepting a request answerable only by doing the expensive work the flag
  exists to avoid.
- **`embed`/`faces` omit `--person`/`--category`.** Both are derived from the
  data those commands produce, so selecting their input by one is circular.
- **`locations` takes no selection at all.** Its recompute drops every cluster
  and clears every `location_cluster_id` before rebuilding, so a scoped run
  would not do less work, it would leave everything outside the scope
  permanently unclustered. A partial recompute of a global partition is data
  loss wearing a filter's clothes.

A selection **narrows** an existing set, it never redefines it: each command
intersects the resolved hashes with its own eligibility query rather than
replacing it. And every scoped run prints `N of M`, because a filter matching
nothing is not an error, so without the denominator a wrong filter and an empty
library are indistinguishable.

:warning: **Both selection shapes match each `--path` root in its given *and*
canonical form**, via the shared `roots_in_both_forms`. Only one side of the
comparison can be normalised cheaply: the walk is rooted where the user pointed
it, and a stored row holds whatever the scan recorded, so canonicalising rows at
match time would cost a stat per row. Replacing the root with its canonical form
instead broke both shapes independently, each silently: on Linux `/lib` resolves
to `/usr/lib`, so `--path /lib` matched none of the rows stored under `/lib`;
on macOS the same happened under any tempdir, `/tmp` or `/var`. The row side
survived local testing and was caught only by CI, because `/lib` does not exist
on macOS and the root then survived by accident.

Neither form covers a row stored under a symlink whose root is given as the
target. That needs per-row canonicalisation and is a deliberate non-goal.

Missing data excludes. A file with no GPS never matches `--location`, one with
no date never matches `--date`. Dates fall back to `modified_at` first
(`EFFECTIVE_DATE_SQL` in `videre_core::query`); only a file with neither is
excluded.

### Locks are library-scoped, three files under `<root>/.videre/locks`

`activity.lock` (shared while reading, exclusive while state changes shape),
`init.lock` (state being created or reconfigured), and one `<command>.lock`
per command. Every lock is a non-blocking `flock` that refuses rather than
waits; acquisition order is activity, then init, then command. The locks are
keyed by the library's own state directory, so two differently located
libraries never share one, and a lock file is never unlinked (the lock lives
on the inode). See `videre_core::library_locks`' module doc for the
symlink-refusal rules and the one accepted gap.

### WAL everywhere

Every subcommand opens through `videre_core::db::open_wal`. WAL persists in the
file once set, so it is idempotent and safe on every open. This is what lets
`videre watch` write while a `videre gallery` server reads.

### Every operation leaves the library consistent on its own

A gallery action, or any command, that changes a file or a row must itself
invalidate everything derived from what it changed, before it returns. It never
relies on `videre watch` running, on another command being run next, or on the
user clearing a cache. The test: stop the process right after the operation,
and every stored value is either still true or marked for redo, so the next
`embed`, `faces`, `pipeline` or `watch` finishes the job and `videre status`
reports it as outstanding.

Before adding or changing an operation, list every value derived from what it
touches: rows, per-model stores, `classifications`, face geometry, and every
`<hash>_` file in the thumbnail cache. Keying by content hash is exactly what
makes this easy to miss, because a metadata edit such as an EXIF rotation keeps
the hash while changing what the pixels mean.

### Resumability uses "already processed", not "has a result"

Two tables exist purely for this, and both record work that produced no rows:

- `faces_scanned` records every hash face detection processed, **including
  images where zero faces were found**. Without it, every landscape photo is
  re-detected on every run.
- An unrecognised file records `mime = 'application/octet-stream'` rather than
  NULL, so `mime IS NULL` means only "never scanned". An incremental `scan`
  (the default) uses that. `effective_mime` treats the sentinel exactly as NULL
  and falls back to the extension, so a merely-unidentified file is still
  processed.
- `decode_failures` (`videre_core::decode_failures`) records a file a decode
  stage tried and could not turn into pixels, per `(hash, stage)`. `embed` and
  `faces` decode before their work, and a file that hangs QuickLook or is
  otherwise undecodable produced nothing, was recorded nowhere, and stayed
  pending forever, re-paying its multi-second timeout on every run (including
  every `watch` cycle). The skip is deliberately **two-strike**, not one: a
  QuickLook timeout can be transient (cross-process `qlmanage` contention, see
  that section), so a consumer skips only at `fail_count >= FAILURE_THRESHOLD`
  (2), and any success `clear`s the row. `embed --reprocess` is the retry
  hatch: it `clear_stage`s the recorded failures so a fixed file is tried
  again. `faces` has no such hatch today: a file it gave up on stays skipped. A faces *detection* failure is not a decode failure and is not
  recorded (`WorkerMsg::DecodeError` vs `ImageError`). The gallery's on-demand
  thumbnail endpoint uses the same table under `STAGE_THUMBNAIL`: a HEIC whose
  QuickLook conversion keeps failing is refused before the conversion once at
  the threshold, so it stops re-paying the timeout on every tile request; a
  success clears it. It has no `--reprocess`, so the gallery server
  `clear_stage`s `STAGE_THUMBNAIL` on start: the skip lasts a server run, and a
  transient failure heals on the next launch. Its outstanding count is also kept
  honest: `videre status`
  reports threshold-failed files as `skipped`, not outstanding, so it does not
  suggest a command that would do nothing.

All encode the same lesson: the skip set has to be "already tried", or work
that legitimately produces nothing repeats forever.

### DNG must be vetoed explicitly

`.dng` reports `image/tiff`, because DNG genuinely is a TIFF variant. TIFF is
embeddable and DNG is not decodable, so `ext = 'dng'` explicitly vetoes
embeddability. Routing on mime alone revives the bug fixed 2026-08-01, where
every DNG was queried as pending and failed to decode on every single run.

### One capture date per file, on one clock

Every row carries `capture_date` and `date_source`, resolved by
`videre_core::capture_date::resolve` in this order: EXIF `DateTimeOriginal`, a
video's `com.apple.quicktime.creationdate`, a Google Takeout sidecar's
`photoTakenTime`, a video's `mvhd` creation time, the file's mtime. Every date
consumer reads it through `EFFECTIVE_DATE_SQL` (which falls back to the old
expression for an unresolved row) or `capture_date::CAPTURE_TIME_SQL` (a real
capture time, NULL for an mtime date: fix-dates, status, Events).

**Every stored date is a local wall clock** (`YYYY-MM-DDTHH:MM:SS`), the form
EXIF writes. The Apple key is local time with an offset; the wall-clock part is
kept and the offset dropped. Sources that are UTC instants (a sidecar, `mvhd`,
an mtime) convert through `capture_date::wall_clock`, machine-local, the
inverse of `fix_dates_target`, so a date that goes to the mtime and back is
unchanged. EXIF hour 24 normalises to 00 of the next day.

This is not a stylistic choice. Storing UTC beside wall clocks put two clocks in
one comparison: a clip shot at 21:49 local landed on the following day, an
undated file written at 00:30 local landed on the previous one, and two commands
(import writing sidecar instants, fix-dates writing EXIF as local) undid each
other by hours. `fix-dates` is now the only command that writes an mtime;
`import` changes no file. Measured on a 260-file corpus: 10 clips carry only
`mvhd`. On a 14,471-file Takeout library: 3,518 files had no EXIF date and sat
on the unpack date until scan read their sidecars.

Takeout sidecars are matched by `videre::takeout_sidecar`, shared by scan and
import; a sidecar's `geoData` fills a row with no GPS (`gps_source`).

### A resized copy is confirmed by pixels, at different sizes only

`videre::duplicates` defines every kind of duplicate once, for dedupe, the
gallery's Duplicates page, the static review page and MCP. `resized` takes a
pair within 4 fingerprint bits, at different decoded pixel sizes, aspect within 1%,
and 64x64 greyscale mean absolute difference at most 0.5. Measured 2026-10-06
on a 14,000-image Takeout library: 16x16 could not tell a still burst from a
copy (MAD 1.0 between two separate shots); at 64x64 every owner-confirmed copy
at a different size was at 0.56 or less and every other different-size pair at
0.59 or more.

:warning: **Never let a same-size pair be a resized copy.** At one size, burst
frames (0.16) and messenger copies (0.01-0.1) overlap, and capture time cannot
split them either (iPhone burst frames share the second). Which burst frame to
keep is a choice, not a rule.

Image-model similarity is not a substitute: it scores different shots of one
scene alike. Google's `-EFFECTS`/`-SMILE` creations are matched by name
(`videre_core::takeout_names`), since their pixels range from 0.04 to over 9
from the original. Signatures are decoded only for candidates and kept in
`pixel_signatures`; a gallery rotation forgets them with the fingerprint.

:warning: **The size comes from the decoded pixels, never the row.** A copy
can keep its original's size tags: two Takeout rows read 1932x2576 while one
file's pixels were 1536x2048, and comparing the rows hid that copy.

### Probe videos before invoking QuickLook

`qlmanage -t` does not fail on a container with no video track, it **hangs**, so
videre paid the full 20s `QLMANAGE_TIMEOUT` per such file on every run.
`videre_core::video_probe` walks ISO-BMFF boxes for a `vide` handler and fails
open, so any parse error proceeds to QuickLook as before. Measured on a real
70,601-file library: three audio-only Live Photo companions cost 60s per `embed`
run and, while scan still computed fingerprints, another 60s per `scan --similar`.

### qlmanage concurrency is capped machine-wide

All `qlmanage` launches share one process-wide semaphore
(`videre_core::heic::qlmanage_semaphore`), 6 by default. Raising it from 3 to 6
gave a further ~1.23x on top of a ~3.23x from the worker pool, for ~4.48x over
the serial baseline. Beyond 6 the gains are 1.3-4.4% and per-image detect time
creeps up, so the bottleneck shifts into CPU contention.

**The semaphore alone is per process, so two videre processes permitted 12
against one per-user QuickLook agent.** Each conversion therefore also holds a
`flock` slot in `<cache base>/videre/locks/quicklook/` (`heic::acquire_slot`),
shared by every videre process on the machine whichever library it works on.
The slot count is the process's own cap, so the machine total is the largest
cap in use, not the sum, and the wait comes before spawn, never inside the 20s
timeout.

Measured 2026-09-29 with `faces` on two 250-file libraries at once: before
slots, one HEIC (5.2s to convert alone) timed out in each library on 3 of 3
runs and was recorded in `decode_failures`; with slots, 0 of 3. That is worse
than slow: `decode_failures` skips a file after two strikes, so two overlapping
runs could leave a file permanently without faces. Every other pair measured
(same command twice, watch plus a manual command, faces plus embed, gallery
plus faces, two first-time model downloads) was refused cleanly or gave the
same result as a serial run.

### `pipeline_runs` tracks exactly 8 commands

`videre_core::pipeline_runs::TRACKED_COMMANDS`. `status` is stored as
`running`/`success`/`failed`/`interrupted`; `crashed` is **never written**, only
computed at read time when a `running` row's lock is not held by a live process.
Per-item errors do not mark a run failed, so `fix-dates`/`faces` can exit
nonzero while recording success. They exit through `Exit::skipped(n)`, which `pipeline`
counts as a finished stage with `n` files skipped, not a failed one: a few
QuickLook timeouts once marked a whole night's faces stage as failed.

:warning: **Watch's stages record through `track_cycle_in_as`, never
`track_in_as`.** A cycle updates the latest row but is not kept in
`pipeline_run_history`. Watch reruns a batch while a stage is busy and sees
another command's file writes in waves: one fix-dates run of 6,339 date
changes, through `track_in_as`, filled every stage's history with sub-second
cycles a second apart and pushed out the long runs the history keeps.

### HEIC conversion uses `qlmanage`, never `sips`

Some HEIC files (iPhone photos where rotation is encoded via the HEIF `irot`
box rather than an EXIF Orientation tag) come out sideways with `sips`, which
copies raw sensor-buffer pixels unrotated. This affects face detection,
embedding preprocessing, and every thumbnail path.

### Raster thumbnail orientation is applied before resize and cache

JPEG camera pixels may be landscape-shaped while EXIF Orientation declares the
portrait display transform. The gallery's sized raster path reads orientation
from the decoder over the source bytes already in memory, applies it, then
resizes and encodes. Re-encoding first discards the tag and permanently caches
the sideways pixels.

Raster previews use their own versioned cache names instead of the QuickLook
thumbnail names. This lets a raster-rendering change bypass incompatible output
without invalidating HEIC conversions, which can take seconds each. Grid,
lightbox, date, similarity and paginated **Show more** cards all go through the
same `buildPreview` and sized-file endpoint; do not add a second preview path.

### Every source-file decode is orientation-correct, through one helper

`videre_core::image_decode` is the only place that turns a source image file
into pixels: `decode_oriented_file`, `decode_oriented_bytes` (gallery),
`decode_oriented_reader` (already-open handles), plus raw variants returning
the tag for callers that must crop first. All of them read the EXIF
Orientation tag through the image crate's decoder and apply it, so what every
consumer sees is the display canvas a person sees. A bare `image::open` of a
source file anywhere else reintroduces the rotated-canvas bug that left the
owner's faces as unassigned singletons (detection scrambled, ArcFace
embeddings orthogonal to their own identity) and made SigLIP embed sideways
scenes.

:warning: **Never route videre-produced files through the helper.** QuickLook
conversions (HEIC, video) and the thumbnail/`original` caches are already
upright pixels with no orientation tag; applying orientation again
double-rotates.

Every face row's bbox and landmark are on the display canvas, so face
thumbnails decode upright and then crop. `faces.oriented` is written as 1 and
nothing reads it: raw-canvas rows only existed in libraries older than schema
3, which are refused.

### Logging goes through one tracing subscriber; errors are logged once

`main` installs a `tracing` subscriber (`crates/videre/src/logging.rs`) once
the command and library are known: a terminal layer that prints what videre
always printed, plus `.videre/logs/<command>.log` (errors and warnings,
always) and `<command>.trace.log` (only at `log_level` info or debug). Library
crates only emit events. Only targets starting with `videre` reach any layer.

A failure is logged once, at the highest boundary that still knows what
failed, through `videre_core::error_log::report`. Lower levels never log an
error they also return; they add `.context(...)` and, where the cause is
known, a `videre_core::error_kind::ErrorKind`. `log_level` affects the files
only, never the terminal.

:warning: **Never call `std::process::exit` in the binary** outside the last
line of `main`. It skips destructors, and the log writers flush on drop, so a
direct exit loses the final lines. Return `exit::Exit` instead: `Exit::code`
for a deliberate status, `Exit::shown` for a failure already printed as JSON.
The SIGINT handler exits from core, so it calls `videre_core::shutdown::flush`
first.

## The commit guard

`hooks/pre-commit`, installed with `git config core.hooksPath hooks`. It refuses
a commit that does not look like a source change, on three rules: more than 30
files staged, a path escaping the repository layout, or a file that is not
source, config, docs, or a fixture.

:warning: **It exists because certain amount of thumbnail images of personal
photographs reached public repository** because of AI model. `git add -A`
staged a cache a tool had written into the working tree, and `git commit -q`
suppressed the "n files changed" line that would have shown it at once.

:warning: **Removing that data cost far more than preventing it would have.** A
force-push does not undo it: `refs/pull/*/head` are read-only to repository
owners and keep every commit reachable, and `raw.githubusercontent.com` serves
the blobs from a separate cache. Both were verified still serving the images
after the rewrite. Only GitHub Support can clear either, and the repository had
to be made private in the meantime.

Two rules take an override, because a guard that cannot be overridden is deleted
the first time it is wrong: `VIDERE_ALLOW_BIG_COMMIT=1` and
`VIDERE_ALLOW_ANY_FILE=1`. The path rule has none, since a staged path beginning
with `~` or `/` is never intentional.

**Media belongs in `crates/*/tests/fixtures/` or `docs/public/` and nowhere
else.** That is what the file-kind rule encodes, and it is why a new fixture
commits without argument while a stray image does not.

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

**It is a Cloudflare Worker**, configured by `docs/wrangler.jsonc` and named
`videre-doc`. Specifically an **assets-only Worker: there is no `main` entry
point**, so Cloudflare serves the built `dist/` directly and no script runs on
the request path. `not_found_handling` is `404-page`, so unknown paths get
Starlight's own 404 rather than a bare Cloudflare error.

:warning: **Adding a `main` changes the billing model.** Static asset requests
are free and unlimited; the free plan's 100,000/day cap counts **Worker
invocations**, which today are zero. Anything that can be a file in
`docs/public/` should be, rather than a route in a script.

`docs/public/` is copied to `dist/` verbatim (`robots.txt`, `favicon.svg` and
friends arrive that way), which is how a non-Astro file gets served at a fixed
path.

### The install script and the apex

`docs/public/install` is the shell installer, served at
<https://docs.videre.sh/install> as an ordinary static asset. It is also
reachable at <https://videre.sh/install>, which is a **Workers route** on the
`videre-doc` Worker rather than a redirect, so both addresses serve the same
bytes.

The apex zone is separate from `docs.videre.sh` and carries a proxied
placeholder `AAAA @ -> 100::` (the IPv6 discard prefix), which is what lets
Cloudflare answer for the hostname at all when there is no origin behind it.

:warning: **`https://videre.sh/` itself returns 522, and that is expected.**
Only `/install` is routed; every other path reaches the placeholder and fails.
The root is reserved for a landing page. Do not "fix" the 522 by pointing the
apex at the docs Worker, because that would silently make the docs site the
landing page.

:warning: **The script's own `--help` prints the `docs.videre.sh` address on
purpose.** That is where the file actually lives; the apex is a route that can
be removed in the dashboard without touching this repo. A script that advertises
an address it does not depend on keeps working if the route disappears.

Changing the asset naming in `.github/workflows/release.yml` breaks the
installer, and nothing else connects the two.
`.github/scripts/test-install.sh` is what catches it, run by
`.github/workflows/install.yml` on both platforms and by `make test-install`.
It caught exactly that on its first run: the checksum asset replaces the
`.tar.gz` extension rather than extending it.

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

## Invariant index

Detail lives at the code site named; this is only a pointer, so a constraint is
discoverable before you touch its code. Cross-cutting invariants stay in full
above.

- Import file-location ladder -> `videre_core::import_location` (module doc)
- `embed --batch` corrupts above ~121 -> `videre_ml::model::MAX_SAFE_BATCH`, `clamp_batch`
- Per-library, per-model embedding DBs -> `videre_core::embeddings_db`
- Input validation at every entrance -> `videre_ml::model::clamp_batch`, `videre_core::embeddings::validate_model_id`
- Timeout error path must not touch the filesystem -> `videre_core::io_timeout::run_with_timeout_for_path_detailed`
- Model loads inside `with_work`, never before -> `videre_core::work`
- Person identity vs display name -> `videre_core::person`
- Search predicates shared by CLI and MCP -> `videre_core::query`
- Hashing has a no-progress deadline only, never a total or size-derived one:
  any nonzero read refreshes it -> `videre_core::io_timeout::run_with_progress_timeout`
- Other whole-file read timeouts scale with size, the stat timeout does not -> `videre_core::io_timeout::timeout_for_size`
- One I/O worker holds one permit from before spawn until its closure exits,
  timed out or not; a full pool refuses (capacity), never reported as a dead
  drive, and prune treats it as unknown -> `videre_core::io_timeout` (`IoWorkerPool`)
- Face clustering O(n^2) fixes (memory and time) -> `videre_core::face_cluster`
- Location clustering has no n*n matrix: cells first, then sparse average
  linkage -> `videre_core::location_cluster::cluster_by_distance`
- Source-file decodes are orientation-correct -> `videre_core::image_decode`
- Watch prunes by default (`--no-prune` opts out) and cannot override the
  guards; dedupe and the gallery's Delete and Trash clean up as they remove ->
  `commands::prune::PruneArgs::for_watch_stage`, `commands::prune::clean_up_after_removal`
- `videre locations` is a global recompute -> `commands::locations`
- One clustering parameter set; flag > `gallery.json` `faces.clustering` >
  built-in, per field (watch: no flag layer) -> `videre_ml::cluster_params`,
  `commands::cluster_settings`, `gallery::recluster`
- Undecodable files are skipped after two strikes -> `videre_core::decode_failures`
- watch completes every new file per batch: the full place recompute and face
  regroup only when the last one took under five seconds, otherwise only the
  new rows and faces are placed and attached; a bulk import scans first and
  finishes once quiet -> `videre_core::recompute_cost`,
  `location_cluster::assign_new_rows`, `pipeline::attach_new_faces`,
  `commands::watch` (`next_mode`)
- QuickLook conversions share a machine-wide slot pool, waited for before
  spawn -> `videre_core::heic::acquire_slot`
- Face learning never uses `AppState.conn`: its own connection and thread,
  off unless `gallery.json` sets `faces.learning` -> `gallery::learning` (module doc)
- Never open the library database through `std::fs`: closing any descriptor
  on it drops every SQLite lock the process holds, and another process then
  deletes the WAL under a live connection -> `videre_core::library_db::is_sqlite_file`
- Errors logged once, at boundaries; per-command log layout and reader -> `videre_core::error_log`, `videre_core::error_kind`, `crates/videre/src/logging.rs`
- File identity is the content key, metadata excluded -> `videre::content_key`
- Duplicate kinds, keepers and the whole-group query rule, shared by dedupe,
  the gallery and MCP -> `videre::duplicates`
- One capture date per row, every date a local wall clock; schema 5 adds it in
  place and the next scan resolves old rows without rehashing ->
  `videre_core::capture_date`, `videre::sqlite_output::resolve_unresolved`
- Face IDs are never reused (schema 4); prune keeps the journal and withdraws
  evidence that lost its source face -> `videre_core::library_db::upgrade_v3_to_v4`,
  `videre_core::face_learning::prune`
- Offline map basemap: one shared PMTiles archive per machine, downloaded once
  behind a cross-process flock; the map grid never gates on MapLibre's load
  (a WebGL probe can pass where the context still cannot render) -> `videre_core::basemap`, `commands::gallery::server` (`handle_basemap_*`), `static/map.js`
- A setting's range or choices are written once: gallery settings in
  `static/gallery-schema.json` (the views clamp with it, a save is checked
  against it), config keys in `library_config::KEYS` (whose constants `edit`
  and `load` enforce); the settings pages draw from both and a save lands
  whole or not at all -> `gallery::settings::validate`, `library_config::edit_many`,
  `gallery::config_form`, `static/settings-form.js`
