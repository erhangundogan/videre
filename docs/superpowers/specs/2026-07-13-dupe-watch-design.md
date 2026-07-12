# `dupe-watch`: Background Pipeline Populator Design

## Goal

Add a new binary, `dupe-watch`, that periodically re-scans a photo directory
and incrementally runs the scan/faces/HEIC/location pipeline stages in the
background - so that by the time someone opens `dupe-report --by-date
--show-faces`, everything is already computed and the lightbox never has to
wait on a live conversion or lookup.

## Background

Recent work on `--show-faces` (server mode) revealed a real cost model
problem: the live report was doing expensive work synchronously per request
or per lightbox-open - HEIC-to-JPEG conversion via `qlmanage` (fixed by
making it lazy/on-demand through `/api/raw`, but still pays a real per-file
cost the first time each thumbnail is requested), and reverse-geocoding via
`/api/location` (also lazy, cached into `location_name` after first lookup).
Both of these "pay on first access" fixes are correct for a live server, but
they mean the *first* view of any given photo/location is always slow.
`dupe-watch` removes that first-access latency entirely by doing this work
ahead of time, on a schedule, in a separate process the user never has to
think about while browsing the report.

## CLI

```
dupe-watch <directory> --output-sqlite <db> [--scan] [--faces] [--heic] [--location] [--interval <seconds>]
```

- `directory`: same positional argument `dupe` already takes
- `--output-sqlite <db>`: same flag name/meaning as `dupe --output-sqlite`
- `--scan` / `--faces` / `--heic` / `--location`: enable that stage for this
  run. **If none are passed, all four run** - the common case ("just keep
  everything up to date") shouldn't require memorizing four flags.
- `--interval <seconds>` (default 300): how often to re-run the enabled
  stages. Runs once immediately on startup, then sleeps for `interval`
  between cycles.
- Foreground process, stopped with Ctrl-C - no daemonization, no PID file,
  no subcommands. Same operating model as `dupe-report --faces` today: the
  user runs it in a terminal, tmux pane, or their own `launchd`/`systemd`
  unit if they want it to survive logout, but `dupe-watch` itself doesn't
  manage any of that.

## Stages

Each stage is independently idempotent: it only does work for rows/files
that don't already have the corresponding result, so interrupting
`dupe-watch` mid-cycle (Ctrl-C, crash, machine sleep) and resuming later is
always safe - the next cycle just picks up wherever the previous one left
off.

### `--scan`

Re-runs the existing scan+hash+EXIF pipeline (walk the directory, BLAKE3
hash, extract EXIF/GPS/dimensions, upsert into `file_hashes`) against
whatever's changed since the last cycle. This is the one stage every other
stage depends on, since faces/heic/location all key off rows already present
in `file_hashes`.

**Requires extracting this pipeline into a callable function.** Today it
only exists as logic invoked from `dupe`'s own `main()` - `dupe-watch` needs
to call the same code in-process, not shell out to the `dupe` binary as a
subprocess (per the "call as library" decision below).

### `--faces`

Runs face detection/embedding/clustering for any hash not yet present in the
`faces` table - exactly the same "process new hashes only" mode
`dupe-faces` already supports (the default mode when run without
`--reprocess`).

**Requires extracting this incremental path into a callable function** from
`dupe-faces`'s `main()`, for the same in-process reuse reason as `--scan`.

### `--heic`

For every HEIC file whose hash doesn't yet have a cached thumbnail, converts
it via the existing `qlmanage`-based `heic_via_quicklook` helper and writes
two JPEG files to the cache directory (see below): a 240px thumbnail and a
1200px lightbox-size version - matching the two sizes `--heic`/
`--heic-original` already produce in static mode, and the two sizes
`/api/raw?size=` already requests in server mode.

### `--location`

For every distinct `(gps_lat, gps_lon)` pair in `file_hashes` that doesn't
yet have a `location_name`, resolves it via the existing
`dupe_core::location::location_name()` function and writes it back - the
same column/migration `/api/location` already uses, just populated ahead of
time instead of lazily on first lightbox-open.

## Thumbnail cache

Cached HEIC thumbnails are written to `~/.cache/dupe/thumbnails/<hash>_<size>.jpg`
(e.g. `~/.cache/dupe/thumbnails/70e67b1c.../240.jpg`), keyed by content hash
rather than file path - matching this project's existing `~/.cache/ort/`
convention for cached model weights, and meaning the same photo scanned into
multiple different databases only needs converting once.

**`dupe-report`'s `/api/raw` endpoint gains a cache-check**: before falling
back to live per-request `qlmanage` conversion (the fix from the previous
session), it first checks whether `~/.cache/dupe/thumbnails/<hash>_<size>.jpg`
already exists and serves that directly if so. A folder `dupe-watch` has
already processed never hits the live-conversion path at all; a folder it
hasn't gets today's lazy-conversion behavior as a fallback, so
`--show-faces` works correctly with or without `dupe-watch` running.

## Concurrency: WAL mode

`dupe-watch` (writing) and a running `dupe-report --show-faces`/`--faces`
server (reading and occasionally writing, e.g. face label assignment) will
hold two separate connections to the same SQLite file at the same time.
SQLite's default rollback-journal mode only allows one writer and blocks
readers during a write, which risks "database is locked" errors under this
two-process access pattern.

**Fix: switch to `PRAGMA journal_mode=WAL`** the first time any binary opens
a database (a one-time, idempotent migration - WAL mode persists in the
database file itself once set, similar in spirit to the existing
`location_name` column migration). WAL allows one writer plus many
concurrent readers without blocking, which is the standard fix for
multi-process SQLite access and requires no application-level locking logic.

## Implementation shape

- New binary: likely `crates/dupe/src/bin/dupe-watch.rs` for the scan/heic/
  location stages (same crate as `dupe`/`dupe-report`, same dependencies
  already available), but the `--faces` stage needs whatever `dupe-ml`
  functions `dupe-faces` already calls (ONNX runtime, face detection/
  embedding models) - so `dupe-watch` likely depends on `dupe-ml` the same
  way `dupe-faces` does.
- Refactor: extract `dupe`'s scan+hash+EXIF pipeline into a function
  callable from both `dupe`'s `main()` and `dupe-watch` (exact location -
  `dupe-core` vs a shared module in the `dupe` crate - is an implementation
  detail for the plan, not this spec).
