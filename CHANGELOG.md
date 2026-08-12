# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Note on pre-1.0 versioning: while the version stays below 1.0.0, the **minor**
number is the compatibility boundary, which is how Cargo reads it. A `0.x.y` to
`0.x.z` change is compatible and safe to pick up with `cargo update`. A change
to `0.x` itself may break your build or require action on your library.

All four crates (`videre`, `videre-core`, `videre-api`, `videre-ml`) share a
version number and are released together.

## [0.13.1] - 2026-08-12

### Changed

- **`videre scan --similar` now computes perceptual hashes in parallel, and
  shows progress while it does.** The pass ran single-threaded with no output,
  so on a large library it was a long silence indistinguishable from a hang.
  Measured on 700 real files (300 HEIC, 300 JPEG, 60 MOV, 40 PNG, 2.3 GB) on a
  10-core machine: **108s to 15s**. Expect less on an external drive, where
  reading the files rather than decoding them becomes the limit.

  The hashing pass above it was already parallel; only this one was not. It
  became far more noticeable in 0.13.0, when HEIC started taking the QuickLook
  path and joined video in paying a conversion per file.

  Hashes and their order are unchanged: verified end to end on real HEIC and
  video, where the parallel and serial runs produced identical values for all
  700 files.

## [0.13.0] - 2026-08-12

### Added

- **`videre import`**, bringing photos in from another tool. Point it at a
  folder, a library package, or an export and it works out the rest:

  ```bash
  videre import ~/Pictures        # find whatever is in there
  videre import ~/Takeout         # a Google Takeout export
  videre import                   # search the usual places
  ```

- **Google Takeout.** Restores the capture dates Takeout leaves in `.json`
  sidecars rather than in the files. Handles Google's truncated sidecar names
  (`photo.jpg.supplemental-metadata.json` arriving as `photo.jpg.s.json`), `(1)`
  duplicate counters, and `-edited` versions. Uses `photoTakenTime`, never
  `creationTime`, which is the upload date and the most common way other tools
  get this wrong. An ambiguous name applies no date at all, since a wrong date is
  worse than a missing one.

- **Apple Photos and iPhoto.** Reads `originals/`, `Masters/` or `Originals/`
  directly, covering all three layouts Apple has used. Prints a pre-flight
  checklist first, led by "Download Originals to this Mac", because importing
  iCloud placeholders as though they were originals is the one failure here that
  destroys data. Warns when the library's median file size suggests optimised
  storage, and detects a referenced library.

- **Adobe Lightroom.** Reads `.lrcat` to find which folders hold the photos,
  which is the one thing a filesystem scan cannot tell you. The catalog is copied
  before reading, never opened in place. Root folders on disconnected drives are
  reported as offline rather than treated as missing files.

- **A shared location contract** in `videre-core`, used by every source: an
  optional provider database, then known folder layouts, then asking the user.
  The default never opens a provider database. `--originals <dir>` overrides
  every step, so the feature keeps working by hand on the day a vendor changes
  their structure rather than after videre ships a fix. Each run reports which
  step found the files.

### Notes

- Import modifies file timestamps, like `fix-dates`, so it asks for confirmation
  and supports `--dry-run`. Only the modification time changes.
- Import comes **before** `scan`, since dates must be corrected before `scan`
  records them.
- `--into`, for copying into a clean destination tree, is accepted but not yet
  implemented; it exits with a message rather than being silently ignored.

## [Unreleased]

### Fixed

Found by running `videre import` against a real 36GB Google Takeout export and
a real Photos library, rather than against fixtures.

- **Takeout detection missed `Google Photos/`**, the folder people are most
  likely to point at. A real export keeps only album directories there, with
  the sidecars one level inside them, so probing the immediate directory alone
  reported a genuine export as ordinary photos. Detection now looks one level
  down, bounded so it stays a recognition step rather than a deep walk.

- **A permission failure was reported as a missing library.** Layout probing
  goes through `Path::is_dir`, which answers false for "blocked" exactly as it
  does for "absent", so a macOS-protected `.photoslibrary` produced advice
  about Apple changing their structure. It now names Full Disk Access and the
  need to restart the program, which is the actual fix.

