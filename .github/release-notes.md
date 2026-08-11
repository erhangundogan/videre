## Install

```bash
brew install erhangundogan/tap/videre
```

Or download an archive below and put `videre` on your `PATH`. No Rust toolchain, nothing to compile.

| platform | archive |
|---|---|
| Apple Silicon Mac | `aarch64-apple-darwin` |
| Linux x86_64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

Each has a `.sha256` alongside it. Every archive is downloaded onto a clean machine and actually run before this release is published.

Model weights download on first use, not at install: ~780 MB for `videre embed`, ~180 MB for `videre faces`. `scan`, `dedupe`, `fix-dates`, `prune`, `stats` and `locations` need no model at all.

Intel Macs are not supported: ONNX Runtime ships no prebuilt binaries for `x86_64-apple-darwin`.

See [CHANGELOG.md](https://github.com/erhangundogan/videre/blob/main/CHANGELOG.md) for what changed.
