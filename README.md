<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
    <img alt="videre logo" src="assets/logo-light.svg" width="216">
  </picture>
</p>

# videre

A local-first tool for making sense of a folder full of photos and videos.

- find and remove duplicates (identical *and* near-identical)
- search your photos by describing them, like "sunset over water" or "my red car"
- recognise faces and search by person
- browse everything in a generated HTML gallery
- fix wrong file dates from the camera's own EXIF data
- group photos by where they were taken

## Why videre

Most photo apps want to *own* your library. They import everything into their
own storage, index it in a database only they can read, then nudge you toward their
cloud. videre works the other way round: it's a lens over a folder you already
own. Point it at a directory and you get a single SQLite file describing what's
there. Stop using it and your photos are exactly as they were.

- **It never touches your photos.** Nothing is moved, renamed, copied, or
  re-encoded. The one exception is `videre fix-dates`, which you run
  deliberately, and which only corrects a file's date.
- **Nothing leaves your machine.** No account, no upload, no telemetry. The one
  exception is `videre search --location "Berlin"`, which looks a place name up
  once and remembers it.
- **It won't delete anything behind your back.** `videre dedupe` prints what
  *could* go and stops there. You decide, and you can look through the
  candidates in a browser first.
- **Naming faces is bulk work, not a chore.** videre groups faces together
  itself, so you name one group of 40 photos rather than tagging 40 photos.
- **Long jobs are safe to interrupt.** Preparing search or scanning faces can
  take hours on a large library. Ctrl-C is fine; rerunning continues from where
  it stopped.

And because it's an ordinary command-line tool over an ordinary SQLite file, an
AI assistant can drive it directly. `videre mcp` hands search and duplicate
review to a model with no server to set up.

## Install

```bash
cargo install videre
```

Or from source:

```bash
git clone git@github.com:erhangundogan/videre.git
cd videre
cargo build --release
```

**macOS is the primary platform.** videre also runs on Linux, with one gap:
HEIC photos and video frames are decoded using a macOS system tool, so on
Linux those files are skipped (with a clear message) for thumbnails, search,
and face detection. They are still scanned, hashed, and de-duplicated. Regular
JPEG/PNG and friends work everywhere.

On ARM64 Linux the build needs one extra flag:

```bash
RUSTFLAGS="-C target-feature=+fp16" cargo install videre
```

## Quickstart

Start here. Everything else reads from what this creates.

```bash
videre scan ~/Photos
```

That builds a database at `~/.videre/hashes.db` describing what you have. It
does not change your photos.

### Clean up duplicates

```bash
videre dedupe                 # list which copies could go
videre report                 # ...or review them visually in a browser first
videre dedupe | xargs trash   # delete them
videre prune                  # tidy the database afterwards
```

`videre dedupe` never deletes anything itself. It prints a list for you to
check. Add `--similar` to also flag photos and videos that merely *look* alike;
those are reported for review only, never included in the delete list.

### Search your photos

```bash
videre embed                              # one-time: prepares photos for search
videre search "golden gate bridge at sunset"
videre search --image reference.jpg       # find photos like this one
```

The first `videre embed` downloads about 1.4 GB of model data and takes a while
on a big library. You can stop it at any point and rerun later, and it picks up
where it left off.

### Find people

```bash
videre faces                  # detect and group faces
videre report --faces         # name the groups in your browser
videre search --person "Alice"
```

### Other things it can do

```bash
videre classify                        # tag screenshots/documents/memes
videre search --category screenshot

videre locations                       # group photos by place
videre search --location "Berlin"

videre fix-dates                       # set file dates from EXIF
videre report --all                    # browse the whole library
videre stats                           # what's in the library
videre watch ~/Photos                  # keep everything fresh in the background
```

## Commands

| Command | What it does |
|---------|--------------|
| `videre scan` | Read a folder and record what's in it. Run this first. |
| `videre dedupe` | List duplicate copies you could delete |
| `videre report` | Generate an HTML gallery, or serve the face-naming UI |
| `videre search` | Find photos by description, example image, person, category, or place |
| `videre embed` | Prepare photos for search (one-time, resumable) |
| `videre faces` | Detect and group faces |
| `videre classify` | Tag photos as photo/screenshot/document/meme |
| `videre locations` | Group photos by where they were taken |
| `videre fix-dates` | Set each file's date from its EXIF shoot date |
| `videre prune` | Remove database entries for files that no longer exist |
| `videre watch` | Background loop keeping everything current |
| `videre stats` | Library totals and what has run recently |
| `videre config` | Show or change defaults |
| `videre mcp` | Expose search to AI agents |