- **An empty `originals/` was asserted to be a referenced library.** On disk
  that is indistinguishable from one whose originals iCloud has evicted,
  including when iCloud was switched off with "Remove from Mac". Photos keeps
  displaying every picture from the previews it retained, so nothing looks
  wrong until an original is needed. Both causes are now offered, the one that
  loses data first.

- **A kept-originals backup was not detected at all.** Apple detection required
  `database/Photos.sqlite` beside the originals, so a backup where only
  `originals/` was copied off a library went unrecognised. The `0`-`F` hex
  fan-out is the signature that survives such a copy, and is specific enough
  not to fire on an ordinary folder that merely happens to be called
  `originals`.

- **`videre scan --similar` computed no perceptual hash for HEIC**, silently
  skipping the default iPhone format in near-duplicate detection. `PHASH_MIMES`
  omitted `image/heic` and the underlying `image` crate cannot decode it. HEIC
  now converts through QuickLook exactly as video already did, asking for a
  64px rendition since the hash resizes to 9x8 - measured at 3 seconds per 20
  files against 2 for JPEG. macOS-only, as QuickLook is.


## [0.12.0] - 2026-08-11

### Added

- **`videre search` filters now compose.** `--person`, `--category`, `--location`
  and the new date bounds can be combined freely; each narrows the result set,
  and a text query or `--image` ranks whatever survives. Previously exactly one
  mode could be used per invocation.
- **Date filtering.** `--after` (inclusive) and `--before` (exclusive), or the
  `--date` shorthand accepting `YYYY`, `YYYY-MM` or `YYYY-MM-DD`. Adjacent ranges
  tile without both claiming the boundary instant.
- **`--sort`**, taking a comma-separated `field[:asc|desc]` list over
  `relevance`, `distance`, `date` and `size`. Later fields break ties in earlier
  ones, so `--sort=distance,date` means nearest first and newest first among
  files at the same place. Directions are optional; each field defaults to what
  is usually meant.
- **The MCP `search` tool exposes all of it**, under the same names. The CLI and
  the MCP server now run the identical code path, so results cannot diverge.
- Search results carry the effective date, so a caller can see why a file
  matched.

### Changed

- **`-k` now applies to `--person` and `--category`.** Those modes previously
  returned every match, unordered. They are now truncated like any other search
  and ordered deterministically. Pass a large `-k` for the full set.
- **The MCP `search` tool's rule changed** from "exactly one of query, person or
  image_path" to "at most one ranker, plus any filters, at least one of
  something". Existing single-parameter calls keep working.
- Asking for a sort whose input is absent is now an error rather than a silent
  fallback: `--sort relevance` needs a query or `--image`, `--sort distance`
  needs `--location`.

### Notes

Dates match the EXIF capture date when a file has one, and its modification time
otherwise, so files without EXIF (screenshots, most videos) stay reachable by
date. The trade-off is that results can mix "when taken" with "when the file was
last written"; running `videre fix-dates` makes the two agree.


## [0.11.5] - 2026-08-11

### Added

- **Prebuilt binaries.** Releases now ship ready-to-run archives, so installing
  no longer needs a Rust toolchain or a long compile:

  | platform | archive |
  |---|---|
  | Apple Silicon Mac | `aarch64-apple-darwin` (21 MB) |
  | Linux x86_64 | `x86_64-unknown-linux-gnu` (23 MB) |
  | Linux ARM64 | `aarch64-unknown-linux-gnu` (24 MB) |

  Each has a `.sha256` alongside. Every archive is downloaded onto a clean
  machine and actually run before the release is published, so a binary that
  only works where it was built cannot ship.

### Changed

- The README now leads with the binary download, and notes the macOS Gatekeeper
  quarantine on browser downloads.

### Known limitation

