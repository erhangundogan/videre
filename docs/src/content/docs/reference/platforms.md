---
title: Platform support
description: What works on macOS and Linux, and what does not work at all.
---

macOS is the primary platform, and everything works there. videre also builds
and runs on Linux, with one functional gap.

## Per-command support

| | macOS | Linux |
|-|-------|-------|
| `scan`, `dedupe`, `gallery`, `fix-dates`, `prune`, `locations`, `stats`, `mcp`, `config` | yes | yes |
| `embed`, `search` | yes (GPU) | yes (CPU only) |
| `faces` | yes | yes |
| `watch` | yes | yes (`--heic` stage unavailable) |
| HEIC decoding | yes | **no** |
| Video frame extraction | yes | **no** |
| HEIC and video scanning, hashing, EXIF | yes | yes |
| File creation time | yes | always empty |

## Intel Macs are not supported

The ONNX Runtime dependency ships no prebuilt binaries for
`x86_64-apple-darwin`, so any build fails with:

```
no prebuilt binaries available for target x86_64-apple-darwin
```

This is not a packaging problem and cannot be worked around by building it
yourself: `cargo install videre` fails identically on an Intel Mac.

## The HEIC and video gap on Linux

HEIC images and video frames are decoded through macOS QuickLook, which has no
equivalent elsewhere. On Linux, `.heic`, `.mov` and `.mp4` files get:

- no thumbnails
- no search embeddings
- no face detection
- no near-duplicate fingerprint

They are still scanned, hashed, EXIF-extracted, and exactly de-duplicated. The
failure is explicit rather than silent: a message is printed once per run rather
than each file failing quietly.

Regular JPEG, PNG, GIF, WebP, BMP and TIFF work identically on both platforms.

## Building on ARM64 Linux

Building from source needs one extra flag. The released binary does not, since
it is already built with it:

```bash
RUSTFLAGS="-C target-feature=+fp16" cargo install videre
```

Without it the build fails with `instruction requires: fullfp16`. Building from
inside a clone of the repository handles this automatically.

x86_64 Linux is unaffected.

:::caution
`cargo check` passes on ARM64 Linux even when `cargo build` fails, because
`check` never runs code generation. A green `check` is not evidence that
`cargo install` works.
:::