- Refactor: extract `dupe-faces`'s incremental "new hashes only" path into a
  function callable from both `dupe-faces`'s `main()` and `dupe-watch`.
- New: HEIC thumbnail-cache-writing helper (reuses `heic_via_quicklook`,
  writes to `~/.cache/dupe/thumbnails/`).
- New: location pre-resolution loop (reuses `dupe_core::location::location_name`).
- Modified: `handle_raw_file` in `dupe_report.rs` checks the thumbnail cache
  directory before falling back to live conversion.
- Modified: WAL-mode migration, applied wherever databases are currently
  opened (`dupe`, `dupe-report`, `dupe-faces`, `dupe-watch` itself).

## Testing

- Unit tests for each stage's "what still needs doing" query (e.g. "which
  HEIC hashes lack a cache file", "which coordinates lack a location_name")
  against a fixture database - these are the core idempotency guarantees the
  whole design depends on.
- Integration test: run `dupe-watch` against a fixture directory with
  `--interval` set very low, let it complete one cycle, assert the expected
  rows/cache files exist; run a second cycle after adding a new file and
  assert only the new file gets processed (not a full reprocessing pass).
- Manual/browser verification (per `superpowers:verify`): start `dupe-watch`
  against a real photo folder alongside `dupe-report --show-faces --by-date`,
  confirm thumbnails that were previously slow (HEIC, first-time location
  lookups) are now instant once `dupe-watch` has had a chance to run one
  cycle.

## Out of scope

- No daemonization, PID files, or start/stop/status subcommands - the user
  manages the process themselves (terminal, tmux, or their own OS-level
  service supervisor)
- No real-time filesystem watching (`notify`/inotify/FSEvents) - periodic
  rescan only
- No config file for managing multiple directories/databases from one
  process - one `dupe-watch` instance per collection, same as running `dupe`
  multiple times today
- No changes to `dupe-fix-dates`/`dupe-prune` - those remain separate,
  manually-invoked tools
