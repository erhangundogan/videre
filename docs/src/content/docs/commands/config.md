---
title: videre config
description: Show or edit settings for one directory-local library.
---

Shows or changes settings for the selected library. With no `--library`
option, the current directory is the library.

```bash
cd ~/Photos
videre config
videre config set model google/siglip-base-patch16-224
videre config set read-rate 10
videre config set xmp file
videre config set export-xmp-on-watch true
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
model:         google/siglip-base-patch16-224
read-rate:     20 MB/s (default)
xmp:           db
export-xmp-on-watch: off
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
| `xmp` | Effective XMP precedence for ingest |
| `export-xmp-on-watch` | Whether watch exports XMP each cycle |

Showing config creates nothing. Setting a value may create
`.videre/config.toml`, but it does not create the database. `scan` and `watch`
are responsible for initializing a library database.

## Settings

| Command key | TOML key | Accepted value |
|---|---|---|
| `model` | `default_model` | A supported `owner/model` identifier |
| `read-rate` | `min_read_rate_mb_s` | A positive whole number in MB/s |
| `xmp` | `xmp_precedence` | `db`, `file`, or `newest` |
| `export-xmp-on-watch` | `export_xmp_on_watch` | `true` or `false` |

Storage cannot be redirected. `db` and `jsonl` are fixed declarations and
must remain `hashes.db` and `hashes.jsonl`. The removed global `path` and `db`
settings are not accepted by `videre config`.

A new local config contains the fixed storage declarations and built-in
defaults:

```toml
db = "hashes.db"
jsonl = "hashes.jsonl"
default_model = "google/siglip-base-patch16-224"
xmp_precedence = "db"
export_xmp_on_watch = false
```

Setting or unsetting one key preserves the others and any unknown tables.

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
