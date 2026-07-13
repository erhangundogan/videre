# Videre Rename + Single-Binary Restructure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename the project from `dupe` to `videre` and consolidate its 8 separate binaries into one `videre` binary with a clap subcommand tree (`videre dedupe`, `videre report`, `videre search`, ...), with zero behavior change per command.

**Architecture:** Three mechanical phases. (1) Rename crates/dirs/imports while keeping all 8 old binary names working, so tests stay green through the rename itself. (2) Break the future dependency cycle by moving the `watch` binary out of `videre-ml` (the unified binary must depend on `videre-ml`, and `videre-ml` currently depends on the `videre` lib only because of that one binary). (3) Convert one binary per task into a subcommand module under `crates/videre/src/commands/`, deleting the old `[[bin]]` and porting its integration tests to `videre <subcommand>` invocation in the same task, so every commit is green and reviewable.

**Tech Stack:** Rust workspace (`videre`, `videre-core`, `videre-ml`), `clap` derive (`Subcommand`/`Args`), existing `rusqlite`/`axum`/`candle`/`ort` stack. No new dependencies.

**Behavior contract:** Every subcommand keeps its old flags, defaults, stdout/stderr output, and exit codes byte-identical. `videre dedupe ~/Photos | xargs trash` must behave exactly as `dupe ~/Photos | xargs trash` does today. The SQLite schema, `~/.cache/ort/`, and the HuggingFace cache are untouched. The only deliberate behavior change is the thumbnail cache directory (Task 9), which gains a one-time migration.

**Repo context:** This repo (`/Users/erhangundogan/projects/rust/videre`) is a fresh clone of the old `dupe` repo with full history; `origin` has been removed and will be pointed at a new GitHub repo after this plan lands. Work directly on a feature branch in this repo.

---

### Task 1: Workspace rename (dirs, packages, imports) with old binary names intact

**Files:**
- Rename: `crates/dupe` -> `crates/videre`, `crates/dupe-core` -> `crates/videre-core`, `crates/dupe-ml` -> `crates/videre-ml`
- Modify: root `Cargo.toml`, all three crate `Cargo.toml`s, every `.rs` file importing `dupe::`/`dupe_core::`/`dupe_ml::`

- [ ] **Step 1: Create a working branch**

```bash
cd /Users/erhangundogan/projects/rust/videre
git checkout -b restructure
```

- [ ] **Step 2: Rename the crate directories**

```bash
git mv crates/dupe crates/videre
git mv crates/dupe-core crates/videre-core
git mv crates/dupe-ml crates/videre-ml
```

- [ ] **Step 3: Update the workspace members**

Edit root `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/videre", "crates/videre-core", "crates/videre-ml"]
```

- [ ] **Step 4: Rename the packages and path dependencies**

In `crates/videre/Cargo.toml`: `name = "dupe"` -> `name = "videre"`, and the `[lib] name = "dupe"` -> `name = "videre"`. Update `dupe-core = { path = "../dupe-core" }` -> `videre-core = { path = "../videre-core" }`. Leave every `[[bin]]` section exactly as it is (names `dupe`, `dupe-report`, `dupe-fix-dates`, `dupe-prune` still build from the same paths).

In `crates/videre-core/Cargo.toml`: `name = "dupe-core"` -> `name = "videre-core"`.

In `crates/videre-ml/Cargo.toml`: `name = "dupe-ml"` -> `name = "videre-ml"`; `dupe-core = { path = "../dupe-core" }` -> `videre-core = { path = "../videre-core" }`; `dupe = { path = "../dupe" }` -> `videre = { path = "../videre" }` (this dependency is removed entirely in Task 2). `[[bin]]` sections stay (`dupe-embed`, `dupe-search`, `dupe-faces`, `dupe-watch`).

- [ ] **Step 5: Rewrite imports across all Rust sources**

macOS sed (BSD) with word boundaries via `[[:<:]]`/`[[:>:]]`:

