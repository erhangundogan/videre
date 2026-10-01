---
title: videre config
description: Show or edit settings for one directory-local library.
---

Shows or changes settings for the selected library. With no `--library`
option, the current directory is the library.

```bash
cd ~/Photos
videre config
videre config set model google/siglip2-base-patch16-224
videre config set read-rate 10
videre config set io-workers 64
videre config set xmp file
videre config set export-xmp-on-watch true
videre config set watch-debounce-ms 500
videre config set watch-bulk-threshold 500
videre config set log-level debug
videre config unset model
```

Select another library explicitly when needed:

```bash
videre --library ~/Pictures config
videre config --library ~/Pictures set xmp db
```

## Reading the output

```text
library:       /Users/you/Photos
selected by:   invocation directory
state:         /Users/you/Photos/.videre
config:        /Users/you/Photos/.videre/config.toml
db:            /Users/you/Photos/.videre/hashes.db
jsonl:         /Users/you/Photos/.videre/hashes.jsonl
model:         google/siglip2-base-patch16-224
read-rate:     20 MB/s (default)
io-workers:    40 (default)
xmp:           db
export-xmp-on-watch: off
watch-debounce-ms: 1500 ms (default)
watch-bulk-threshold: 1000 files (default)
watch-bulk-quiet-ms: 30000 ms (default)
log-level:     warn
log-format:    json
log-max-size-mb: 10 MB
log-keep:      5
log-max-age-days: 30 days
search-min-match: 0.1 (default)
similar-min-score: none (default)
```

| Line | Meaning |
|---|---|
| `library` | Canonical root of the selected library |
| `selected by` | Whether the root came from `--library` or the invocation directory |
| `state` | Reserved local state directory |
| `config` | Local config file, marked absent when it does not exist |
| `db` | Fixed SQLite database path |
| `jsonl` | Fixed JSONL snapshot path |
| `model` | Effective embedding and search model |
| `read-rate` | Assumed minimum read speed used for large-file timeouts |
| `io-workers` | Maximum concurrent helper I/O workers in this process |
| `xmp` | Effective XMP precedence for ingest |
| `export-xmp-on-watch` | Whether watch exports XMP each cycle |
| `gallery-starts-watch` | Whether the gallery starts `videre watch` beside it |
| `watch-debounce-ms` | How long watch lets file changes settle before processing |
| `watch-bulk-threshold` | Files waiting at which watch switches to bulk mode |
| `watch-bulk-quiet-ms` | How long a bulk import must go quiet before watch finishes it |
| `log-level` | What the per-command log files record |
| `log-format` | How log lines are written |
| `log-max-size-mb` | Size at which a log file rotates |
| `log-keep` | Rotated log files kept per log file |
| `log-max-age-days` | Age after which a rotated log file is deleted |
| `search-min-match` | The least match a text search keeps |
| `similar-min-score` | The least similarity an image search keeps |

Showing config creates nothing. Setting a value may create
`.videre/config.toml`, but it does not create the database. `scan` and `watch`
are responsible for initializing a library database.

## Settings

| Command key | TOML key | Accepted value |
|---|---|---|
| `model` | `default_model` | A supported `owner/model` identifier |
| `read-rate` | `min_read_rate_mb_s` | A positive whole number in MB/s |
| `io-workers` | `max_io_workers` | A whole number from 1 through 256 |
| `xmp` | `xmp_precedence` | `db`, `file`, or `newest` |
| `export-xmp-on-watch` | `export_xmp_on_watch` | `true` or `false` |
| `gallery-starts-watch` | `gallery_starts_watch` | `true` (default) or `false` |
| `watch-debounce-ms` | `watch_debounce_ms` | A positive whole number of milliseconds |
| `watch-bulk-threshold` | `watch_bulk_threshold` | A positive whole number of files (default 1000) |
| `watch-bulk-quiet-ms` | `watch_bulk_quiet_ms` | A positive whole number of milliseconds (default 30000) |
| `log-level` | `log_level` | `error`, `warn` (default), `info`, or `debug`; `info` and `debug` add a trace file |
| `log-format` | `log_format` | `json` (default) or `text` |
| `log-max-size-mb` | `log_max_size_mb` | A positive whole number of megabytes (default 10) |
| `log-keep` | `log_keep` | A whole number of rotated files, 0 or more (default 5) |
| `log-max-age-days` | `log_max_age_days` | A positive whole number of days (default 30) |
| `search-min-match` | `search_min_match` | A match probability from 0 to 1 (default 0.1); [search](/commands/search/) drops weaker text matches, and `0` keeps every one |
| `similar-min-score` | `similar_min_score` | A similarity from -1 to 1, or unset (the default) to keep every result of an image search or **Similar** |

Storage cannot be redirected. `db` and `jsonl` are fixed declarations and
must remain `hashes.db` and `hashes.jsonl`. The removed global `path` and `db`
settings are not accepted by `videre config`.

A new local config contains the fixed storage declarations and built-in
defaults:

```toml
db = "hashes.db"
jsonl = "hashes.jsonl"
default_model = "google/siglip2-base-patch16-224"
xmp_precedence = "db"
export_xmp_on_watch = false
watch_debounce_ms = 1500
```

Setting or unsetting one key preserves the others and any unknown tables.

`io-workers` limits helper threads that may remain blocked on an unresponsive
drive. The built-in default is four times the reported CPU count, with a floor
of 32 and a ceiling of 128, so the displayed default varies by computer. A
worker keeps its slot until it actually exits, even when its caller has timed
out. The limit applies to this videre process, not other running processes.

`watch-debounce-ms` is how long [`videre watch`](/commands/watch/) lets file
changes settle before processing them as one batch. Lower it for the fastest
reaction to a single file; raise it on a chatty importer or a slow mount so
more of a burst lands in one batch.

`watch-bulk-threshold` and `watch-bulk-quiet-ms` shape how watch handles a
large import; see [bulk imports](/commands/watch/#bulk-imports).

The `log-*` keys control the per-command log files under `.videre/logs/`.
They never change what the terminal shows. See
[Logging and error handling](/guides/logging-and-errors/) for what each level
records and how to read the files.

## Selection and precedence

The library root is selected in this order:

1. `--library DIR`
2. The directory where videre was invoked

Within that library, a configurable behavior is selected in this order:

1. A command option such as `--model` or `--xmp`
2. The value in `.videre/config.toml`
3. The built-in default

There is no process-wide library home, saved default path, or saved default
database. Each library carries its own state and settings.
