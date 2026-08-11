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

**Documentation: [docs.videre.sh](https://docs.videre.sh)**

## Why videre

Most photo apps want to *own* your library. They import everything into their
own storage, index it in a database only they can read, then nudge you toward
their cloud. videre works the other way round: it's a lens over a folder you
already own. Point it at a directory and you get a single SQLite file describing
what's there. Stop using it and your photos are exactly as they were.

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

On macOS or Linux with Homebrew:

```bash
brew install erhangundogan/tap/videre
```

Or download a binary from the [latest release](https://github.com/erhangundogan/videre/releases/latest)
and put it on your `PATH`. No Rust toolchain needed, nothing to compile.

Or build it yourself, which needs Rust:

```bash
cargo install videre
```

**If you previously ran `cargo install videre`**, that copy lives in
`~/.cargo/bin` and usually comes first on `PATH`, so it will keep shadowing a
newer Homebrew install. Check with `which videre`, and remove it with
`cargo uninstall videre`.

**Intel Macs are not supported.** A dependency ships no prebuilt binaries for
them, so videre cannot be built there at all, including via `cargo install`.

**macOS is the primary platform.** videre also runs on Linux, with one gap: HEIC
photos and video frames are decoded using a macOS system tool, so on Linux those
files are skipped for thumbnails, search, and face detection. They are still
scanned, hashed, and de-duplicated.

Full install notes, including the extra flag needed to build on ARM64 Linux:
[docs.videre.sh/start/install](https://docs.videre.sh/start/install/)

## Quickstart

Start here. Everything else reads from what this creates.

```bash
videre scan ~/Photos
```

That builds a database at `~/.videre/hashes.db` describing what you have. It
does not change your photos.

```bash
videre dedupe                 # list which copies could go
videre report                 # ...or review them visually in a browser first
videre dedupe | xargs trash   # delete them

videre embed                  # one-time: prepares photos for search
videre search "golden gate bridge at sunset"

videre faces                  # detect and group faces
videre report --faces         # name the groups in your browser
videre search --person "Alice"
```

The first `videre embed` downloads about 780 MB of model data, and `videre
faces` a separate 180 MB. Nothing is downloaded until you run a command that
needs it, and both are resumable.

More: [docs.videre.sh/start/quickstart](https://docs.videre.sh/start/quickstart/)

## Commands

`scan`, `dedupe`, `report`, `search`, `embed`, `faces`, `classify`, `locations`,
`fix-dates`, `prune`, `watch`, `stats`, `config`, `mcp`.

Every command takes `--help`. Full reference with every flag:
[docs.videre.sh/commands](https://docs.videre.sh/commands/)

## Before you point it at a real library

Most of videre is read-only. These are the parts that are not.

**`videre dedupe` prints files to delete.** Its output is the REMOVE side of each
duplicate group, so `videre dedupe | xargs trash` deletes those files
immediately. Look before you pipe: run `videre report` first, or send the list to
a file and read it.

**Keep your photos connected when running `prune`.** It removes database entries
for files it cannot find. videre guards against an unplugged drive (a row is only
removed when the file is missing *and* its folder still exists), but the guard is
worth knowing about rather than relying on.

**`videre fix-dates` rewrites file timestamps on disk.** There is no undo. It
asks for confirmation first, and `--dry-run` shows exactly what it would do.

The rest, including disk use and running two heavy commands at once:
[docs.videre.sh/start/cautions](https://docs.videre.sh/start/cautions/)

## License

Apache License 2.0. See [LICENSE](LICENSE).
