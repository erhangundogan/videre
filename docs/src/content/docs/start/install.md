---
title: Install
description: Install videre with Homebrew, a prebuilt binary, or from source.
sidebar:
  order: 1
---

## Homebrew

On macOS or Linux:

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

:::danger[An old `cargo install` will shadow a newer Homebrew install]
If you previously ran `cargo install videre`, that copy lives in `~/.cargo/bin`,
which usually comes first on `PATH`. It will keep winning even after you install
a newer version with Homebrew, and `videre --version` will quietly report the
old one.

```bash
which videre          # check which copy you are actually running
cargo uninstall videre
```
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

- The first [`videre embed`](/commands/embed/) downloads about 780 MB.
- The first [`videre faces`](/commands/faces/) downloads a separate 180 MB.

Both are resumable, so you can stop and rerun.
