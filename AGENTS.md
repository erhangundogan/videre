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
- Do not read or expose local secrets unless explicitly asked.

## Build And Test

- Build debug/test targets: make build-dev
- Build release binary: make build
- Format before committing: cargo fmt --all
- Check formatting: make fmt-check
- Run tests: make test
- Lint (list clippy warnings): make lint
- Lint gate as CI runs it (macOS config): make lint-check
- Lint gate for the Linux config, in Docker: make lint-linux
- Full verification: make verify
- Install docs deps: make docs-install
- Build docs: make docs-build

The clippy CI job runs on **Linux**, so a macOS `make lint-check` cannot see a
lint that only exists in the Linux config (the `cfg(target_os = "macos")`
branches are compiled out there). When a change touches `cfg(target_os = ...)`
code or its helpers/tests, verify with `make lint-linux` (Docker) before
pushing. Details and the reasoning are in CLAUDE.md's `## CI` section.

## Development Rules

- Rust toolchain is pinned in rust-toolchain.toml.
- Prefer Makefile targets where they exist.
- User-facing behavior changes must update docs under docs/src/content/docs/.
- Tests must not download model weights.
- Anything shared by multiple crates belongs in videre-core.
- Anything shared only by CLI subcommands should stay under crates/videre/src/.
- Verify with real command output before claiming work is complete.
- Tests come first: write the failing unit/integration test, then implement the feature or fix (TDD).
- Work each ticket in its own git worktree under .claude/worktrees/.
- .claude/worktrees/ is shared: never modify or delete a worktree you did not create.

### Changelog and releases

- The CHANGELOG uses a **write-at-release** approach: a feature or fix PR does
  **not** add a `[Unreleased]` entry and does **not** bump the version. A
  separate batched release PR bumps the version (all four crates share one) and
  writes that version's changelog section from the PRs merged since the last
  tag. Details in CLAUDE.md ("Getting a change in" and "Release and
  publishing").
