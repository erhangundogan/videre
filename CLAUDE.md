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

One binary, `videre`, with fifteen subcommands. `main.rs` dispatches to one
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
failure never hides the other's, and `cargo test --no-fail-fast` so one failing
test *binary* never hides the later ones. Both were learned the same way: a
Linux-only failure in `videre-core`'s lib tests stopped the run before the
`videre` integration tests, whose Linux result was then unknown rather than
green. Only four tests are macOS-gated.

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

:warning: **A test that downloads does not just break "tests never download",
it changes which *other* tests run.** The warm-up step fetches SigLIP on macOS
only, so on Linux `siglip_ready()` is false and
`cpu_batch_matches_single_image_baseline` skips in milliseconds - it had never
once run there. `argument_robustness.rs` then scanned an `a.jpg` and ran
`videre embed` against it, which loads the model and pulls 777MB; the cache
saved those weights, and from the next run the batch test found a warm cache
and began really running. 28 minutes, then 35, with nothing failing, which is
why it read as a hang rather than a slow test.

The fix is the `.dng` in that test library: scanned and stored like anything
else but explicitly vetoed as non-embeddable, so `embed` and `classify` return
before loading a model. `HF_HOME` points into the test's temp dir and a guard
asserts the sweep leaves it at zero bytes.

:warning: **Optimizing dependencies in the test build was the wrong fix, and
was reverted.** `[profile.dev.package."*"] opt-level = 3` did make the woken
test bearable (2287s to 66s, since unoptimized candle measures **9.41s per
224px forward pass against 0.168s in release, 56x**), but it put release-grade
codegen in every test build: Ubuntu's Build step went 35s to 660s and did *not*
amortize per cache key as predicted. Wall clock stayed ~4x worse than before
0.15.3. Removing the weights instead lets the test skip in milliseconds, which
is what it did for its whole life. Deleting a cached artifact beat optimizing
the work it caused.

The salt in the model cache key (`hf-v2-`) exists because the key hashes
`face_models.rs` and `embeddings.rs`, neither of which changed, so the poisoned
cache would otherwise be restored forever.

The remaining hole is that this test skips silently rather than calling
`skip_without_models`, so `VIDERE_TEST_REQUIRE_MODELS=1` - which exists to turn
exactly this into a failure - never fires for it.

Clippy is not in CI yet: it reports 31 warnings as of 2026-08-16 (18 when
first counted), so a lint job would need `--allow`-ing them or a cleanup
pass first. `make lint` runs it. The count drifts upward precisely because
nothing enforces it, which is the argument for adding the job.

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

### Import locates files through one shared contract

`videre_core::import_location` owns the ladder every `videre import` source
uses: an optional provider database, then known folder layouts, then asking the
user. `videre_core::import_providers` is the data, a static table of descriptors
plus structural detection.

**The default never opens a provider database.** Apple is read purely from the
filesystem, because Apple's schema changes between macOS releases and its folder
layout does not: `originals/` (Photos 5+), `Masters/` (iPhoto 9), `Originals/`
(early iPhoto). Lightroom is the one source starting on the database rung, and
there it is not optional, since its files live in arbitrary user folders with no
layout to fall back to.

A new source declares which rung it starts on and supplies only the
provider-specific part. It must not invent its own discovery scheme. If adding a
provider ever requires editing `import_location.rs`, the design has failed.

Asking a catalog *where to look* is not the same as asking it *what is there*.
Location may come from a database; content and metadata always come from the
files.

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

### Input validation belongs where every entrance passes

Two panics reachable from ordinary CLI input, both the same shape: a guard
placed at one of several entrances to the same function.

- **Model ids** are validated by `videre_core::embeddings::validate_model_id`,
  called from `resolve_model_id_in`. It used to be private to
  `commands/config.rs`, so `videre config set` was guarded and `--model` was
  not, and `videre embed --model foo` hit
  `split_once('/').expect("model id is owner/name")` in `videre-ml`. `--model`
  also has a clap `value_parser`, so a typo fails at parse time rather than
  after an unrelated "no database found".
- **Batch sizes** go through `videre_ml::model::clamp_batch`, because
  `slice::chunks(0)` panics. `embed` guarded it, `faces` did not.

