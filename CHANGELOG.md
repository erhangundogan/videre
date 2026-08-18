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

## [0.16.0] - 2026-08-19

### Fixed

- **`alice` and `Alice` were two different people.** A different capitalisation
  silently split someone in two, so person search returned half their photos
  with nothing on screen to explain it. A person now has an identity that
  ignores case, accents and spacing, and a separate display name that keeps
  exactly what you typed. `--person alice`, `--person Alice` and
  `--person "Ahmet Arı"` all find the same person.

  **Existing libraries are migrated automatically** the first time a command
  that writes faces runs, keeping your original spelling as the display name.
  Nothing is lost, and names that differed only in case are merged into one
  person.

### Added

- **The labeling UI says when a name will join an existing person**, instead of
  merging silently. Typing a name that already exists now shows
  `adds to Ahmet Arı, 247 face(s)` and the button reads `Add to Ahmet Arı`. The
  New Person box also gained the autocomplete the other name boxes already had.

- **A person's display name can be edited** without changing their identity, so
  adding a surname does not break the URL or touch a single photo.

## [0.15.10] - 2026-08-18

### Added

- **`videre stats` now reports what videre is using on disk**, largest first,
  marking which stores are safe to delete because they rebuild. Thumbnails and
  place names regenerate; the database and embeddings do not.
- **`videre stats` now shows what the library is made of**, by file extension
  with the mime type alongside. These are the values `--ext` and `--mime` take.

### Changed

- **Durations are readable.** `stats` printed raw milliseconds, so a run of an
  hour and a half showed as `5412000ms`. Elapsed times across `stats`, `embed`,
  `classify` and `faces` now read `2h 14m`, `41s`, `3.2s`, `840ms`.

### Fixed

- Embeddings are counted per library rather than per home. Other libraries
  sharing a `VIDERE_HOME` are reported separately instead of being added to
  whichever one you asked about.

## [0.15.9] - 2026-08-18

### Fixed

- **The face-labeling UI reshuffled its lists on every reload.** People and
  unassigned clusters came back in a different arbitrary order each time the
  page fetched them, so after assigning one cluster the next one you had lined
  up had moved, and so had the person you were dragging it onto. People are now
  ordered by name, and clusters largest first.

## [0.15.8] - 2026-08-18

### Fixed

- **Two different people could end up in one face group.** After clustering,
  videre merges groups whose average embeddings point in a similar direction,
  and that test alone is too weak: two tight groups that sit far apart can still
  have similar averages. Merging now also requires the groups to be close
  overall. On a real library this separated every mixed group among the fourteen
  largest, at the default settings, and left all labels intact.

- **`videre faces --help` described `--merge-sim` backwards.** It said `0 =
  identical direction required`; 0 in fact merges everything into one group.
  Read literally it sent you the wrong way when two people had merged. The docs
  page always had this right.

## [0.15.7] - 2026-08-18

### Changed

- **`embed`, `classify` and `faces` now word their progress and "nothing to do"
  lines the same way.** All three previously counted different things in
  different words - "pending file(s)", "pending hash(es)", "eligible file(s)" -
  for what is one idea; they all say `pending item(s)` now. If you grep videre's
  stderr in a script, this is the change that affects you.

### Internal

- The three commands share one implementation of "work out what is pending,
  narrow it by the selection, and stop if nothing is left". The model load now
  sits inside that shared path, so a command cannot reach it with nothing to
  process. A wrong copy of that guard is what let a unit test download 778MB of
  model weights.

## [0.15.6] - 2026-08-18

### Fixed

- **A city's administrative sector could be named instead of the city.** A
  Bucharest cluster was labelled `Sector 1, RO`. GeoNames lists such numbered
  slices as populated places, and one can sit closer to your photos than the
  city's own entry. Twelve of them are now excluded, so that cluster reads
  `Bucharest, RO`. Requires a `videre locations` re-run to take effect.

## [0.15.5] - 2026-08-18

### Fixed

