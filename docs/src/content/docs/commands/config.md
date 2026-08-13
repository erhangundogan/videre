---
title: videre config
description: Show or change where videre looks for things, and which model it uses.
---

Shows what every path and setting currently resolves to, and changes the three
defaults that let you run other commands with fewer arguments.

```bash
videre config                          # show where everything resolves to
videre config set db ~/photos.db       # use this database by default
videre config set path ~/Photos        # scan this folder when none is given
videre config set model google/siglip-base-patch16-224   # default search model
videre config unset db                 # go back to the default database
videre config unset model              # go back to the built-in search model
videre config unset path               # require a folder argument again
```

## Reading the output

```
home:          ~/.videre
config:        ~/.videre/config.toml
db:            (not set) [set with: videre config set db <path>]
resolved db:   ~/.videre/hashes.db
resolved path: ~/Photos [from config.toml]
model:         google/siglip-base-patch16-224 (default) [set with: videre config set model <id>]
jsonl:         ~/.videre/hashes.jsonl
```

| Line | Meaning |
|---|---|
| `home` | The directory holding everything. Follows `VIDERE_HOME` if set |
| `config` | Where the settings file lives, inside that home |
| `db` | Your configured default, or `(not set)` with the command to set it |
| `resolved db` | What commands will **actually** use right now |
| `resolved path` | The folder `scan` and `watch` use with no argument |
| `model` | The resolved search model, marked `(default)` when not configured |
| `read-rate` | Assumed floor read speed in MB/s, used to scale the file-read timeout to file size. Default 20. Only worth changing on a mount slower than that |
| `jsonl` | Where a bare `scan --output` would write |

The distinction between `db` and `resolved db` is the useful part. The first is
what you set; the second is what you get, including the fallback when you have
set nothing. When something reads the wrong library, this is the line to check.

`[from config.toml]` marks a value that came from your settings rather than a
built-in default.

## The three settings

| Key | Effect when set | Stored as |
|---|---|---|
| `db` | The database every command uses without `--db` | `default_db` |
| `path` | The folder `scan` and `watch` use with no argument | `default_path` |
| `model` | The [search model](/reference/models/) used without `--model` | `default_model` |

The file itself is plain TOML, and editing it by hand is fine:

```toml
default_path = "/Users/you/Photos"
default_db = "/Users/you/photos.db"
default_model = "google/siglip2-base-patch16-384"
```

Setting one key preserves the others. `videre config set db` stores an absolute
path, so a relative one is resolved against your current directory at the time
you set it, not later.

## How a value is chosen

For every command, first match wins:

1. An explicit flag (`--db`, `--model`)
2. Your setting in `config.toml`
3. The built-in default (`~/.videre/hashes.db`, or the default model)

`path` has no step 3: without it, `scan` and `watch` require a folder argument.
See [where your data lives](/reference/paths/) for the full picture.

## Switching between libraries

If you keep more than one library, setting the default is how you avoid typing
`--db` constantly:

```bash
videre config set db ~/personal.db
videre config set path ~/Photos
videre scan                            # scans ~/Photos into ~/personal.db

videre config set db ~/work.db
videre config set path ~/WorkShoots
videre scan                            # now scans WorkShoots into work.db
```

For switching everything at once, including caches and locks, `VIDERE_HOME` is
the bigger hammer. Each home has its own `config.toml`, so the two never
interfere.

## Caveats

**Settings live inside the home.** `config.toml` is stored in whichever
directory `VIDERE_HOME` points at, so changing that env var switches to a
different settings file entirely. Values are not shared between homes, and
`videre config` under a different `VIDERE_HOME` may correctly show nothing set.

**`set model` does not validate or download anything.** It records the id. A
typo is only reported later, when a command tries to use it, and the error then
lists the models you actually have prepared.

**`set db` does not create the database.** It records a path.
[`videre scan`](/commands/scan/) creates it; readers pointed at a missing
database say so and exit rather than creating an empty one.

**`scan` may set `path` for you.** The first folder you ever scan is adopted as
your default, with a note on stderr. It never overwrites a value you set
yourself. See [`videre scan`](/commands/scan/).

**Model choice is deliberately not an environment variable.** Use this command
or `--model`. The only environment variables that affect behaviour are
`VIDERE_HOME` and `VIDERE_EMBED_DTYPE`, both listed under
[where your data lives](/reference/paths/#environment-variables).

## More detail

- [Keeping libraries separate](/guides/multiple-libraries/) covers using these settings for more than one collection.

## Which database a command uses

Resolved in this order, first match wins:

1. `--db <path>` on the command line
2. `$VIDERE_HOME/hashes.db`, when `VIDERE_HOME` is set
3. `default_db` from `config.toml`
4. `~/.videre/hashes.db`

`VIDERE_HOME` deliberately outranks `default_db`, so pointing it at a directory
keeps a run inside that directory. This matters when the home is a *copy* of
another: the copied `config.toml` carries the original's absolute `default_db`,
and before v0.14.1 that path won, so the run wrote back into the source library.

When the two disagree, videre prints which one it is using.