Every command below also takes `--help`.

### videre scan

Reads a folder recursively and records every media file in the database.

```bash
videre scan ~/Photos                              # scan into the default database
videre scan                                       # same, using the folder from `videre config set path`
videre scan ~/Photos --similar                    # also fingerprint images/videos for near-duplicate detection
videre scan ~/Photos --output-sqlite ~/photos.db  # write to a specific database instead
videre scan ~/Photos --output                     # write JSONL to ~/.videre/hashes.jsonl instead of SQLite
videre scan ~/Photos --output out.jsonl           # write JSONL to a specific file
videre scan ~/Photos --silent                     # no progress output
videre scan ~/Photos --json                       # print one JSON summary object instead
```

Re-running is safe and idempotent, since existing entries are updated in place.
`--output` and `--output-sqlite` cannot be combined. A bare `--output` must come
*after* the folder, or it swallows the folder as its value.

### videre dedupe

Finds duplicates already recorded in the database. Prints one path per line to
stdout, and nothing else, so it pipes cleanly.

```bash
videre dedupe                          # list removable copies (one path per line)
videre dedupe | xargs trash            # ...and delete them
videre dedupe --similar                # also report look-alike groups (review only, not printed to stdout)
videre dedupe --db ~/photos.db         # use a specific database
videre dedupe --silent                 # suppress the summary; paths still print
videre dedupe --json                   # print one JSON object instead
```

Look-alike groups from `--similar` are deliberately kept out of stdout, so
piping into a delete command can never act on a mere resemblance.

### videre report

Builds a browsable HTML page, or serves the interactive face-naming UI.

```bash
videre report                          # duplicate-review page, written next to the database
videre report -o out.html              # write somewhere specific (--output works too)
videre report --all                    # every file, with in-page similarity search
videre report --by-date                # Year/Month/Day drill-down gallery
videre report --heic                   # embed HEIC thumbnails (macOS only, bigger file)
videre report --heic-original          # ...plus full-size versions for the lightbox
videre report --faces                  # face-naming UI at http://localhost:7878
videre report --show-faces             # live page showing names and places in the lightbox
videre report --db ~/photos.db         # use a specific database
videre report --all --model <model-id> # use a specific model for in-page similarity
```

`--faces` and `--show-faces` start a local server instead of writing a file.
Everything stays on your machine.

### videre search

```bash
videre search "sunset over water"          # search by description
videre search --image photo.jpg            # find photos like this one
videre search --person "Alice"             # photos of a named person (after naming faces)
videre search --category screenshot        # photo / screenshot / document / meme / unknown
videre search --location "Berlin, Germany" # photos taken near a place
videre search "a dog" -k 50                # more results, default 20 (--top-k works too)
videre search "a dog" --scores             # show how well each result matched
videre search "a dog" --json               # print one JSON object instead
videre search --location "Rome" --radius 5 # tighter radius in km (default 20)
videre search "a dog" --db ~/photos.db     # use a specific database
videre search "a dog" --model <model-id>   # search a specific model's data
```

Text and image search need `videre embed` first; `--person` needs `videre
faces`; `--category` needs `videre classify`; `--location` needs GPS data in
your photos.

### videre embed

Prepares photos so they can be searched by description. One-time per photo,
and resumable.

```bash
videre embed                           # process everything not done yet
videre embed --db ~/photos.db          # use a specific database
videre embed --model <model-id>        # prepare with a specific model, kept separately
videre embed --batch 64                # images per inference batch (default 32, max 96)
videre embed --chunk 1000              # rows saved per transaction (default 500)
videre embed --silent                  # no per-image progress
```

Safe to Ctrl-C, since rerunning continues where it stopped. Raising `--batch` is not
a way to make this faster; values above 96 are capped automatically.

### videre faces

Detects faces, then groups them so you can name a person once instead of
tagging each photo.