- **Intel Macs are not supported.** The ONNX Runtime dependency ships no
  prebuilt binaries for `x86_64-apple-darwin`, so videre cannot be built for
  that target at all, including via `cargo install`. This was not introduced
  here; it has been true for as long as ONNX Runtime has been a dependency, and
  setting up release builds is simply what surfaced it.

## [0.11.4] - 2026-08-10

### Fixed

- **Documentation corrections.** No code changes; released so the corrected
  README reaches crates.io.
  - The first `videre embed` downloads about **780 MB**, not the 1.4 GB the
    README claimed. That figure was `siglip2-base-patch16-384`, which stopped
    being the default in 0.10.0. `videre faces` downloads a separate 180 MB.
  - Stated explicitly that model downloads are **lazy and per-command**:
    `scan`, `dedupe`, `fix-dates`, `prune`, `stats` and `locations` need no
    model at all.
  - The README now documents `videre prune`'s safety guards, added in 0.11.2
    but never described there: a disconnected drive is left alone,
    `--prune-unreachable` overrides it, and a cleanup removing more than 20%
    of the library stops unless forced.

## [0.11.3] - 2026-08-10

### Changed

- **`videre scan` and `videre watch` now take `--db`**, the same flag every
  other subcommand uses. `--output-sqlite` still works as an alias, so existing
  scripts are unaffected.

  It was called `--output-sqlite` for historical reasons: `scan` predates the
  readers, from when JSONL and SQLite were peer output *formats* rather than one
  destination and one opt-out. `videre scan --db` failing was surprising every
  time.

## [0.11.2] - 2026-08-10

### Fixed

- **`videre prune` no longer deletes your library when a drive is
  disconnected.** It treated every "cannot read this file" as "this file was
  deleted", so pruning with an external drive unplugged removed every row for
  that drive. The rows were the cheap part: their embeddings and cached
  thumbnails were then swept as orphans, which is hours of recompute against
  minutes to re-scan rows. `videre watch --prune` runs the same code unattended
  on a loop.

  A row is now removed only when the file is missing **and its parent directory
  still exists**. A missing parent means the folder or the whole volume is gone,
  so the rows are kept and reported. Pass `--prune-unreachable` when a folder
  really is gone for good.

### Added

- **`videre prune` stops before an implausibly large deletion.** A run removing
  more than 20% of the library and at least 100 rows now refuses and changes
  nothing, so a volume that remounts empty cannot quietly empty the database.
  `--force` proceeds anyway.
- **`videre prune` gives up after 10 consecutive errors** instead of printing
  one near-identical line per row, reporting the first error verbatim. Earlier
  changes stay committed and prune is idempotent, so it is safe to re-run once
  the cause is fixed.
- Skipped rows are reported **even with `--silent`**, naming the missing
  directories, since a run that quietly skips thousands of rows is the problem
  this release exists to fix.

`videre watch --prune` can override neither guard: it runs unattended and cannot
ask.

## [0.11.1] - 2026-08-09

### Added

- **`videre scan --retry-incomplete`** processes only files a previous scan did
  not finish, instead of re-reading the whole library. A full scan of a 70,601
  file library reads roughly 460GB and takes about 10 minutes, so filling a
  handful of stragglers no longer costs a full pass.

### Changed

- A file whose bytes match no known signature now records
  `application/octet-stream` instead of an empty type, so it is not re-read on
  every retry. This does not change how such files are decoded.

## [0.11.0] - 2026-08-09

### Added

- **`videre scan` records each file's real type** in a new `file_hashes.mime`
  column, detected from its magic bytes rather than its filename. Costs no
  extra reading: the bytes are already in memory for hashing. Existing
  libraries gain the column automatically and fill it on the next scan.

### Changed

- **Removed the pre-0.10 fallback that read embeddings from the main
  database.** It was always scheduled for this release. An unmigrated 0.9.x
  library now gets the same clear error as any other missing model, naming
  what does exist and the command to run. Nothing is deleted; the old rows sit
  untouched and can be dropped by hand.

### Fixed

