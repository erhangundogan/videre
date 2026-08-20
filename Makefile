.DEFAULT_GOAL := help

# Every Rust command goes through the toolchain pinned in rust-toolchain.toml,
# never a bare `cargo`.
#
# The channel is read from rust-toolchain.toml so the version lives in exactly
# one place. Change it there.
#
# `cargo +<toolchain>` rather than a bare `cargo`, so the version is explicit
# even if a non-shim cargo is ever first on PATH again. That machine is not
# hypothetical: until 2026-08-20 a Homebrew `rust` formula owned
# /opt/homebrew/bin/cargo, and a real cargo binary ignores rust-toolchain.toml
# entirely. With `+`, such a cargo fails loudly instead of silently building
# with the wrong compiler.
#
# :warning: NOT `rustup run <toolchain> cargo`. That execs the right cargo but
# does **not** put the toolchain's bin on PATH, so cargo subcommands are not
# found: `rustup run 1.96.0 cargo fmt` dies with "no such command: fmt". It
# appeared to work only while Homebrew's cargo-fmt happened to be on PATH, and
# broke the moment that was uninstalled.
TOOLCHAIN := $(shell sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
CARGO := cargo +$(TOOLCHAIN)

# cargo-llvm-cov must run through the rustup-managed toolchain, not the
# default `cargo` on PATH. This machine's default cargo/rustc are a separate
# Homebrew Rust install with no rustup component support, while
# llvm-tools-preview only installs into a rustup toolchain. Mixing the two
# pairs mismatched LLVM versions and produces invalid coverage data. See the
# "Test coverage" section in CLAUDE.md for the full explanation.
# Overridable so this works off an Apple-Silicon Mac, e.g.
#   make coverage COVERAGE_TOOLCHAIN=stable-x86_64-unknown-linux-gnu
COVERAGE_TOOLCHAIN ?= stable-aarch64-apple-darwin

.PHONY: help
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-16s\033[0m %s\n", $$1, $$2}'

.PHONY: build
build: ## Build the release binary (target/release/videre)
	$(CARGO) build --release

.PHONY: build-dev
build-dev: ## Build the debug binary (target/debug/videre)
	$(CARGO) build

# --no-fail-fast to match CI. Without it cargo stops at the first failing test
# binary, so one failure hides every later one - which is how a Linux-only
# failure in videre-core once masked whether the videre integration tests
# passed at all.
.PHONY: test
test: ## Run the full workspace test suite (as CI does)
	$(CARGO) test --workspace --no-fail-fast

# Not part of `test`, because it downloads real published releases over the
# network. CI runs it only when the script or its test changes.
.PHONY: test-install
test-install: ## Exercise docs/public/install end to end (needs network)
	.github/scripts/test-install.sh

.PHONY: fmt
fmt: ## Format all Rust code
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check: ## Check formatting without modifying files
	$(CARGO) fmt --all -- --check

.PHONY: lint
lint: ## Run clippy across the workspace
	$(CARGO) clippy --workspace --all-targets

.PHONY: coverage
coverage: ## Print per-file unit-test coverage (cargo-llvm-cov)
	cargo +$(COVERAGE_TOOLCHAIN) llvm-cov --workspace --summary-only

.PHONY: coverage-html
coverage-html: ## Generate an HTML coverage report at target/llvm-cov/html/index.html
	cargo +$(COVERAGE_TOOLCHAIN) llvm-cov --workspace --html

.PHONY: verify
verify: fmt-check test docs-build ## Everything CI and a release gate on: formatting, tests, docs

.PHONY: docs
docs: ## Serve the docs site at http://localhost:4321 with live reload
	yarn --cwd docs dev

.PHONY: docs-install
docs-install: ## Install the docs site's dependencies (yarn 4, run once)
	yarn --cwd docs install --immutable

.PHONY: docs-build
docs-build: ## Build the docs site into docs/dist
	yarn --cwd docs build

.PHONY: docs-og
docs-og: ## Regenerate the social card at docs/public/og.png
	yarn --cwd docs og

# :warning: This installs into ~/.cargo/bin, which usually comes first on PATH
# and will then shadow a Homebrew-installed videre, silently. `videre --version`
# keeps reporting the cargo copy however many times you `brew upgrade`. Check
# with `which videre`, and `cargo uninstall videre` to undo.
.PHONY: install
install: ## Install to ~/.cargo/bin (warning: shadows a Homebrew install, see comment)
	$(CARGO) install --path crates/videre --force

.PHONY: clean
clean: ## Remove build artifacts (cargo clean)
	$(CARGO) clean