:warning: **`clamp_batch`'s upper cap is a parameter, not a constant.**
`MAX_SAFE_BATCH` exists because *this inference path* silently corrupts
embeddings above ~121. That is a fact about embedding, not about face detection,
so `faces` passes `None` and gets the zero-guard only. Do not "tidy" the two
callers into sharing one cap.

### The timeout error path must not touch the filesystem

`hash_file`'s timeout handler called `std::fs::metadata` **unbounded** on the
path that had just timed out, only to name the timeout in its message. On a
stale mount `metadata` is the call that never returns, so the handler hung in
the exact scenario the timeout exists to survive - the protection defeated by
its own error reporting.

`io_timeout::run_with_timeout_for_path_detailed` returns `TimedOutAfter`, which
carries the timeout that was applied and which phase used it. Format from that.
Anything in this path that consults the filesystem reintroduces the bug.

It also made the message honest: a dead drive reports that the `stat` never
answered, instead of claiming a read took 20s when nothing was read.

### A model is loaded inside `with_work`, never before it

`videre_core::work` owns "compute what is pending, narrow it by the selection,
stop if nothing is left". `embed` and `classify` run their whole tail inside
`with_work`, so `Embedder::load` is unreachable when there is nothing to
process. That is structural, not a convention: there is no code path from
`Work::Nothing` to the closure, and a unit test asserts the closure does not
run.

It exists because the convention failed. All three commands hand-wrote the same
guard, `classify`'s copy was wrong, and it downloaded **778MB of SigLIP from
inside a unit test**. On CI those weights entered the model cache and woke
`cpu_batch_matches_single_image_baseline`, which skips when weights are absent
and had skipped since the day it was written; the Ubuntu job went from ~3
minutes to nearly 40. Three copies meant no single test could cover it.

:warning: **`faces` shares `narrow` but not `with_work`, deliberately.**
Clustering runs on every path through that command, including when there is
nothing new to detect, so gating the tail on "is there work" would silently stop
it. Detection is the optional part there, not the command.

Messages are unified rather than per-command: `pending item(s)` everywhere, one
sentence per state. A command needing different wording passes it in via
`Words::saying` rather than printing its own, which is how three names for one
idea appeared in the first place.

### A person has an identity and a display name

`videre_core::person::normalize` produces the identity - trim, fold diacritics
to ASCII, lowercase, spaces to `_`, drop punctuation - and it is what
`faces.person_label` stores and what the URL uses. `people(name PRIMARY KEY,
full_name)` holds what a reader sees. `alice` and `Alice` are one person by
construction rather than by a comparison rule every call site must remember.

**Folding, not stripping.** Dropping a diacritic turns `Şefik` into `efik` and
`Çağdaş` into `ada`, eating the first letter of any name starting with one - 14
of 85 names in the library this was built against. Turkish `I` is mapped
explicitly because `to_lowercase` is Unicode-default, not locale-aware: `İ`
lowercases to `i` plus a combining mark. A test pins the whole set
`öÖüÜıIiİşŞçÇğĞ`.

:warning: **`normalize` must stay idempotent.** Reads normalize too, so it runs
on values that are already identities. The first version dropped `_` as
non-alphanumeric, so `isil_ozyegin` became `isilozyegin` and every multi-word
person URL would have resolved to nothing. `_` is a separator on input as well
as output, and a test asserts the fixed point.

**`by_person` matches either form**, because both are things a user types: the
identity from the URL, and the display name from the screen. They stop agreeing
as soon as someone adds a surname.

:warning: **Any entry point taking a person name must normalize**, and there are
more than expected: `assign`, `set_primary`, `delete_person`, `set_full_name`,
`person_detail`, `person_search`, plus `query::by_person` which funnels
`search --person`, the selection layer and the MCP tool. Migrating labels
without this made `videre search --person "Ahmet Arı"` return nothing while the
labeling UI still showed the person.

:warning: **An identity is permanent, and that is a design decision, not an
omission.** `set_full_name` changes what a person is shown as; nothing changes
the `name` a face row points at or the `/person/<name>` URL. `rename_person` and
`/api/rename-person` existed, worked, were tested, and were called by no page,
so they were removed in 0.17.0 rather than given a caller: an endpoint that
exists eventually gets used, and videre-desktop had already wired it up. To
correct an identity, delete the person and relabel.

