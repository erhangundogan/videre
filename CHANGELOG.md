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

## [Unreleased]

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

[Unreleased]: https://github.com/erhangundogan/videre/compare/v0.10.0...HEAD
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