- **Place names lost their accents: `Üsküdar, TR` appeared as `UEskuedar, TR`.**
  The offline reverse geocoder read GeoNames' `asciiname` column, which is ASCII
  by definition and expands umlauts German-style. videre now ships its own
  GeoNames extract built from the real `name` column, so `Üsküdar`, `Malmö` and
  `Hôtel-de-Ville` come out as written. 21% of the 170,761 place names in the
  dataset contain a non-ASCII character, so this affects far more than one
  language.

  Existing libraries keep the old names until `videre locations` is re-run:
  location names are resolved when they are written, not when they are shown.

## [0.15.4] - 2026-08-18

### Fixed

- **`videre locations` took seven minutes and looked frozen.** The recompute
  runs one UPDATE per distinct coordinate, and that UPDATE matched
  `ROUND(gps_lat, 6) = ROUND(?, 6)` - a function call on a column, so no index
  could be used, and there was no index on the GPS columns anyway. Each of
  26,744 updates scanned all 70,601 rows. It now matches exactly against a new
  index: **412s to 86s** on that library.

- **Location `photo_count` counted some photos twice.** The same `ROUND`
  over-matched, so two coordinates differing past the sixth decimal both
  claimed the same photo. 181 photos were double-counted on a real library,
  179 of them in one city.

### Changed

- **`videre locations` reports progress.** It previously printed nothing until
  the final list, which for a multi-minute recompute is indistinguishable from
  a hang. Both slow phases now report: the distance matrix build (which at
  26,744 coordinates allocates ~5.3GB before any work) and the per-coordinate
  assignment. Suppressed under `--silent`, `--json` and `--geojson`.

- Progress output counts what a command actually processes. The non-TTY line
  said "images processed" for every command, including ones counting
  coordinates.

### Internal

- Dependencies are now optimized in dev and test builds. `cargo test` is a
  debug build, where one CPU inference pass measured 9.41s against 0.168s in
  release; a CPU batch-correctness test consequently ran for 28 minutes on CI.
  Workspace crates stay unoptimized and keep compiling quickly.

- The argument-robustness tests no longer download model weights (777MB) as a
  side effect of exercising `videre embed`.

## [0.15.3] - 2026-08-16

### Fixed

- **`videre scan --output <directory>` scanned the wrong library.** `--output`
  takes an optional value, so `videre scan --output ~/Photos` bound `~/Photos`
  as the output *file* and left the directory unset, which then fell back to the
  configured `default_path`. The scan ran end to end against a different library
  and only the final write failed.

  The severity is the near-miss: had the swallowed path been an existing *file*
  rather than a directory, the write would have **succeeded**, putting one
  library's records into it with no error at all. It now fails before walking
  anything, with a message naming both the cause and the fix.

### Testing

- **An argument-robustness sweep** across all fifteen subcommands: unknown
  flags, flags a command cannot answer, conflicting pairs, degenerate values,
  hostile strings and odd-but-harmless input. It asserts the weakest useful
  property - no panic, ever - rather than pinning messages, and separately
  asserts the library survives SQL-shaped input, which a `format!`-built query
  would fail while passing everything else.

  72 nonsense invocations were run by hand first: zero panics, zero hangs, and
  the database intact after every injection attempt.

- **Coverage on the two least-tested commands.** `classify.rs` 4.7% -> 78.9%,
  `watch.rs` 7.5% -> 38.2%, workspace 82.5% -> 83.8%. Both are where the
  selection layer was wired in most recently, so the newest logic had been
  sitting in the least-tested files.

- `--mime` had no coverage anywhere in the suite and `--path` had one use; both
  are axes 0.15.0 added, and both bugs that shipped in that release were here.

### Changed

- `make test` now passes `--no-fail-fast`, matching CI. Without it one failing
  test binary hides every later one, so `make verify` was no longer "what CI
  gates on".

## [0.15.2] - 2026-08-16

### Fixed

- **`--model` with a malformed id panicked instead of erroring.**
  `videre embed --model foo` aborted with `thread 'main' panicked ... model id
  is owner/name`. A validator existed but lived in the `config` command and so
  guarded only `videre config set`; the flag reached the loader's `expect`
  directly. An invariant enforced on one of two entrances to the same function
  is not enforced.

  Validation now lives in `videre-core` beside the resolver, so both the flag
  and a hand-edited `config.toml` go through it, and `--model` is additionally
  checked when the argument is parsed - a typo now fails immediately rather than
  after an unrelated "no database found".

