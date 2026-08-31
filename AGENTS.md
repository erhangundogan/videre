# videre

## Project

videre is a Rust CLI for local-first photo and video library management.

## Required Context

Before substantial work, read:

- README.md
- CONTRIBUTING.md
- CLAUDE.md
- Relevant product docs under docs/src/content/docs/

Private/local context may exist outside this repository, but it must not be quoted, committed, or published.

## Safety Rules

- Never blanket-stage files. Use explicit paths only.
- Never stage more than 30 files in one pass.
- Never add generated caches, private media, thumbnails, home-directory files, secrets, or non-source files.
- Treat ~/.videre and external media libraries as real user data, not test data.
- Do not read or expose local secrets unless explicitly asked.

## Build And Test

- Build debug/test targets: make build-dev
- Build release binary: make build
- Format before committing: cargo fmt --all
- Check formatting: make fmt-check
- Run tests: make test
- Full verification: make verify
- Install docs deps: make docs-install
- Build docs: make docs-build

## Development Rules

- Rust toolchain is pinned in rust-toolchain.toml.
- Prefer Makefile targets where they exist.
- User-facing behavior changes must update docs under docs/src/content/docs/.
- Tests must not download model weights.
- Anything shared by multiple crates belongs in videre-core.
- Anything shared only by CLI subcommands should stay under crates/videre/src/.
- Verify with real command output before claiming work is complete.