- **Misnamed files are decoded by their real type.** A JPEG named `.png`
  previously failed every `videre embed` run with "Invalid PNG signature"; it
  now embeds correctly. Decoding, perceptual hashing, EXIF extraction, and the
  photo/video split all route on content rather than filename.
- **A partly-created per-model embedding database is repaired** rather than
  attached broken forever. Previously an initialisation cut short by a crash
  or a full disk left a file that every later run skipped, failing with "no
  such table".

## [0.10.1] - 2026-08-09

### Fixed

- **Videos with no video track no longer cost 20 seconds each.** `qlmanage`
  hangs rather than failing on such a file, so `videre embed` and
  `videre scan --similar` waited out the full timeout on every run, forever.
  They are now detected and skipped instantly. Measured on a 70,601-file
  library: 60 seconds saved per `embed` run and the same per `scan --similar`.

## [0.10.0] - 2026-08-07

### Added

- `--model <id>` on `embed`, `search`, `classify`, `report`, and `mcp`, so
  several embedding models can be used against one library and compared.
  Resolution order is `--model`, then `default_model` in `config.toml`, then
  the built-in default.
- `videre config set model <id>` persists a default embedding model, alongside
  the existing `db` and `path` keys.
- `videre stats` reports embeddings per model: count, dimensions, and file
  size. The `--json` output gains a matching `embeddings` array; existing
  fields are unchanged and `schema_version` stays `1`.

### Changed

- **The default search model is now `google/siglip-base-patch16-224`**, about
  twice as fast as the previous default. Each model keeps its own data, so this
  leaves any existing model's vectors intact and reachable with `--model`.
- **`VIDERE_EMBED_MODEL` has been removed.** Use `videre config set model <id>`
  for a lasting default or `--model <id>` for one command. Configuration
  belongs in `config.toml`, not the environment.
- **Embeddings moved out of the main database** into one SQLite file per model
  per library, under `~/.videre/embeddings/`. Vectors were roughly three
  quarters of a real 427 MB library, and a single shared table allowed only one
  model to be usable at a time.
- `videre prune` now sweeps orphan embeddings across every model database and
  reports its counts per model.
- `classifications` is keyed by `(model_id, hash)` rather than `hash` alone.
  This also fixes a real bug: any hash already classified was skipped for every
  model, so a second model would classify nothing while reporting success. A
  table predating this change is reset, with a note; rebuilding it is minutes
  of vector arithmetic with no image decoding.

### Upgrading

**If you embedded on 0.9.23 or later, run one command before searching:**

```bash
videre config set model google/siglip2-base-patch16-384
```

Embeddings still in your main database are read as before, but only for the
model that produced them, and the default model changed in this release. Your
existing vectors are tagged `siglip2-base-patch16-384`, so without that setting
a search asks for `siglip-base-patch16-224` and finds nothing. `videre search`
tells you so rather than returning an empty result quietly.

Alternatively, run `videre embed` to build the new default's vectors, which
takes roughly an hour for 70,000 photos. Either way, nothing is lost or
overwritten: models keep separate data.

The read fallback for pre-0.10 embeddings is removed in 0.11.0.

If you depend on these crates as libraries rather than using the CLI, note that
`videre-core`'s `ensure_embeddings_table` is gone, `insert_classifications` and
`paths_for_category` take a `model_id`, `resolve_model_id` returns
`anyhow::Result<String>` rather than `String` so a malformed `config.toml` is
an error rather than a silent fallback, and `videre-ml`'s `Embedder::load`
takes the model id explicitly instead of reading it from the environment.

## [0.9.29] - 2026-08-05

### Changed

- Removed dashes used as punctuation throughout code comments and docs.

## [0.9.28] - 2026-08-05

### Added

- Per-command reference in the README covering every flag.

## [0.9.27] - 2026-08-04

### Changed

- Restored a plain-language "Why videre" section to the README and folded the
  technical detail into `CLAUDE.md`.

## [0.9.26] - 2026-08-04

### Changed

- Rewrote the README for readers who are not Rust developers.

## [0.9.25] - 2026-08-04

### Added

- Environment variables promoted to a top-level README section.