```bash
grep -rl 'dupe_core' --include='*.rs' crates/ | xargs sed -i '' 's/[[:<:]]dupe_core[[:>:]]/videre_core/g'
grep -rl 'dupe_ml' --include='*.rs' crates/ | xargs sed -i '' 's/[[:<:]]dupe_ml[[:>:]]/videre_ml/g'
grep -rl 'use dupe::' --include='*.rs' crates/ | xargs sed -i '' 's/use dupe::/use videre::/g'
```

Then verify nothing is left: `grep -rn 'dupe_core\|dupe_ml\|use dupe::' --include='*.rs' crates/` must return nothing. Do NOT touch string literals containing "dupe" (binary names in `#[command(name = ...)]`, cache paths, help text, eprintln output); those change in their own tasks.

- [ ] **Step 6: Build and test**

Run: `cargo test --workspace`
Expected: PASS, same counts as baseline. All 8 binaries still build under their old names (`cargo build --workspace && ls target/debug | grep -E '^dupe'` shows all 8).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: rename workspace crates dupe/dupe-core/dupe-ml to videre/videre-core/videre-ml"
```

---

### Task 2: Move the watch binary out of videre-ml (dependency-cycle prevention)

The unified `videre` binary (Task 3 onward) must depend on `videre-ml` for embed/search/faces. `videre-ml` currently depends on the `videre` lib solely because `dupe-watch.rs` lives in it. Cargo forbids crate cycles, so the watch binary moves first.

**Files:**
- Move: `crates/videre-ml/src/bin/dupe-watch.rs` -> `crates/videre/src/bin/dupe_watch.rs`
- Move: `crates/videre-ml/tests/watch.rs` -> `crates/videre/tests/watch.rs`
- Modify: `crates/videre/Cargo.toml`, `crates/videre-ml/Cargo.toml`

- [ ] **Step 1: Move the binary source and test file**

```bash
git mv crates/videre-ml/src/bin/dupe-watch.rs crates/videre/src/bin/dupe_watch.rs
git mv crates/videre-ml/tests/watch.rs crates/videre/tests/watch.rs
```

- [ ] **Step 2: Update Cargo.toml on both sides**

Remove from `crates/videre-ml/Cargo.toml`: the `[[bin]] name = "dupe-watch"` section, the `videre = { path = "../videre" }` dependency, and `chrono` IF `grep -rn chrono crates/videre-ml/src --include='*.rs'` shows the lib itself doesn't use it (the watch binary was its only user; verify before removing).

Add to `crates/videre/Cargo.toml`:

```toml
[[bin]]
name = "dupe-watch"
path = "src/bin/dupe_watch.rs"
```

and to its `[dependencies]`: `videre-ml = { path = "../videre-ml" }` (needed by the watch binary's `videre_ml::pipeline` imports; also needed by Tasks 6-8).

- [ ] **Step 3: Fix imports in the moved file**

`dupe_watch.rs` imports its own crate's scan modules via `use videre::{hasher, scanner, sqlite_output, types};` - since it now lives inside the `videre` crate as a bin target, that import form still works unchanged (bins link against their own package's lib by name). Verify it compiles as-is; adjust only if the compiler objects.

- [ ] **Step 4: Confirm the dependency DAG is now acyclic**

Run: `cargo tree -p videre-ml | grep -c videre-ml` and confirm `cargo tree -p videre-ml` shows no `videre` (only `videre-core`).

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --workspace`
Expected: PASS. The watch integration tests now run from `crates/videre/tests/watch.rs` against the same `dupe-watch` binary name (the `watch_bin()` helper's `current_exe().pop().pop()` lookup is unchanged).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: move watch binary into videre crate to break the videre-ml -> videre dependency"
```

---

### Task 3: `videre` binary skeleton + `dedupe` subcommand

**Files:**
- Create: `crates/videre/src/commands/mod.rs`, `crates/videre/src/commands/dedupe.rs`
- Rewrite: `crates/videre/src/main.rs`
- Modify: `crates/videre/Cargo.toml`, `crates/videre/tests/integration.rs`

- [ ] **Step 1: Move main.rs content into the command module**

Create `crates/videre/src/commands/mod.rs`:

```rust
pub mod dedupe;
```

Create `crates/videre/src/commands/dedupe.rs` containing everything currently in `src/main.rs` EXCEPT `fn main`, with these transformations:
- `#[derive(Parser)] #[command(name = "dupe", version, about = ...)] struct Args` becomes `#[derive(clap::Args)] pub struct DedupeArgs` (field attributes like `#[arg(long, conflicts_with = ...)]` stay identical; the doc comments on fields stay, they become the flag help text).
- The body of the old `fn main()` becomes `pub fn run(args: DedupeArgs) -> anyhow::Result<()>` with `Ok(())` at the end. Internal `process::exit` calls (if any) stay as-is to preserve exit codes. All helper functions in the file move along unchanged, private to the module.
- `use clap::Parser;` becomes unnecessary; remove it. Keep `use videre::{hasher, output, scanner, sqlite_output, types};` and the rest.

- [ ] **Step 2: Write the new main.rs**

Replace `crates/videre/src/main.rs` entirely:

```rust
use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(
    name = "videre",
    version,
    about = "Local-first media library toolkit: dedupe, semantic search, faces, and reports over one SQLite database"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a directory, hash every image, and print duplicate paths to stdout
    Dedupe(commands::dedupe::DedupeArgs),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Dedupe(args) => commands::dedupe::run(args),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 3: Swap the bin target**

In `crates/videre/Cargo.toml`, replace the `[[bin]] name = "dupe"` section with:

```toml
[[bin]]
name = "videre"
path = "src/main.rs"
```

Add `anyhow = "1"` to `[dependencies]` if not already present (check first).

- [ ] **Step 4: Port the integration tests**

In `crates/videre/tests/integration.rs`, the binary lookup helper currently resolves the `dupe` binary; change it to resolve `videre`, and insert `"dedupe"` as the first argument at every spawn site, e.g. `Command::new(bin()).arg(&dir)` becomes `Command::new(bin()).arg("dedupe").arg(&dir)`. Read the actual helper in the file first; keep its `current_exe()` pattern.

- [ ] **Step 5: Run the ported tests, then the full suite**

Run: `cargo test -p videre --test integration`
Expected: PASS (all 6).

Run: `cargo test --workspace`
Expected: PASS.

Also verify by hand: `cargo run -p videre --bin videre -- dedupe --help` prints the same flags the old `dupe --help` had, and `cargo run -p videre --bin videre -- nonsense` exits with clap's "unrecognized subcommand" error listing valid ones.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: videre single binary with subcommand tree; dedupe is the first subcommand"
```

---

### Task 4: `report` subcommand

**Files:**
- Move: `crates/videre/src/bin/dupe_report.rs` -> `crates/videre/src/commands/report.rs`
- Modify: `crates/videre/src/commands/mod.rs`, `crates/videre/src/main.rs`, `crates/videre/Cargo.toml`, `crates/videre/tests/report.rs`, `crates/videre/tests/faces_server.rs`

- [ ] **Step 1: Move and adapt the file**

`git mv crates/videre/src/bin/dupe_report.rs crates/videre/src/commands/report.rs`, then:
- `#[derive(Parser)] struct Args` -> `#[derive(clap::Args)] pub struct ReportArgs` (drop any `#[command(name = "dupe-report", ...)]` attribute; keep all field attributes).
- The old `fn main()` becomes `pub fn run(args: ReportArgs) -> anyhow::Result<()>`. Read the actual current main first: if it is `#[tokio::main]` or builds a runtime for server mode, preserve that by having `run` construct the runtime explicitly (`tokio::runtime::Runtime::new()?.block_on(...)`) for the server path, keeping the static-report path fully synchronous. If main currently returns `Result<(), Box<dyn Error>>`, map the error into `anyhow::Error` at the boundary rather than rewriting internal signatures.
- The file's `#[cfg(test)] mod tests` moves along unchanged and now runs as unit tests of the `videre` bin target.

- [ ] **Step 2: Wire into the tree**

Add `pub mod report;` to `commands/mod.rs`; add to the enum and dispatch in `main.rs`:

```rust
    /// Generate an HTML review page, or serve the live report/labeling UI
    Report(commands::report::ReportArgs),
```

```rust
        Command::Report(args) => commands::report::run(args),
```

Remove the `[[bin]] name = "dupe-report"` section from `crates/videre/Cargo.toml`.

- [ ] **Step 3: Port the tests**

In `crates/videre/tests/report.rs` and `crates/videre/tests/faces_server.rs`, change the binary helper to resolve `videre` and prepend `"report"` to the argument list at each spawn site (e.g. `.arg(&db)` becomes `.arg("report").arg(&db)`; flags like `--faces`, `--all`, `--show-faces` follow unchanged).

- [ ] **Step 4: Run the ported tests, then the full suite**

Run: `cargo test -p videre --test report --test faces_server`
Expected: PASS (every test in both files; take the counts from the pre-change baseline run and confirm they match).

Run: `cargo test --workspace`
Expected: PASS. Confirm `target/debug/dupe-report` no longer exists after `cargo build --workspace`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: report subcommand replaces dupe-report binary"
```

---

### Task 5: `fix-dates` and `prune` subcommands

**Files:**
- Move: `crates/videre/src/bin/dupe_fix_dates.rs` -> `crates/videre/src/commands/fix_dates.rs`
- Move: `crates/videre/src/bin/dupe_prune.rs` -> `crates/videre/src/commands/prune.rs`
- Modify: `crates/videre/src/commands/mod.rs`, `crates/videre/src/main.rs`, `crates/videre/Cargo.toml`, `crates/videre/tests/prune.rs`

- [ ] **Step 1: Move and adapt both files**

Same pattern as Task 4: `Args` -> `pub struct FixDatesArgs` / `pub struct PruneArgs` with `#[derive(clap::Args)]`; `main()` -> `pub fn run(args: ...) -> anyhow::Result<()>`. Both binaries exit with code 1 on partial failure via `process::exit(1)`; keep those calls verbatim inside `run` so exit-code behavior is unchanged. `prune.rs` uses `.expect("failed to open database")` on open; keep it.

- [ ] **Step 2: Wire into the tree**

`commands/mod.rs` adds `pub mod fix_dates;` and `pub mod prune;`. Enum additions (clap derives kebab-case automatically, so `FixDates` becomes the `fix-dates` subcommand):

```rust
    /// Set each file's mtime to its EXIF shoot date
    FixDates(commands::fix_dates::FixDatesArgs),
    /// Remove stale rows, sync metadata, clean orphan embeddings
    Prune(commands::prune::PruneArgs),
```

Dispatch arms follow the same pattern as Task 4. Remove both `[[bin]]` sections. The `crates/videre/src/bin/` directory now contains only `dupe_watch.rs`.

- [ ] **Step 3: Port the prune tests**

`crates/videre/tests/prune.rs`: binary helper -> `videre`, prepend `"prune"` at spawn sites. `fix-dates` has no dedicated integration test file (verify with `ls crates/videre/tests/`); its behavior is covered by manual verification in Task 11.

- [ ] **Step 4: Run tests**

Run: `cargo test -p videre --test prune` then `cargo test --workspace`
Expected: PASS (6 prune tests; full suite green). `videre fix-dates --help` and `videre prune --help` show the old flags.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: fix-dates and prune subcommands replace their binaries"
```

---

### Task 6: `embed` and `search` subcommands

**Files:**
- Move: `crates/videre-ml/src/bin/dupe-embed.rs` -> `crates/videre/src/commands/embed.rs`
- Move: `crates/videre-ml/src/bin/dupe-search.rs` -> `crates/videre/src/commands/search.rs`
- Move: `crates/videre-ml/tests/person_search.rs` -> `crates/videre/tests/person_search.rs`
- Modify: `crates/videre/src/commands/mod.rs`, `crates/videre/src/main.rs`, `crates/videre-ml/Cargo.toml`

- [ ] **Step 1: Move and adapt both files**

Same pattern: `EmbedArgs`/`SearchArgs`, `run(args) -> anyhow::Result<()>`. Their imports (`videre_ml::{device, model, preprocess}`, `videre_core::{embeddings, vectors}`, etc.) already resolve since `videre` depends on both crates (Task 2). `dupe-search.rs` may declare mutually exclusive query modes via clap groups; keep every attribute identical.

- [ ] **Step 2: Wire into the tree**

```rust
    /// Compute SigLIP embeddings for every image in the database
    Embed(commands::embed::EmbedArgs),
    /// Search images by text, example image, or person name
    Search(commands::search::SearchArgs),
```

Remove both `[[bin]]` sections from `crates/videre-ml/Cargo.toml`. Check whether `rayon`, `anyhow`, or `clap` are now unused by `videre-ml`'s remaining lib code (`grep -rn 'rayon\|anyhow::\|clap' crates/videre-ml/src --include='*.rs'`) and remove any dependency that has no remaining user; leave anything the lib still uses.

- [ ] **Step 3: Port the person-search test**

`git mv crates/videre-ml/tests/person_search.rs crates/videre/tests/person_search.rs`; binary helper -> `videre`, spawn args gain `"search"` first (e.g. `.arg("search").arg(&db).arg("--person").arg("Alice")`).

- [ ] **Step 4: Run tests**

Run: `cargo test -p videre --test person_search` then `cargo test --workspace`
Expected: PASS (3 person-search tests; full suite green). `videre search --help` and `videre embed --help` match old flags.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: embed and search subcommands replace their binaries"
```

---

### Task 7: `faces` subcommand

**Files:**
- Move: `crates/videre-ml/src/bin/dupe-faces.rs` -> `crates/videre/src/commands/faces.rs`
- Move: `crates/videre-ml/tests/faces_pipeline.rs` -> `crates/videre/tests/faces_pipeline.rs`
- Possibly move: `crates/videre-ml/tests/fixtures/` (check what references it first)
- Modify: `crates/videre/src/commands/mod.rs`, `crates/videre/src/main.rs`, `crates/videre-ml/Cargo.toml`

- [ ] **Step 1: Move and adapt**

Same pattern: `FacesArgs`, `run(args) -> anyhow::Result<()>`. The file's hash-selection logic and calls into `videre_ml::pipeline::{run_face_pipeline, run_clustering}` move unchanged.

- [ ] **Step 2: Wire into the tree**

```rust
    /// Detect, embed, and cluster faces; enables person search
    Faces(commands::faces::FacesArgs),
```

Remove the `[[bin]]` section. Before moving `tests/fixtures/`, run `grep -rn 'fixtures' crates/videre-ml/tests/ crates/videre/tests/` to see which test files reference it and by what path (`CARGO_MANIFEST_DIR`-relative paths break when a test moves crates); move the fixtures dir with the tests that use it and fix any path constants.

- [ ] **Step 3: Port the pipeline test**

`faces_pipeline.rs`: binary helper -> `videre`, prepend `"faces"` at spawn sites.

- [ ] **Step 4: Run tests**

Run: `cargo test -p videre --test faces_pipeline` then `cargo test --workspace`
Expected: PASS (2 tests; full suite green).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: faces subcommand replaces dupe-faces binary"
```

---

### Task 8: `watch` subcommand (last binary retired)

**Files:**
- Move: `crates/videre/src/bin/dupe_watch.rs` -> `crates/videre/src/commands/watch.rs`
- Delete: `crates/videre/src/bin/` (now empty)
- Modify: `crates/videre/src/commands/mod.rs`, `crates/videre/src/main.rs`, `crates/videre/Cargo.toml`, `crates/videre/tests/watch.rs`

- [ ] **Step 1: Move and adapt**

Same pattern: `WatchArgs` (directory positional, `--output-sqlite`, the four stage flags, `--interval`, `--silent` all unchanged), `run(args) -> anyhow::Result<()>` containing the existing infinite loop (it never returns Ok; that is correct and unchanged). Its helpers (`run_cycle`, `run_scan_stage`, `run_faces_stage`, `run_heic_stage`, `run_location_stage`, `dedup_paths_by_hash`, `file_hashes_table_exists`) move along as private functions.

- [ ] **Step 2: Wire into the tree, remove the last old bin**

```rust
    /// Background loop keeping scan/faces/HEIC-cache/location data fresh
    Watch(commands::watch::WatchArgs),
```

Remove the `[[bin]] name = "dupe-watch"` section and delete the now-empty `src/bin/` directory. After `cargo build --workspace`, `ls target/debug | grep -E '^dupe'` must show nothing; the only project binary is `videre`.

- [ ] **Step 3: Port the watch tests**

`crates/videre/tests/watch.rs`: rename `watch_bin()` to `videre_bin()` resolving `videre`, and every spawn gains `"watch"` as the first arg (before the directory positional). All 5 tests (scan/faces/heic/location/fresh-db) otherwise unchanged.

- [ ] **Step 4: Run tests**

Run: `cargo test -p videre --test watch` then `cargo test --workspace`
Expected: PASS (5 watch tests; full suite green).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: watch subcommand replaces dupe-watch; all eight binaries now consolidated"
```

---

### Task 9: Thumbnail cache path rename with one-time migration

**Files:**
- Modify: `crates/videre-core/src/thumb_cache.rs`, `crates/videre/src/main.rs`

- [ ] **Step 1: Write the failing tests**

Add to `thumb_cache.rs`'s test module:

```rust
    #[test]
    fn cache_dir_is_under_videre() {
        assert!(cache_dir().to_string_lossy().contains("videre"));
        assert!(!cache_dir().to_string_lossy().contains("/dupe/"));
    }

    #[test]
    fn migrate_dir_moves_old_into_place() {
        let tmp = std::env::temp_dir().join(format!("thumb_migrate_{}", std::process::id()));
        let old = tmp.join("old_cache");
        let new = tmp.join("new_cache");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("h_240.jpg"), b"x").unwrap();
        migrate_dir(&old, &new);
        assert!(new.join("h_240.jpg").exists(), "cached file must survive migration");
        assert!(!old.exists(), "old dir must be gone after migration");
        let _ = std::fs::remove_dir_all(&tmp);
    }
```

Run: `cargo test -p videre-core thumb_cache::`
Expected: FAIL (`cache_dir` still says dupe; `migrate_dir` undefined).

- [ ] **Step 2: Implement**

In `thumb_cache.rs`: `cache_dir()` becomes `dirs_cache_dir().join("videre").join("thumbnails")`. Add:

```rust
/// One-time migration from the pre-rename cache location. Thumbnails are
/// content-hash keyed and expensive to regenerate for large HEIC libraries,
/// so a rename of the tool should not orphan them. Only fires when the old
/// dir exists and the new one does not; a plain rename, so it is atomic on
/// the same filesystem and a no-op on any error (cache regenerates lazily).
pub fn migrate_legacy_dupe_cache() {
    let old = dirs_cache_dir().join("dupe").join("thumbnails");
    let new = cache_dir();
    migrate_dir(&old, &new);
}

fn migrate_dir(old: &std::path::Path, new: &std::path::Path) {
    if old.is_dir() && !new.exists() {
        if let Some(parent) = new.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::rename(old, new);
        if let Some(old_parent) = old.parent() {
            let _ = std::fs::remove_dir(old_parent); // only removes if empty
        }
    }
}
```

In `crates/videre/src/main.rs`, call it once before dispatch: `videre_core::thumb_cache::migrate_legacy_dupe_cache();` as the first line of `main` after `Cli::parse()`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p videre-core thumb_cache::` then `cargo test --workspace`
Expected: PASS (4 thumb_cache tests; full suite green).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: thumbnail cache moves to ~/.cache/videre with one-time migration from ~/.cache/dupe"
```

---

### Task 10: Documentation rewrite

**Files:**
- Modify: `README.md`, `CLAUDE.md`

- [ ] **Step 1: Rewrite README.md**

Project title/intro become videre ("a local-first media library toolkit"; dedupe is the first-listed capability, not the identity). The Binaries table becomes a Subcommands table (`videre dedupe`, `report`, `fix-dates`, `prune`, `embed`, `search`, `faces`, `watch` with their existing one-line purposes). Every command example switches to subcommand form (`dupe --output-sqlite ~/photos.db ~/Photos` -> `videre dedupe --output-sqlite ~/photos.db ~/Photos`, and so on for all ~20 examples including the quickstart). Install section: `cargo build --release` produces the single `./target/release/videre`. Platform-notes table collapses to per-subcommand rows. Mention the thumbnail-cache path is now `~/.cache/videre/thumbnails/` with automatic migration from the old location. Keep the schema/EXIF/dHash reference sections with only invocation-syntax updates.

- [ ] **Step 2: Rewrite CLAUDE.md**

Same substitutions, plus: Project structure section reflects the new layout (`crates/videre/src/{main.rs,commands/*.rs,lib-modules}`, `crates/videre-core`, `crates/videre-ml`, single `[[bin]]`); the SQLite schema and WAL sections keep their content with `dupe-watch`/`dupe-report` references becoming `videre watch`/`videre report`. Add one line noting `docs/superpowers/` specs/plans predate the rename and use the old `dupe-*` names historically (do not rewrite those files).

- [ ] **Step 3: Verify no stale references and commit**

`grep -rn 'dupe-' README.md CLAUDE.md` should only match intentional historical notes. Then:

```bash
git add README.md CLAUDE.md
git commit -m "docs: rewrite README and CLAUDE.md for videre single-binary subcommand CLI"
```

---

### Task 11: End-to-end verification

**Files:** none (verification only)

- [ ] **Step 1: Release build and help-tree check**

`cargo build --release`. Then `./target/release/videre --help` must list all 8 subcommands; `./target/release/videre dedupe --help` (and each other subcommand) must show the same flags as the old binaries. `./target/release/videre bogus` must print clap's suggestion error.

- [ ] **Step 2: Real fixture end-to-end**

Using HEIC fixtures (e.g. from `/Library/User Pictures/Flowers/`): `videre dedupe --output-sqlite <db> <dir>` (duplicate printed to stdout), `videre report <db>` (HTML generated), `videre fix-dates <db> --dry-run`, `videre prune <db> --dry-run`, one `videre watch <dir> --output-sqlite <db> --interval 3600` cycle (kill after stages complete; confirm thumbnails land under `~/.cache/videre/thumbnails/`), then `videre report <db> --show-faces` and curl `/api/raw?path=...&size=240` to confirm a cache hit.

- [ ] **Step 3: Cache migration check**

Create a fake `~/.cache/dupe/thumbnails/test_240.jpg`, remove `~/.cache/videre/thumbnails`, run any `videre` subcommand (`videre dedupe --help` is enough since migration runs pre-dispatch), and confirm the file now sits under `~/.cache/videre/thumbnails/` and the old dir is gone. Clean up the fake entry afterward.

- [ ] **Step 4: Record results**

PASS/FAIL per step; any FAIL loops back to the owning task before the branch is considered done.
