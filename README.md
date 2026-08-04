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
- search your photos by describing them - "sunset over water", "my red car"
- recognise faces and search by person
- browse everything in a generated HTML gallery
- fix wrong file dates from the camera's own EXIF data
- group photos by where they were taken

Everything runs on your machine, against a single SQLite file. No account, no
upload, no telemetry. videre never moves, renames, or re-encodes your photos -
it only reads them. Stop using it and your files are exactly as they were.

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

`videre dedupe` never deletes anything itself - it prints a list for you to
check. Add `--similar` to also flag photos and videos that merely *look* alike;
those are reported for review only, never included in the delete list.

### Search your photos

```bash
videre embed                              # one-time: prepares photos for search
videre search "golden gate bridge at sunset"
videre search --image reference.jpg       # find photos like this one
```

The first `videre embed` downloads about 1.4 GB of model data and takes a while
on a big library. You can stop it at any point and rerun later - it picks up
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

Every command takes `--help`. Most take `--silent`, and many take `--json` if
you want to script against them.

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
| `VIDERE_EMBED_MODEL` | Use a different search model. `google/siglip-base-patch16-224` is about twice as fast, at some cost to accuracy on fine detail. **Changing this means re-running `videre embed` over your whole library** - videre warns you before it starts. |
| `VIDERE_EMBED_DTYPE` | `f16` for slightly faster search preparation. Does not affect existing data. |

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

## Good to know

- Long jobs (`embed`, `faces`, `classify`) are resumable - Ctrl-C is safe, and
  rerunning continues where it stopped.
- Two different videre commands can run at once against the same database.
  Running the *same* command twice is refused rather than allowed to corrupt
  anything.
- `videre report --faces` and `--show-faces` start a local web server on
  `localhost:7878`. Nothing leaves your machine.
- The only feature that touches the network is `videre search --location`,
  which looks up a place name once and caches the result.

## License

Apache License 2.0 - see [LICENSE](LICENSE).