- **`videre faces --batch 0` panicked.** It reached `slice::chunks(0)`. `embed`
  had guarded exactly this and `faces` had not, so the guard moved next to
  `MAX_SAFE_BATCH` where both commands reach it. The upper cap stays specific to
  embedding: it exists because that inference path silently corrupts embeddings
  above roughly 121, which is not a fact about face detection, so `faces` takes
  the zero-guard only.

- **The read-timeout handler could hang on the drive it was reporting about.**
  After a timeout, `hash_file` called `std::fs::metadata` **unbounded** on the
  same path, purely to name the timeout in its message. On a stale mount that is
  the call that never returns, so the error path blocked in precisely the
  scenario the timeout exists to survive.

  The applied timeout now comes back with the failure, so the message needs no
  filesystem access. It also names the phase: a dead drive reports that the
  `stat` never answered, rather than claiming a read took 20 seconds when
  nothing was ever read.

## [0.15.1] - 2026-08-13

### Documentation

- **The scoping filters are documented on the pages people actually read.**
  0.15.0 added `--type`, `--ext`, `--mime` and `--path` to `videre search`, but
  neither the search page nor the compositional-search guide mentioned them, so
  the four newest filters were discoverable only from `--help`.

- Worked, end-to-end examples for composing filters: staging a large library
  through `embed` in slices, bounding `faces` to one trip, re-labelling a single
  folder with `classify`, and watching only an inbox. Every example in the docs
  was verified to parse against the real binary.

### Fixed

- Two compiler warnings that only ever appeared during `cargo publish`, which is
  the one build in the pipeline that compiles from a clean target directory and
  so re-emits what a cached local build stays silent about. `apply_location` in
  `search.rs` was left unused when the selection layer took over location
  filtering; a closure in `selection.rs` was needlessly `mut`.

## [0.15.0] - 2026-08-13

### Added

- **Scoping filters on the long-running commands.** `videre embed`, `faces`,
  `classify`, `scan` and `watch` now take the same filter flags `videre search`
  has, so a run can cover part of a library instead of all of it: `videre embed
  --type video`, `videre faces --date 2024-07`, `videre scan ~/Photos --path
  ~/Photos/2024`. On a large library this is the difference between an
  afternoon and a few minutes.

  `--type`, `--ext` and `--mime` are repeatable and accept comma-separated
  lists. Combining flags narrows further: every condition must hold.

- **`videre search` gained `--type`, `--ext`, `--mime` and `--path`**, the same
  four the walk-based commands take, and the MCP `search` tool gained the
  matching parameters so an agent can ask for them too.

