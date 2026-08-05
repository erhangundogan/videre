.DEFAULT_GOAL := help

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
	cargo build --release

.PHONY: build-dev
build-dev: ## Build the debug binary (target/debug/videre)
	cargo build

.PHONY: test
test: ## Run the full workspace test suite
	cargo test --workspace

.PHONY: fmt
fmt: ## Format all Rust code
	cargo fmt --all

.PHONY: fmt-check
fmt-check: ## Check formatting without modifying files
	cargo fmt --all -- --check

.PHONY: lint
lint: ## Run clippy across the workspace
	cargo clippy --workspace --all-targets

.PHONY: coverage
coverage: ## Print per-file unit-test coverage (cargo-llvm-cov)
	rustup run $(COVERAGE_TOOLCHAIN) cargo llvm-cov --workspace --summary-only

.PHONY: coverage-html
coverage-html: ## Generate an HTML coverage report at target/llvm-cov/html/index.html
	rustup run $(COVERAGE_TOOLCHAIN) cargo llvm-cov --workspace --html

.PHONY: install
install: ## Install the videre binary to ~/.cargo/bin via cargo install
	cargo install --path crates/videre --force

.PHONY: clean
clean: ## Remove build artifacts (cargo clean)
	cargo clean