```bash
videre faces                           # detect, group, and store (resumable)
videre faces --limit 500               # only process 500 new images, then stop
videre faces --recluster               # regroup existing faces without re-detecting
videre faces --reprocess               # start over: re-detect everything
videre faces --dry-run                 # detect but write nothing
videre faces --profile                 # print per-stage timing when finished
videre faces --silent                  # no per-image progress
videre faces --db ~/photos.db          # use a specific database

# Tuning: only worth touching if grouping looks wrong
videre faces --eps 0.6                 # how alike faces must be to group (default 0.6)
videre faces --min-cluster-size 3      # fewest faces that can form a group (default 3)
videre faces --merge-sim 0.35          # how readily two groups merge (default 0.35)
videre faces --min-face-size 80        # ignore faces smaller than this in pixels (default 80)
videre faces --max-generic-sim 0.4     # drop blurry/featureless faces (default 0.4)
videre faces --batch 8                 # images per batch (default 8)
videre faces --workers 8               # parallel workers (default: 2x your CPU cores)
videre faces --qlmanage-concurrency 6  # simultaneous HEIC conversions (default 6)
```

After running this, name the groups with `videre report --faces`.

### videre classify

Tags each photo as photo, screenshot, document, or meme. Reuses the work
`videre embed` already did, so it is quick.

```bash
videre classify                        # classify everything not done yet
videre classify --reprocess            # redo everything, including already-tagged
videre classify --margin 0.05          # how confident it must be (default 0.05)
videre classify --silent               # no per-image progress
videre classify --db ~/photos.db       # use a specific database
```

Anything it isn't confident about is stored as `unknown` rather than guessed.

### videre locations

Groups photos by where they were taken, using GPS data already in the files.

```bash
videre locations                       # group and print a summary
videre locations --radius 25           # how far apart places can be, in km (default 15)
videre locations --json                # print one JSON object instead
videre locations --geojson             # print GeoJSON (opens in geojson.io, QGIS, ...)
videre locations --silent              # no summary
videre locations --db ~/photos.db      # use a specific database
```

### videre fix-dates

Sets each file's date from the date the camera recorded. This is the only
command that changes your files.

```bash
videre fix-dates --dry-run             # show what would change, touch nothing
videre fix-dates                       # apply (asks for confirmation first)
videre fix-dates --yes                 # apply without asking (for scripts)
videre fix-dates --silent              # no per-file output
videre fix-dates --db ~/photos.db      # use a specific database
```

### videre prune

Cleans up database entries for files you have deleted. Never touches real
files.

```bash
videre prune --dry-run                 # show what would be removed
videre prune                           # remove stale entries and refresh metadata
videre prune --silent                  # no per-file output
videre prune --db ~/photos.db          # use a specific database
```

### videre watch

Keeps everything current in the background. Runs until you stop it with Ctrl-C.

```bash
videre watch ~/Photos                  # scan, faces, HEIC cache, and locations every 5 minutes
videre watch                           # same, using the folder from `videre config set path`
videre watch ~/Photos --scan --faces   # only these stages
videre watch ~/Photos --heic           # only pre-convert HEIC thumbnails
videre watch ~/Photos --location       # only look up place names
videre watch ~/Photos --prune          # also clean stale entries (off by default)
videre watch ~/Photos --interval 60    # seconds between cycles (default 300)
videre watch ~/Photos --silent         # no per-cycle output
videre watch ~/Photos --output-sqlite ~/photos.db   # use a specific database
```

### videre stats

```bash
videre stats                           # library totals and what has run recently
videre stats --json                    # print one JSON object instead
videre stats --check                   # exit non-zero if anything failed or crashed (for cron)
videre stats --db ~/photos.db          # use a specific database
```

### videre config

```bash
videre config                          # show where everything resolves to
videre config set db ~/photos.db       # use this database by default
videre config set path ~/Photos        # scan this folder when none is given
videre config unset db                 # go back to the default database
videre config unset path               # require a folder argument again
```

### videre mcp

Serves read-only search, duplicate review, and stats to an AI assistant over
stdio.

```bash
videre mcp                             # serve using the default database
videre mcp --db ~/photos.db            # serve a specific database
```

## Supported files

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.heic` `.mov` `.mp4` `.dng`

## Where your data lives

Everything lives in `~/.videre`:

```
~/.videre/
  hashes.db      # the database
  config.toml    # your defaults
  locks/         # marks which command is currently running
