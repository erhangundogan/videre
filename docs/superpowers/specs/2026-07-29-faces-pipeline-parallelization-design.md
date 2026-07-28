# videre faces pipeline: profiling + parallelization - design

Date: 2026-07-29
Status: approved for planning
Scope: (1) add per-stage timing instrumentation to `run_face_pipeline`, (2)
restructure it from strictly serial to a multi-worker pipeline (approach A -
a symmetric worker pool). No changes to detection/embedding models, clustering
logic, or CLI-level resumability semantics beyond the note in "Resumability"
below.

## 1. Background

`videre faces` measured at ~1.3 images/second (~0.84s/image average) against
the user's real library. An independent investigation (see memory
`faces-speed-levers` and this session's transcript) ruled out GPU acceleration
(CoreML measured zero speedup in an earlier session, and Amdahl's law caps any
possible GPU win at ~1.2x even if the mechanism were fixed) and ruled out
vision-LLM-based detection/clustering (would be 3-10x slower per image, and
general vision embeddings are unsuited to identity-discrimination clustering
quality). The strongest identified lever instead: `run_face_pipeline`
(`crates/videre-ml/src/pipeline.rs`) processes one image at a time - load,
detect, align, then batch only the embedding call - despite the codebase
already having a `qlmanage` HEIC-conversion semaphore that permits 3 concurrent
conversions (`crates/videre-core/src/heic.rs`) that a fully serial pipeline
never uses.

This spec covers making that pipeline concurrent. Per explicit scope
agreement, three other identified levers (capping HEIC render size, JPEG
scaled decoding, pixel-packing loop micro-optimization) are NOT part of this
spec - each is smaller and independent enough to be its own follow-up once
this lands.

## 2. Phase 1: profiling instrumentation

**Goal:** get real per-stage timing data before finalizing Phase 2's worker
count/tuning - every estimate in the investigation that led here (including
the ~0.84s/image breakdown) was itself an estimate, not a measurement.

**Add a `--profile` flag to `videre faces`** (`crates/videre/src/commands/faces.rs`).
When set, `run_face_pipeline` (and its CLI-facing summary) accumulates timing
for each stage of the existing serial loop:

- `load` (the `load_image` call - HEIC via `qlmanage` vs. everything else via
  `image::open`, tracked separately since HEIC is the known-different case)
- `detect` (`detector.detect`)
- `align` (`face_align::align_face`, summed across all crops for that image)
- `embed` (`embedder.embed_batch` - a per-chunk cost; divide by the chunk's
  image count for a per-image average)
- `db_write` (`replace_faces_for_hash` + `mark_scanned`)

Implementation shape: a `ProfileStats` accumulator struct (fields like
`load_heic: Duration`, `load_other: Duration`, `detect: Duration`,
`align: Duration`, `embed: Duration`, `db_write: Duration`, plus per-category
image counts), threaded through `run_face_pipeline` as an `Option<&mut
ProfileStats>` (or similar - `None` when `--profile` isn't passed, so the
timing calls are the only overhead in the common case, not the struct's
existence). `std::time::Instant::now()` wraps each stage inline in the
existing loop; no concurrency involved in this phase.

At the end of the run, print average ms/image per stage (overall, and split
HEIC vs. other) to stderr, gated by the `--profile` flag - this is a debugging/
tuning tool, not a permanent user-facing report, so it doesn't need to go
through the same formatting conventions as the normal summary line.

**This phase ships and gets run once against the real ~35k-image remaining
library before Phase 2's worker-count default is finalized** - if the load
stage turns out to dominate by an even larger margin than estimated, that's a
signal to revisit the worker-count heuristic in Phase 2; if it's more balanced
than expected, the default may need adjusting. The plan should treat "run
`--profile` and report the numbers" as an explicit checkpoint between the two
phases, not just an implementation detail to build and forget.

## 3. Phase 2: parallel pipeline (approach A - symmetric worker pool)

**Rejected alternative (approach B, producer/consumer split):** separate
loader-thread pool feeding a bounded queue into a separate inference-worker
pool. Rejected because: the overlap it targets (I/O-bound load vs. CPU-bound
inference) already happens for free in approach A (while one worker's
`qlmanage` subprocess blocks, the OS scheduler runs another worker's ORT
inference on the freed core) - B only wins if the load:inference throughput
ratio is known and skewed enough to justify a fixed pool-size split, which
isn't known before Phase 1's data exists, and guessing it wrong makes B worse
than A (idle threads in whichever pool was over-provisioned). B also requires
designing real backpressure (bounded channel between stages: block, drop, or
grow unbounded when inference falls behind?) that A avoids entirely, since
each worker holds at most one decoded image before immediately running
inference on it.

