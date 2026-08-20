---
title: Install
description: Install videre with Homebrew, a prebuilt binary, or from source.
sidebar:
  order: 1
---

## Install script

One command, no Rust toolchain, nothing to compile:

```bash
curl -fsSL https://docs.videre.sh/install | sh
```

It works out your platform, downloads the matching release, **verifies it
against the `.sha256` published with that release before unpacking it**, and
puts the binary in `~/.local/bin`. If that directory is not on your `PATH` it
prints the line to add. It never edits your shell profile.

| Option | |
|---|---|
| `--version X.Y.Z` | Install a specific release rather than the latest |
| `--to DIR` | Install somewhere other than `~/.local/bin` |
| `--uninstall` | Remove the binary the script installed |
| `--help` | Show usage |

Pass options after `-s --`:

```bash
curl -fsSL https://docs.videre.sh/install | sh -s -- --version 0.18.0
```

:::note[Upgrades are manual]
The script does not upgrade videre for you. Run it again to move to a newer
release; it overwrites in place. If you would rather something managed the
version for you, use Homebrew below.
:::

### Removing it

```bash
curl -fsSL https://docs.videre.sh/install | sh -s -- --uninstall
```

This removes **one file**, the binary the script installed. Your library and
the downloaded models are left exactly where they are, and the script prints
both paths with their sizes so you can remove them yourself if you want to:

- `~/.videre/` holds the database, your config and the search embeddings. An
  embedding costs hours to recompute, so nothing here is deleted for you.
- `~/.cache/huggingface/` holds the models, and is shared with any other tool
  that uses the Hugging Face cache.

If the `videre` it finds was installed by Homebrew or `cargo`, the script
refuses to delete it and tells you the right command instead. Removing a
package manager's file behind its back leaves it believing videre is still
installed.

## Homebrew

**The best option on macOS**, because it handles upgrades:

```bash
brew install erhangundogan/tap/videre
```

## Prebuilt binary

Download from the [latest release](https://github.com/erhangundogan/videre/releases/latest)
and put it on your `PATH`. No Rust toolchain needed, nothing to compile.

| Platform | File |
|---|---|
| Apple Silicon Mac | `videre-<version>-aarch64-apple-darwin.tar.gz` |
| Linux x86_64 | `videre-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `videre-<version>-aarch64-unknown-linux-gnu.tar.gz` |

```bash
tar xzf videre-*-aarch64-apple-darwin.tar.gz
./videre --version
```

Each archive has a `.sha256` alongside it.

:::caution[Gatekeeper on macOS]
A binary downloaded with a browser is quarantined by macOS. Either download it
with `curl`, or clear the flag:

```bash
xattr -d com.apple.quarantine videre
```

Homebrew installs are not quarantined, so `brew install` avoids this entirely.
:::

:::caution[More than one videre will shadow itself]
If you have installed videre more than one way, whichever copy comes first on
your `PATH` wins, and `videre --version` quietly reports that one. An old
`cargo install videre` in `~/.cargo/bin` is the usual culprit.

```bash
which videre          # check which copy you are actually running
```

[Troubleshooting](/reference/troubleshooting/) lists where each install method
puts the binary and how to remove each one properly.
:::

## From source

Needs a Rust toolchain.

```bash
cargo install videre
```

```bash
git clone git@github.com:erhangundogan/videre.git
cd videre
cargo build --release
```

On ARM64 Linux, *building from source* needs one extra flag. The released
binary does not, since it is already built with it:

```bash
RUSTFLAGS="-C target-feature=+fp16" cargo install videre
```

Without it the build fails with `error: instruction requires: fullfp16`. This
is the main reason to prefer the [install script](#install-script) on ARM64
Linux: it fetches a binary that already has the flag baked in, so the problem
never arises.

## Platform notes

**Intel Macs are not supported.** The ONNX Runtime dependency ships no prebuilt
binaries for `x86_64-apple-darwin`, so videre cannot be built there at all,
including via `cargo install`.

**macOS is the primary platform.** videre also runs on Linux, with one gap: HEIC
photos and video frames are decoded using a macOS system tool, so on Linux those
files are skipped (with a clear message) for thumbnails, search, and face
detection. They are still scanned, hashed, and de-duplicated. JPEG, PNG and
friends work everywhere.

See [platform support](/reference/platforms/) for the per-command matrix.

## Models are not downloaded at install

Nothing is downloaded until you run a command that needs it. Scanning,
de-duplicating, fixing dates, pruning and stats work with no model at all.

Two commands need machine-learning models, and each fetches its own from
[Hugging Face](https://huggingface.co) the first time you run it:

| Command | Model | What it is | Size |
|---|---|---|---|
| [`videre embed`](/commands/embed/) | [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224) | SigLIP, Google's image/text model. Turns a photo and a phrase into comparable vectors, which is what makes "sunset over water" match a picture. | ~780 MB |
| [`videre faces`](/commands/faces/) | [`WePrompt/buffalo_l`](https://huggingface.co/WePrompt/buffalo_l) | InsightFace buffalo_l, two ONNX models: `det_10g.onnx` (SCRFD) finds faces, `w600k_r50.onnx` (ArcFace) turns each face into a vector so matching ones can be grouped. | ~180 MB |

Both downloads are resumable, so you can stop and rerun.

They are only fetched once and then reused, including by later runs against a
different library. The download happens at the start of the run, before any of
your photos are processed.

### Where they are stored

In the standard Hugging Face cache, shared with any other tool that uses it:

```
~/.cache/huggingface/hub/
```

Set `HF_HOME` to put it elsewhere. This is separate from
[videre's own directory](/reference/paths/), so removing `~/.videre` does not
delete the models, and deleting the cache means the next `embed` or `faces` run
downloads them again.

### Bigger models are opt-in

The table above is the default. If you select a different
[search model](/reference/models/), it is fetched instead, and the larger ones
are considerably bigger:
[`siglip2-base-patch16-384`](https://huggingface.co/google/siglip2-base-patch16-384)
is about 1.4 GB and
[`siglip-so400m-patch14-384`](https://huggingface.co/google/siglip-so400m-patch14-384)
about 3.3 GB. Nothing downloads them unless you ask
for them by name.

:::note[Nothing is uploaded]
These are downloads only. The models run on your machine, and your photos are
never sent anywhere. The one feature that makes an outbound request is
[`videre search --location`](/commands/search/), which looks up a place name.
:::