- **A [scoping guide](https://docs.videre.sh/guides/scoping-a-run/)** covering
  the shared vocabulary, which commands accept which flags and why the gaps
  exist, and the rule that a file missing the data a filter needs is excluded
  rather than assumed.

### Changed

- **Every filter now lives in one place** (`videre_core::selection`) rather than
  being reimplemented per command. `search` and the MCP server already shared
  their predicates; that sharing now extends to every command that filters, so
  a fix to a predicate reaches all of them at once.

- **A scoped run reports what it passed over**, as `412 of 70,601`. A filter
  that matches nothing is not an error, so without the denominator a wrong
  filter and an empty library look identical.

- **`videre search` reports truncation.** Showing 20 results out of thousands
  with nothing to say so was indistinguishable from having found only 20. The
  JSON output gained `total_matches`.

### Fixed

- **`--path` under a symlink matched nothing, on both surfaces.** Each root was
  replaced by its canonical form while the other side of the comparison was not
  canonicalised: the walk yields paths rooted where the user pointed it, and a
  stored row holds whatever path the scan recorded. So `--path` under a
  symlinked directory silently selected nothing and reported success. On Linux
  that included `/lib`, which resolves to `/usr/lib`; on macOS, anything under
  `/tmp` or `/var`.

  Both forms of each root are now matched, from one shared helper, so the walk
  side and the row side cannot drift apart again. Per-row canonicalisation would
  cost a stat per row and is deliberately not done.

### Notes

- `videre locations` deliberately takes no filters. It rebuilds every cluster
  from scratch per run, so a scoped run would not do less work, it would leave
  everything outside the scope unclustered.
- `videre embed` and `videre faces` deliberately omit `--person` and
  `--category`. Both are derived from the data those commands produce, so
  selecting their input by one is circular.

## [0.14.1] - 2026-08-13

### Fixed

- **`VIDERE_HOME` now outranks a config file's `default_db`.** Setting
  `VIDERE_HOME` and then having the run write to a different database was
  possible, and did happen: a scan aimed at one home wrote 70,601 records into
  another, because that home's `config.toml` named an absolute `default_db`.

  Environment variables are expected to beat persisted settings, and this one
  did not. The effect was worse than a surprise, because it silently defeats the
  isolation `VIDERE_HOME` exists to provide: a copied home carries the
  original's absolute path, so pointing `VIDERE_HOME` at a copy still wrote into
  the source.

  Precedence is now `--db` > `VIDERE_HOME` > `config.toml` > built-in default.
  When the environment variable and the config disagree, videre says which it is
  using rather than choosing silently, so a deliberate `default_db` is not
  quietly ignored either.

  Nothing changes for anyone not setting `VIDERE_HOME`: `videre config set db`
  behaves exactly as before.

## [0.14.0] - 2026-08-13

### Added

- **Video now carries dates, locations, dimensions, duration and codec.**
  Until now videre read no metadata from video at all: the extractor covered
  jpeg, tiff and heic only, so every `.mov` and `.mp4` was stored with no date
  and no coordinates. Measured on a real library, that was **13,457 videos, 0
  dates, 0 GPS**.

  The effect was wider than two empty columns. Every feature keyed on date or
  place silently excluded video - `--after`/`--before`, `--near`,
  `videre locations` and any query combining them - which on that library meant
  19% of the files were missing from results that presented themselves as
  covering the whole library, with nothing to indicate it.

  Parsing is in-house, walking the container's boxes the way the existing video
  probe already does, so there is no new dependency and no external process.

  Two new columns, `duration_secs` and `codec`. They are added in the same
  release deliberately: existing rows cannot pick any of this up incrementally,
  since `--retry-incomplete` looks for rows with no recorded type and these are
  not that, so one full re-scan is needed either way. Adding them later would
  have required a second one.

  :warning: **Libraries scanned before this release need `videre scan` run
  again** to populate the new fields.

### Notes

- Dates from video are stored as **local wall-clock time**, matching how photo
  dates are stored, so the two sort and filter together. Apple's containers
  record both a local time and a UTC one; the local is preferred, and the UTC
  fallback is used only for files carrying nothing else (10 of 260 on the test
  corpus, all re-encoded renders rather than camera originals).
- Verified against `ffprobe` on 260 real videos: **0 mismatches** across date,
  coordinates and duration.

## [0.13.2] - 2026-08-12

### Fixed

- **Large files are no longer skipped as "unreachable".** `videre scan` bounded
  every file read with a flat 20-second timeout, so a big file on a healthy
  drive was indistinguishable from a hung one and was dropped with a message
  blaming the drive. Measured on a real library: a 3.7 GB video on a drive
  sustaining 158 MB/s needs about 23 seconds to read. File sizes do not change,
  so the same files were skipped on every run, and they are by definition the
  longest videos in a library. No row was written at all, so they were simply
  absent from the database.

  The read timeout now scales with file size, never dropping below the previous
  20 seconds, so small files are unaffected. The `stat` that reads the size is
  bounded separately by a short constant, and that ordering is the safety
  property: a disconnected or stale mount still fails there in about five
  seconds and the read is never attempted, so a large file on a dead mount
  cannot hang for its scaled timeout.

  This applies to whole-file reads only. Decoding is left alone: a QuickLook
  poster frame reads a fraction of a video, so scaling by full file size would
  turn a known QuickLook hang from 20 seconds into minutes.

### Added

- **`videre config set read-rate <MB/s>`**, the assumed floor read rate used to
  scale that timeout. Defaults to 20 MB/s, which is far under real hardware and
  only worth changing for a slower mount. Zero is rejected: as a read rate it
  means an unbounded timeout, which is the hang the mechanism exists to prevent.

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

[Unreleased]: https://github.com/erhangundogan/videre/compare/v0.16.0...HEAD
[0.16.0]: https://github.com/erhangundogan/videre/compare/v0.15.10...v0.16.0
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