`Error::Conflict` went with it, because that function was the only thing that
ever constructed it.

No foreign key from `faces.person_label` either: SQLite leaves
`PRAGMA foreign_keys` off and videre never sets it, so `REFERENCES` would be
documentation rather than a constraint. The original argument was sharper, that
an unenforced `ON UPDATE CASCADE` would silently orphan every face row on a
rename; with renames gone that particular hazard is too, and the pragma reason
stands on its own.

**One resolver, and a test that all surfaces use it.**
`person::resolve_identities` turns a typed name into every identity it could
mean, and `by_person` and `search_by_person` both call it. They did not always:
each normalized its own argument, the two drifted, and the labeling UI stopped
finding anyone whose display name had been edited while the CLI still found
them.

:warning: **SQLite's `LOWER()` is ASCII-only, so the display-name comparison
must stay in Rust.** `LOWER('Ö')` is `'Ö'` while Rust gives `'ö'`, so
`LOWER(full_name) = ?` matched no name containing a Turkish character - 15 of
86 people in the library this was built against. Pushing that comparison back
into SQL for tidiness silently breaks most of the library.

`crates/videre-api/tests/person_surfaces.rs` exercises every lookup and display
surface against one person whose **display name does not normalize back to
their identity** (`ozgur_demirtas` shown as `Özgür`). The divergence is the
point: while the two agree, the identity path satisfies every assertion alone
and the display path is never exercised, which is how tests passed while two
surfaces were broken. Each of the three bugs was replayed against this file and
each one fails it. A new surface belongs there; one that cannot be added is not
going through the resolver.

:warning: **Fixtures in this area must use non-ASCII names.** Every fixture
here was `Alice`/`Bob`, and `Alice` cannot expose an ASCII-only `LOWER()`. The
test data has to look like the library.

### Every filter goes through `videre_core::selection`

One layer, two shapes. `RowSelection` filters rows that exist in the database
and resolves to a hash set; `PathSelection` filters a filesystem walk and is
pure. A command declares its vocabulary by flattening only the clap groups in
`commands/selection_args.rs` it can answer, so an unanswerable request fails to
parse rather than at runtime.

The gaps are load-bearing, not unfinished:

- **`scan`/`watch` take path-side flags only.** A walk has not opened the file,
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
no date never matches `--date`. Dates fall back to `modified_at` first (see
`EFFECTIVE_DATE_SQL` below); only a file with neither is excluded.

### Search predicates live in one place, used by two surfaces

`videre_core::query` owns every search filter: `by_date`, `by_person`,
`by_category`, `by_location`, plus `candidates_with_model` which intersects the
active ones into a hash set. `commands/search.rs` builds a `Filters` from its
args; `commands/mcp.rs` builds a `SearchArgs` and calls straight into
`search::run_json` behind the `QueryEmbedder` trait, so the two surfaces run the
*same* code rather than parallel implementations that can drift.

`QueryEmbedder` exists only so the MCP server can keep its embedder cached
across calls while the CLI builds a fresh one per invocation. Do not collapse it
back into a concrete type without solving that.

**The effective date is `EFFECTIVE_DATE_SQL`, not `exif_date`.** Date filters
match `exif_date` when present and not `0000%`, else `modified_at`. The `0000%`
guard is the same rule `output.rs::best_date` uses when picking which duplicate
to keep; a camera with an unset clock must fall back rather than match year
zero. Filtering happens before ranking, so a composed query scores fewer
vectors than an unfiltered one.

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

### Video dates are local wall-clock, like photo dates

`videre_core::video_meta` prefers `com.apple.quicktime.creationdate` (local
time with a UTC offset) and stores the wall-clock part, discarding the offset.
It falls back to `mvhd`'s creation time, which is **UTC**, only when that key is
absent.

This is not a stylistic choice. `exif_date` holds local wall-clock for photos
because EXIF carries no timezone, and every date filter, `EFFECTIVE_DATE_SQL`
and `output::best_date` compares those strings. Storing UTC for video would put
the two on different clocks in one column: a clip shot at 21:49 local lands on
the following day, `--on` misses it, and a chronological sort interleaves it
wrongly against photos taken minutes earlier. Silent and permanent.

