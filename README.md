<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-animated-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/logo-animated-light.svg">
    <img alt="videre logo" src="assets/logo-animated-light.svg" width="216">
  </picture>
</p>

# videre

A local-first tool for making sense of a folder full of photos and videos.

- find and remove duplicates (identical *and* near-identical)
- search your photos by describing them, like "sunset over water" or "my red car"
- recognise faces and search by person
- import from Google Takeout, Apple Photos, or a Lightroom catalog
- browse everything in a generated HTML gallery
- fix wrong file dates from the camera's own EXIF data
- group photos by where they were taken

Free and open source under the permissive Apache 2.0 licence: no account, no
subscription, no lock-in, and every line is yours to read, audit, or build on.

**Documentation: [docs.videre.sh](https://docs.videre.sh)**

## Why videre

Most photo apps want to *own* your library. They import everything into their
own storage, index it in a database only they can read, then nudge you toward
their cloud. videre works the other way round: it's a lens over a folder you
already own. Point it at a directory and you get a single SQLite file describing
what's there. Stop using it and your photos are exactly as they were.

- **Nothing to keyword first.** Search works on photos you never tagged, by
  description, by person, by place, or by example image.
- **Your photos come in from wherever they are stuck.** `videre import` pulls a
  Google Takeout export, an Apple Photos library, or a Lightroom catalog into an
  ordinary folder, originals rather than derivatives, dates put back.
- **It never touches your photos.** Nothing is moved, renamed, copied, or
  re-encoded. The one exception is `videre fix-dates`, which you run
  deliberately, and which only corrects a file's date.
- **Nothing leaves your machine.** No account, no upload, no telemetry. The one
  exception is `videre search --location "Berlin"`, which looks a place name up
  once and remembers it.
- **Free, open, and permanent.** videre is open source under the permissive
  Apache 2.0 licence. It costs nothing, has no subscription or paywalled tier,
  and cannot be discontinued out from under you: read exactly what it does with
  your photos, keep the version you have forever, fork it, or build on it.
- **It won't delete anything behind your back.** `videre dedupe` prints what
  *could* go and stops there. You decide, and you can look through the
  candidates in a browser first.
- **Naming faces is bulk work, not a chore.** videre groups faces together
  itself, so you name one group of 40 photos rather than tagging 40 photos.
- **An unplugged drive is not deleted photos.** Libraries spread over external
  drives are ordinary. `videre prune` only drops a row when the file is gone
  *and* its folder still exists, so cleaning up with a drive detached does not
  wipe everything on it.
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
videre dedupe --html          # ...or review them visually in a browser first
videre dedupe | xargs trash   # delete them, once you have looked

videre embed                  # one-time: prepares photos for search
videre search "golden gate bridge at sunset"

videre faces                  # detect and group faces
videre gallery                # name the groups in your browser
videre search --person "Alice"
```

The first `videre embed` downloads about 780 MB of model data, and `videre
faces` a separate 180 MB. Nothing is downloaded until you run a command that
needs it, and both are resumable.

More: [docs.videre.sh/start/quickstart](https://docs.videre.sh/start/quickstart/)

## Commands

`scan`, `import`, `dedupe`, `gallery`, `search`, `embed`, `faces`, `classify`,
`locations`, `fix-dates`, `prune`, `watch`, `stats`, `config`, `mcp`.

Every command takes `--help`. Full reference with every flag:
[docs.videre.sh/commands](https://docs.videre.sh/commands/)

## Contributing

videre is maintained by one person and its direction is set deliberately. Bug
reports are always welcome. If you hit a real problem or want a feature you
would use yourself, so are patches: open an issue first to agree the approach.
See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache License 2.0. See [LICENSE](LICENSE).