## [0.9.24] - 2026-08-04

### Added

- Documented `VIDERE_EMBED_MODEL` and `VIDERE_EMBED_DTYPE`.

## [0.9.23] - 2026-08-04

### Changed

- **Default embedding model is now `google/siglip2-base-patch16-384`**, 3.6x
  faster than the previous `siglip-so400m-patch14-384`: 131ms per photo against
  479ms, taking a 70,000 photo library from roughly 9.4 hours to 2.6. A blind
  comparison of 14 searches showed no quality advantage for the slower model.

  Embeddings are tagged with the model that produced them, so this invalidates
  existing ones and requires a full re-run of `videre embed`. Set
  `VIDERE_EMBED_MODEL=google/siglip-so400m-patch14-384` to stay on the old
  model.

## [0.9.22] - 2026-08-04

### Added

- `VIDERE_EMBED_DTYPE=f16` for half-precision inference, roughly 7% faster on a
  realistic mix. Opt-in, with no measurable quality change.

## [0.9.21] - 2026-08-04

### Fixed

- **`videre embed --batch` is capped at 96.** Above a threshold measured
  between 121 and 127, the batched inference path silently returned embeddings
  that did not match a one-at-a-time baseline: no error, no NaN values, just
  wrong vectors, with only the trailing partial batch correct. Values above the
  cap are reduced with a warning.

## [0.9.20] - 2026-08-03

### Changed

- Lock files moved from beside the database to `~/.videre/locks/`, keyed by a
  hash of the database's canonical path so two libraries with the same filename
  no longer share a lock. Stale files from the old layout are cleaned up
  automatically on the first run.

## [0.9.19] - 2026-08-03

### Changed

- README updates.

## [0.9.18] - 2026-08-03

### Added

- Near-duplicate detection for `.mov` and `.mp4` under `scan --similar`, using
  a perceptual hash of the QuickLook poster frame. macOS only; other platforms
  skip it rather than failing.
- First release published to crates.io.

[Unreleased]: https://github.com/erhangundogan/videre/compare/v0.12.0...HEAD
[0.12.0]: https://github.com/erhangundogan/videre/compare/v0.11.5...v0.12.0
[0.11.5]: https://github.com/erhangundogan/videre/compare/v0.11.4...v0.11.5
[0.11.4]: https://github.com/erhangundogan/videre/compare/v0.11.3...v0.11.4
[0.11.3]: https://github.com/erhangundogan/videre/compare/v0.11.2...v0.11.3
[0.11.2]: https://github.com/erhangundogan/videre/compare/v0.11.1...v0.11.2
[0.11.1]: https://github.com/erhangundogan/videre/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/erhangundogan/videre/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/erhangundogan/videre/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/erhangundogan/videre/compare/v0.9.29...v0.10.0
[0.9.29]: https://github.com/erhangundogan/videre/compare/v0.9.28...v0.9.29
[0.9.28]: https://github.com/erhangundogan/videre/compare/v0.9.27...v0.9.28
[0.9.27]: https://github.com/erhangundogan/videre/compare/v0.9.26...v0.9.27
[0.9.26]: https://github.com/erhangundogan/videre/compare/v0.9.25...v0.9.26
[0.9.25]: https://github.com/erhangundogan/videre/compare/v0.9.24...v0.9.25
[0.9.24]: https://github.com/erhangundogan/videre/compare/v0.9.23...v0.9.24
[0.9.23]: https://github.com/erhangundogan/videre/compare/v0.9.22...v0.9.23
[0.9.22]: https://github.com/erhangundogan/videre/compare/v0.9.21...v0.9.22
[0.9.21]: https://github.com/erhangundogan/videre/compare/v0.9.20...v0.9.21
[0.9.20]: https://github.com/erhangundogan/videre/compare/v0.9.19...v0.9.20
[0.9.19]: https://github.com/erhangundogan/videre/compare/v0.9.18...v0.9.19
[0.9.18]: https://github.com/erhangundogan/videre/releases/tag/v0.9.18