### 3.1 Partitioning

`to_process: &[(String, String)]` is split **round-robin** across N worker
threads: worker *i* processes indices `i, i+N, i+2N, ...`. Not contiguous
slices - round-robin avoids one worker getting stuck with a disproportionately
HEIC-heavy (or otherwise slow) run of images if the underlying `SELECT path,
hash FROM file_hashes WHERE ext IN (...)` query happens to return rows
clustered by extension (e.g., a batch of HEIC photos from one import session
sitting contiguously in path/rowid order).

**Accepted tradeoff:** round-robin significantly reduces but does not
eliminate tail imbalance (one worker could still finish later than others if
its round-robin-assigned subset happens to skew slow). Dynamic work-stealing
would eliminate this but adds real complexity for a marginal, unmeasured gain
- not in scope for this first version. Revisit only if Phase 1/2 measurement
shows a real, sizable tail-imbalance effect.

### 3.2 Workers

Each worker thread:
- Loads its own `FaceDetector` and `FaceEmbedder` (own ONNX `Session`
  instances via `face_models::build_session` - the model files are already on
  disk after the first download, so N independent loads costs extra time at
  startup and extra memory (~180MB x 2 models x N workers, per the
  `faces_speed_levers` memory's own note that this is trivial against 32GB
  unified memory), not extra network/disk fetches).
- Runs the *same* load -> detect -> align -> accumulate-into-`batch`-sized
  chunk -> `embed_batch` logic that `run_face_pipeline` already has today, on
  its own round-robin-assigned subset of `to_process`. The per-worker `batch`
  parameter is unchanged from today's meaning (images per `embed_batch` call).
- Sends results back to the coordinator (see 3.3) over an `std::sync::mpsc`
  channel - no new dependency needed, stdlib `mpsc` is sufficient for this
  workload (moderate message rate, not a hot loop needing lock-free queues).

Threads are spawned via `std::thread::scope`, which lets each worker borrow
`to_process` (and any other read-only setup data) directly from the
enclosing stack frame without needing `Arc`/`'static` bounds, since the scope
guarantees all spawned threads are joined before it returns.

