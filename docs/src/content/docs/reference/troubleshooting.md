---
title: Troubleshooting
description: Symptoms you might hit, what causes them, and what to do about each.
---

Organised by what you see, not by which command produced it.

## `videre --version` reports an old version

Almost always more than one videre on your `PATH`. Whichever directory comes
first wins, and nothing looks wrong: the command runs, it just is not the copy
you installed.

```bash
which videre
```

Then work out where that copy came from and remove it with the tool that put it
there:

| Path | Installed by | Remove with |
|---|---|---|
| `~/.cargo/bin/videre` | `cargo install` or `make install` | `cargo uninstall videre` |
| `/opt/homebrew/bin/videre` | Homebrew | `brew uninstall videre` |
| `~/.local/bin/videre` | The [install script](/start/install/) | `curl -fsSL https://videre.sh/install \| sh -s -- --uninstall` |

To list every copy at once:

```bash
echo $PATH | tr ':' '\n' | while read d; do [ -x "$d/videre" ] && echo "$d/videre"; done
```

:::caution[Do not just delete the file]
Deleting `/opt/homebrew/bin/videre` or `~/.cargo/bin/videre` by hand leaves
Homebrew or cargo believing videre is still installed, so `cargo install videre`
will report it as already present and skip. Use the commands above.
:::

## I installed a new version but nothing changed

Same cause as above. The install script warns about this when it runs, naming
both copies.

If you installed with the script, note that it **does not upgrade you
automatically**, and nothing tells you a newer release exists. Run the same
command again to move to one:

```bash
curl -fsSL https://videre.sh/install | sh
```

