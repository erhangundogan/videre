---
title: videre config
description: Show or change where videre looks for things, and which model it uses.
---

```bash
videre config                          # show where everything resolves to
videre config set db ~/photos.db       # use this database by default
videre config set path ~/Photos        # scan this folder when none is given
videre config set model google/siglip-base-patch16-224   # default search model
videre config unset db                 # go back to the default database
videre config unset model              # go back to the built-in search model
videre config unset path               # require a folder argument again
```

Bare `videre config` shows the resolved home directory, the config file path,
the `db`, `path` and `model` settings labeled by the key you would set, the
resolved database, and the JSONL path. Unset values show a hint with the command
to set them.

## The three settings

| Key | Effect when set |
|---|---|
| `db` | The database every command uses when `--db` is not passed |
| `path` | The folder `scan` and `watch` use when no folder argument is given |
| `model` | The [search model](/reference/models/) used when `--model` is not passed |

`set db` writes an absolute path. Setting one key preserves the others.

There is no built-in fallback for `path`: without it, `scan` and `watch` require
a folder argument. [`videre scan`](/commands/scan/) adopts the first folder you
give it automatically if `path` is not already set.

## Settings are configuration, not environment

Model choice in particular is deliberately not an environment variable. Use
`videre config set model <id>` for a lasting default, or `--model <id>` for a
single command.

See [where your data lives](/reference/paths/) for how `--db`, this file, and
`VIDERE_HOME` interact.