**Session intra-op thread cap:** each worker's ORT session must have an
explicit low intra-op thread count set (via whatever `ort` 2.0.0-rc.12's
`SessionBuilder` exposes for this - confirm the exact method name/signature
against the installed crate's docs during implementation rather than assuming
one; do not skip this - leaving each session's default all-core intra-op pool
in place would mean N workers each trying to use all 10 cores simultaneously,
which oversubscribes far worse than today's single-session baseline).

### 3.3 Coordinator (main thread)

The main thread, inside the same `thread::scope`, is the **sole owner of the
SQLite connection** - workers never touch it. It loops receiving messages from
the shared `mpsc::Receiver` (one `Sender` clone per worker) until the channel
closes (all workers finished and all `Sender` clones dropped), and for each
message:
- Calls `progress.tick()` (exactly as today, just now driven by messages
  rather than inline per-image work)
- On a no-face result: `mark_scanned` (if not `dry_run`), matching today's
  behavior of recording zero-face images so they aren't re-detected
- On a faces-found result: `replace_faces_for_hash` then `mark_scanned` (if not
  `dry_run`) - same ordering as today, so an interrupt before the write still
  leaves that hash unmarked and eligible for re-processing on the next run
- On an error result (image load failure, detect failure, embed_batch
  failure): increments `detect_errors`/`write_errors` and prints the same
  `skipping <path>: <reason>` / `detect failed <path>: <e>` style messages
  `run_face_pipeline` already prints today, just routed through the channel
  instead of printed inline by the worker (keeps stderr output
  single-threaded/non-interleaved, which matters for readability)

This keeps the DB-access-serialization property working as it does today
(never holding a lock across CPU-bound work - see the desktop app's earlier
lock-contention fix for why this matters), just extended from "single-threaded
by construction" to "single-threaded by design, with N producer threads."

**Message shape** (exact type left to implementation, but must carry enough
information for the coordinator to reproduce today's exact behavior):
roughly `enum WorkerMsg { ImageError { path, msg }, DetectError { path, msg },
NoFace { hash }, FacesFound { path, hash, rows: Vec<FaceRow> }, EmbedBatchError
{ paths: Vec<String> } }` (the last covers `embed_batch` failing for an entire
chunk, matching today's `detect_errors += chunk_entries.len()` on that path).

### 3.4 Worker count

Defaults from `std::thread::available_parallelism()`, with a simple heuristic
(exact formula to be finalized using Phase 1's real numbers - e.g., if load
dominates as expected, more workers than a naive "cores / 2" split may be
worth trying, since many workers can be blocked on `qlmanage` subprocesses
simultaneously without consuming CPU). Overridable via a new `--workers <n>`
flag on `videre faces`. Not added to `videre watch`'s faces stage in this
pass - `run_face_pipeline`'s signature gains a `workers: usize` parameter, so
wiring a `--workers` flag into `watch` later is a small follow-up, not
something this spec needs to solve now.

### 3.5 Resumability - correctness is preserved; the "lost work on interrupt" window grows

**Correctness first, since this matters more than the window size:** every
`WorkerMsg` is still handled one-at-a-time by a single coordinator thread,
which performs the exact same "write the faces, and only call `mark_scanned`
if that write succeeded" sequence used today, per hash, synchronously. No
hash can ever end up marked scanned without its rows being durably written
first, regardless of worker count - an interrupt (or a worker panic; see 3.3)
can never cause a skipped image, a double-processed image, or a corrupted
write. `select_unscanned` on restart is unchanged and correctly recomputes
the to-do set from `faces_scanned`.

**The size of the "how much gets re-done after a kill" window does grow,
and today's baseline is already larger than "one image":** the *current*
serial pipeline already defers `mark_scanned` for every face-bearing image
in a chunk until that chunk's single `embed_batch` call resolves (see the
`for chunk in to_process.chunks(batch)` loop in `pipeline.rs` - only
zero-face images are marked immediately, inside the per-image loop). So an
interrupt today can already lose up to `batch` (default 8) images' progress,
not 1. With `workers` worker threads, each independently doing this same
chunk-batching on its own partition, the real window becomes up to
`workers * batch` images - e.g. 64 with the default batch of 8 and 8
workers, not `workers` alone. This is a real, larger-but-still-bounded
tradeoff worth documenting precisely (not understating it as "loses at most
N images"), directly following from processing that many images
concurrently instead of one chunk at a time. No mitigation (e.g. scaling
`batch` down as `workers` grows) is planned for this pass - `--limit`
already gives users a lever to bound total exposure per invocation if
they want tighter control.

## 4. Testing

Real ONNX-model-dependent detection has no unit test coverage in this codebase
today (would require downloaded model weights - the only existing coverage of
`run_face_pipeline` is `run_face_pipeline_on_empty_input_is_a_noop`, which
short-circuits before any model loading, plus the quality-gate clustering
tests which operate on synthetic embeddings, not real images). This spec
follows the same constraint: the actual concurrent ONNX pipeline is verified
by building + running it against a real library (per Phase 1's checkpoint and
a final manual verification), not by a new integration test that would need
real model weights and real images with real faces in CI.

What **is** unit-testable without any model dependency, and should be:

- **Round-robin partitioning** as a standalone pure function (e.g.
  `fn round_robin_partition<T: Clone>(items: &[T], workers: usize) ->
  Vec<Vec<T>>`) - test that every input item appears in exactly one output
  partition (no duplicates, no gaps), for a few `(item count, worker count)`
  combinations including edges (`workers > items.len()`, `items.len() == 0`,
  `workers == 1`).
- **Result aggregation** as a standalone function operating on a `Vec<WorkerMsg>`
  (or equivalent) rather than a live channel - test that a synthetic mix of
  `FacesFound`/`NoFace`/`ImageError`/`DetectError`/`EmbedBatchError` messages
  produces the correct `FacesRunResult` (`total_faces`, `images_processed`,
  `detect_errors`, `write_errors`), independent of any real detection/
  embedding.
- **Existing tests must keep passing unchanged**
  (`run_face_pipeline_on_empty_input_is_a_noop`,
  `run_clustering_on_empty_db_does_not_error`, the quality-gate tests) - the
  public `run_face_pipeline` signature changes only by gaining new parameters
  (`workers: usize`, `profile: Option<&mut ProfileStats>` or similar), not by
  changing existing behavior for a single-worker/no-profile call.

## 5. Non-goals (explicitly out of scope for this spec)

- HEIC render-size cap (`qlmanage -s 10000` -> smaller), JPEG scaled decoding,
  and the pixel-packing loop micro-optimization - each identified as a
  separate, smaller, independent lever; each gets its own follow-up spec/plan
  if pursued.
- Adding `--workers`/`--profile` to `videre watch`'s faces stage - the
  underlying `run_face_pipeline` signature change makes this trivial later,
  but it's not part of this spec's scope.
- Any change to the detection/embedding models themselves, the clustering
  algorithm, or the CLI's `--eps`/`--min-cluster-size`/`--merge-sim`/
  `--min-face-size`/`--max-generic-sim` quality-gate parameters.