It overwrites the binary in place; nothing else is touched. See
[upgrading](/start/install/#upgrading). Homebrew handles this for you, which is
why it is the recommended route on macOS.

## Search returns nothing

Filters are applied before ranking, so a composed query can legitimately match
nothing. Work out which part is responsible by removing filters one at a time.

Every scoped run prints `N of M`, so a filter that matched nothing is visible
rather than silent. If `M` is 0, the library itself is empty for that command.

Two setup causes to rule out first:

```bash
videre stats
```

- **Nothing embedded yet.** Text search needs [`videre embed`](/commands/embed/)
  to have run. Scanning alone does not produce searchable vectors.
- **A different model.** Embeddings are stored per model, so searching with a
  model you have not embedded with finds nothing. See
  [using several search models](/guides/multiple-models/).

`--person` and `--category` need [`faces`](/commands/faces/) and
[`classify`](/commands/classify/) respectively.

## Files are skipped as unreachable

videre bounds every filesystem call, so a disconnected or sleeping drive fails
in seconds instead of hanging forever.

Whole-file reads scale their limit with file size, because a large file on a
healthy disk legitimately takes longer than a small one. If your drive is slow
but working, and large videos are being skipped, lower the assumed floor rate so
each file is given more time:

```bash
videre config set read-rate 5
```

The default assumes 20 MB/s or better. The `stat` that reads the file size
keeps a short fixed timeout on purpose, so a dead mount fails there rather than
waiting for a size-scaled read that will never finish.

## HEIC photos or videos are skipped on Linux

Expected. HEIC and video frames are decoded through a macOS system tool, so on
Linux those files are skipped for thumbnails, search and face detection. They
are still scanned, hashed and de-duplicated, so duplicate detection over them
works normally.

JPEG, PNG and the other common formats work everywhere. See
[platform support](/reference/platforms/).

## A command sits there doing nothing

One writer at a time. SQLite in WAL mode allows many readers alongside a single
writer, so a command that needs to write waits while another holds the lock.

The usual cause is [`videre watch`](/commands/watch/) running in the background.
Check what is running:

```bash
videre stats
```

It reports which pipeline step last ran, whether it succeeded, and whether a job
marked running actually crashed.

[`videre locations`](/commands/locations/) is the longest single writer: it
recomputes every cluster in one transaction, so it holds the lock for the whole
run.

## A long job was interrupted

Just run it again. Every long job is resumable and records what it already
processed, including work that produced no result, so re-running does not repeat
finished work.

That last part matters: face detection records images where it found **zero**
faces, and scanning marks files it could not identify. Without that, every
landscape photo would be re-examined on every run. See
[long-running jobs](/guides/long-running-jobs/).

## Rotated photos: wrong face clusters or bad search results

Photos stored rotated on disk (an EXIF orientation tag other than 1, common in
exports and files edited by other tools) are decoded the way you see them since
videre 0.26.0. Files processed by an older version were detected and embedded
on the stored pixel canvas, which can leave a person's own photos as
unassigned singletons and make text search miss photos that are clearly there.
Re-detecting those files fixes both; files scanned after upgrading need
nothing.

Two ways to recover, most precise first:

**Rebuild everything.** Simple, and it re-runs hours of model work:

```bash
videre faces --reset
videre embed --reprocess
videre classify --reprocess
```

`faces --reset` deletes and re-creates every face row, so names you
assigned in the gallery do not carry over and must be assigned again. It
asks for confirmation first; add `--yes` for scripts.

**Repair only the rotated files.** Names on untouched files survive. List the
rotated JPEGs, map them to library hashes, delete exactly their derived rows,
then re-run the commands normally:

```bash
# 1. list rotated JPEGs under the library
exiftool -ext jpeg -ext jpg -if '$Orientation# != 1' \
  -p '$Directory/$Filename' -r ~/Photos | sort > /tmp/rotated.txt

# 2. their hashes, from the library database
sqlite3 ~/Photos/.videre/hashes.db \
  "SELECT hash FROM file_hashes" | python3 -c '
import sqlite3, sys, os
root = os.path.expanduser("~/Photos")
wanted = {os.path.realpath(p) for p in open("/tmp/rotated.txt").read().split()}
con = sqlite3.connect(f"{root}/.videre/hashes.db")
for (h, p) in con.execute("SELECT hash, path FROM file_hashes"):
    if os.path.realpath(p) in wanted:
        print(h)' > /tmp/rotated_hashes.txt

# 3. delete their derived rows (faces, and under each model database the
#    embeddings; classifications lives in the main database)
sqlite3 ~/Photos/.videre/hashes.db \
  "DELETE FROM faces WHERE hash IN ($(sed "s/.*/'&'/" /tmp/rotated_hashes.txt | paste -sd, -));
   DELETE FROM faces_scanned WHERE hash IN ($(sed "s/.*/'&'/" /tmp/rotated_hashes.txt | paste -sd, -));
   DELETE FROM classifications WHERE hash IN ($(sed "s/.*/'&'/" /tmp/rotated_hashes.txt | paste -sd, -));"
sqlite3 ~/Photos/.videre/embeddings/google--siglip-base-patch16-224.db \
  "DELETE FROM embeddings WHERE hash IN ($(sed "s/.*/'&'/" /tmp/rotated_hashes.txt | paste -sd, -));"

# 4. re-run; only the repaired files are processed
videre faces
videre embed
videre classify
```

Back up the database (or the whole `.videre` folder) before step 3.

## Intel Mac: it will not install

videre cannot be built for Intel Macs at all. The ONNX Runtime dependency
publishes no prebuilt binaries for `x86_64-apple-darwin`, so `cargo install`
fails the same way the install script does. This is not something a flag or a
different install route works around. Apple Silicon Macs are fully supported.

## ARM64 Linux: the build fails with `fullfp16`

Building from source on ARM64 Linux needs one compiler flag:

```bash
RUSTFLAGS="-C target-feature=+fp16" cargo install videre
```

The released binaries already carry it, so the
[install script](/start/install/) avoids the problem entirely.

## Still stuck

- [Cautions](/reference/cautions/) covers the commands that change something,
  and the situations that surprise people.
- [Where your data lives](/reference/paths/) explains how a database is
  resolved, which answers most "it is not using the library I expected".
- Report a problem at
  [github.com/erhangundogan/videre/issues](https://github.com/erhangundogan/videre/issues).
  Include `videre --version` and the exact command.