Seeing two date sources and picking the standard-looking one is the obvious
"simplification" here, so the reasoning sits at the parse site as well. Measured
on a 260-file corpus: 10 carry only the UTC field, all re-encoded renders rather
than camera originals.

:warning: **Video metadata needs a full re-scan to appear.**
`--retry-incomplete` keys on `mime IS NULL`, which does not mean "scanned before
video metadata existed", so an older library shows empty dates until re-scanned.

### The read timeout scales with file size; the stat timeout does not

`DEFAULT_IO_TIMEOUT` (20s) is a floor, not a ceiling.
`io_timeout::timeout_for_size` scales a whole-file read by
`MIN_READ_RATE_MB_S_DEFAULT` (20 MB/s), because a constant cannot tell a large
file from a stalled one. Measured 2026-08-12: a healthy 3.7GB video on a drive
sustaining 158 MB/s needs ~23s and was being skipped with a message blaming the
drive. Sizes do not change, so such files were skipped on **every** run, no row
was written at all, and they are by definition the library's longest videos.
Configurable via `videre config set read-rate`.

The `stat` that reads the size keeps a short *constant* timeout, and that
ordering is the safety property: `fs::metadata` is itself one of the calls a
stale mount blocks forever, so a dead mount fails there and the read is never
attempted. Without that, a large file on a dead mount would hang for its scaled
timeout.

:warning: **This applies to whole-file reads only.** `hash_file` reads every
byte, so duration really is proportional to size. The decode paths do not:
`decode_via_quicklook` extracts a poster frame from a fraction of a video, so
scaling those by full file size would turn the known "QuickLook hangs on a
container with no video track" failure from 20s into minutes.

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

### `videre locations` is a full recompute, and the cost moved

Measured on a 70,601-file library with 26,744 distinct coordinates: **412s
before, 86s after** indexing the GPS columns. The whole recompute runs in one
transaction, so it holds the single WAL writer lock for that entire window and
a concurrent `watch` write blocks.

The per-coordinate `file_hashes` UPDATE used to match
`ROUND(gps_lat, 6) = ROUND(?, 6)`. A function call on a column makes any index
unusable and there was no GPS index anyway, so each update scanned every row.
It matches exactly now, against `idx_file_hashes_gps`. `coords` comes from
`SELECT DISTINCT gps_lat, gps_lon`, so those are the exact stored values and
matching them back exactly returns exactly the rows they came from.

The same `ROUND` also over-matched: two coordinates differing past the sixth
decimal both claimed the same photo, and `photo_count` counted it twice. **181
photos were double-counted**, 179 in Berlin. A test pins both halves, including
that `EXPLAIN QUERY PLAN` still says SCAN for the `ROUND` form, so nobody adds
an index and wonders why nothing got faster.

:warning: **The dominant cost is now `cluster_by_distance`, not the UPDATE.**
It builds a dense `vec![vec![0.0; n]; n]`: at 26,744 coordinates that is ~5.3GB
claimed before any work plus ~357 million haversine calls. This file called it
"sub-second", measured when the library had 5,512 coordinates; the video
re-scan that gave 11,985 videos dates and GPS made every one a coordinate to
assign. Believing the stale note is what put the first progress bar on the
wrong phase, whose 4.8x speedup then made the *unmeasured* phase the whole
wait - reported twice as a freeze. **Time the phases before instrumenting.**

Both phases report progress now, and `cluster_by_distance_reporting` takes a
per-row callback (n calls, not n^2/2). Progress counts coordinates, not images:
`Progress::new_counting` exists because the non-TTY line hardcoded "images
processed", which for 26,744 coordinates in a 70,601-file library was three
numbers no reader could reconcile.

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

**It is a Cloudflare Worker**, configured by `docs/wrangler.jsonc` and named
`videre-docs`. Specifically an **assets-only Worker: there is no `main` entry
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

**The apex `videre.sh` is registered** on Cloudflare and separate from
`docs.videre.sh`. It is reserved for a landing page and deliberately has no
apex record yet, so it does not resolve.

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