```

Nothing is created until you actually write something. To use a different
database, pass `--db <path>` (or `--output-sqlite <path>` for `scan` and
`watch`), or set a default once:

```bash
videre config set db ~/photos.db
videre config set path ~/Photos    # so `videre scan` works with no arguments
videre config                      # show what's currently set
```

## Environment variables

| Variable | Effect |
|----------|--------|
| `VIDERE_HOME` | Use a different home directory instead of `~/.videre` |
| `VIDERE_EMBED_MODEL` | Use a different search model by default. `google/siglip-base-patch16-224` is about twice as fast, at some cost to accuracy on fine detail. The `--model` flag overrides this per command. |
| `VIDERE_EMBED_DTYPE` | `f16` for slightly faster search preparation. Does not affect existing data. |

## Using more than one search model

Each model keeps its own data, under `~/.videre/embeddings/`, so they never
overwrite each other and you can compare them on the same library:

```bash
videre embed                                                  # the default model
videre embed --model google/siglip-base-patch16-224           # a second, faster one
videre stats                                                  # see what each has
videre search "sunset" --model google/siglip-base-patch16-224 # search a specific one
```

Preparing a second model does not disturb the first. Asking for a model you
have not prepared yet gives you an error listing the ones you do have, rather
than silently returning nothing.

If you used videre before version 0.10, your existing data still works with no
action needed. Run `videre embed` when convenient to move it to the new
location.

## Working with other tools

`videre dedupe` prints one file path per line, so it pipes into anything:

```bash
videre dedupe | xargs trash
videre dedupe > to-delete.txt
```

`videre search` and `videre dedupe` accept `--json` for scripting, and
`videre mcp` exposes search and duplicate review to AI agents over stdio:

```json
{
  "mcpServers": {
    "videre": { "command": "/path/to/videre", "args": ["mcp"] }
  }
}
```

## Cautions

Most of videre is read-only. These are the parts that are not, plus the
situations that surprise people.

**`videre dedupe` prints files to delete.** Its output is the REMOVE side of
each duplicate group, so `videre dedupe | xargs trash` deletes those files
immediately. Look before you pipe: run `videre report` first and review the
KEEP/REMOVE badges, or send the list to a file and read it. Near-duplicate
groups from `--similar` are deliberately kept out of this output, because they
are for review by eye, not for automatic deletion.

**Keep your photos connected when running `prune` or `watch --prune`.** Both
delete database rows for files they cannot find on disk. If your library lives
on an external drive and that drive is unplugged, every file looks missing and
you lose the whole index. Your photos are untouched, but faces, names, and
locations are stored against those rows and go with them. Rebuilding means a
full rescan plus rerunning `faces`, `embed`, and `classify`.

**`videre fix-dates` rewrites file timestamps on disk.** It sets each file's
modification time from its EXIF date. That is a real change to your files and
there is no undo. It asks for confirmation first, and `--dry-run` shows you
exactly what it would do.

**Do not run two heavy commands at the same time.** `embed`, `faces`, and
`watch` all convert HEIC and video through macOS QuickLook, and they each limit
themselves to a few conversions at a time. That limit is per command, not
system-wide, so two at once can overwhelm QuickLook: measured on a real
library, a single file took over 16 seconds against about 7.6 seconds normally,
and one exceeded the timeout entirely. Nothing is lost, since skipped files are
simply retried next run, but it is much slower than doing one thing at a time.

**Disk use grows quietly.** Each search model keeps its own data, roughly 130MB
to 190MB per model for a 70,000 photo library, and the HEIC thumbnail cache can
reach tens of GB. Only `videre prune` reclaims any of it, and nothing warns you
first. `videre stats` shows what each model is using.

**`videre scan` remembers the first folder you give it** as your default, so
later commands can be run without repeating it. It says so when it happens, and
`videre config set path` changes it.

## Good to know

- Long jobs (`embed`, `faces`, `classify`) are resumable. Ctrl-C is safe, and
  rerunning continues where it stopped.
- Two different videre commands can run at once against the same database.
  Running the *same* command twice is refused rather than allowed to corrupt
  anything. See Cautions above on running two heavy jobs together.
- `videre report --faces` and `--show-faces` start a local web server on
  `localhost:7878`. Nothing leaves your machine.
- The only feature that touches the network is `videre search --location`,
  which looks up a place name once and caches the result.

## More detail

Algorithms, tuning constants, benchmark numbers, and the database schema are in
[CLAUDE.md](CLAUDE.md).

## License

Apache License 2.0. See [LICENSE](LICENSE).
