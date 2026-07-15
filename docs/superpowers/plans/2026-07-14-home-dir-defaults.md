# `~/.videre` Home Directory and Default Database Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every subcommand resolves its database automatically (explicit path > config.toml `default_db` > `$VIDERE_HOME/hashes.db`), so `videre dedupe ~/Photos` followed by bare `videre report` / `videre search "sunset"` just works.

**Architecture:** Per the spec at `docs/superpowers/specs/2026-07-14-home-dir-defaults-design.md`. A new `videre_core::home` module owns path resolution and config load/save. A `resolve_reader_db` helper in `commands/mod.rs` applies the reader policy (defaulted paths must exist, friendly error otherwise; explicit paths keep today's per-command semantics). Six reader commands swap their `db` positional for a `--db` flag; dedupe defaults to SQLite at the resolved db (JSONL only via `--output`, whose bare form targets `$VIDERE_HOME/hashes.jsonl`); watch's `--output-sqlite` becomes optional; a new tenth subcommand `videre config` shows/sets/unsets the default db.

**Tech Stack:** Rust; new deps `anyhow = "1"` and `toml = "0.8"` in `videre-core` (currently only `half` + `rusqlite`). `std::path::absolute` (stable, toolchain is 1.96). Baseline: `cargo test --workspace` = 143 passing on `main` at `c6c968f`. Expected after all tasks: 159.

**Approved breaking changes (the only three):** (1) `db` positional becomes `--db` on report/fix-dates/prune/embed/search/faces; (2) bare `videre dedupe <dir>` writes SQLite to the resolved default db instead of JSONL to `/tmp/hashes`; (3) `watch --output-sqlite` becomes optional. Everything else (flags, stdout/stderr text, exit codes, `--json` documents) stays byte-identical, including all behavior when an explicit path is given.

**House rules:** never use the em dash character anywhere; no Co-Authored-By trailer or "Generated with" line in commits; use the exact commit messages given.

**Branch:** work on a new branch `home-defaults` off `main`:

```bash
cd /Users/erhangundogan/projects/rust/videre
git checkout -b home-defaults
```

---

### Task 1: `videre_core::home` module

**Files:**
- Modify: `crates/videre-core/Cargo.toml`, `crates/videre-core/src/lib.rs`
- Create: `crates/videre-core/src/home.rs`

- [ ] **Step 1: Add dependencies and module**

In `crates/videre-core/Cargo.toml`, `[dependencies]` becomes:

```toml
[dependencies]
anyhow = "1"
half = "2"
rusqlite = { version = "0.32", features = ["bundled"] }
toml = "0.8"
```

In `crates/videre-core/src/lib.rs`, insert `pub mod home;` between `pub mod heic;` and `pub mod location;` (the list is alphabetical).

- [ ] **Step 2: Write the failing unit tests**

Create `crates/videre-core/src/home.rs` containing ONLY the test module first (the functions come in Step 4), so you can watch the tests fail to compile:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn tmp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("videre_home_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_config_yields_defaults() {
        let home = tmp_home("missing");
        assert_eq!(load_config(&home).unwrap(), Config::default());
        assert_eq!(resolve_db_in(&home).unwrap(), home.join("hashes.db"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn config_default_db_wins_over_builtin_default() {
        let home = tmp_home("wins");
        set_default_db(&home, Path::new("/tmp/custom.db")).unwrap();
        assert_eq!(resolve_db_in(&home).unwrap(), PathBuf::from("/tmp/custom.db"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn explicit_path_wins_verbatim() {
        // Explicit paths never consult home or config.
        assert_eq!(
            resolve_db(Some(Path::new("/x/y.db"))).unwrap(),
            PathBuf::from("/x/y.db")
        );
    }

    #[test]
    fn set_default_db_absolutizes_relative_paths() {
        let home = tmp_home("abs");
        set_default_db(&home, Path::new("rel.db")).unwrap();
        let db = load_config(&home).unwrap().default_db.unwrap();
        assert!(db.is_absolute(), "saved path must be absolute: {}", db.display());
        assert!(db.ends_with("rel.db"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn set_preserves_unknown_keys() {
        let home = tmp_home("preserve");
        std::fs::write(home.join("config.toml"), "future_key = \"x\"\n").unwrap();
        set_default_db(&home, Path::new("/tmp/a.db")).unwrap();
        let text = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(text.contains("future_key"), "unknown keys must survive a rewrite: {text}");
        assert!(text.contains("default_db"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unset_removes_key_and_is_noop_when_missing() {
        let home = tmp_home("unset");
        unset_default_db(&home).unwrap(); // no file: no-op, Ok
        set_default_db(&home, Path::new("/tmp/a.db")).unwrap();
        unset_default_db(&home).unwrap();
        assert_eq!(load_config(&home).unwrap(), Config::default());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn malformed_config_is_error() {
        let home = tmp_home("malformed");
        std::fs::write(home.join("config.toml"), "not = = toml").unwrap();
        let err = load_config(&home).unwrap_err();
        assert!(format!("{err:#}").contains("malformed config"), "{err:#}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
```

Note the tests never mutate process environment variables (`videre_home()`'s env reading is covered by the integration tests in later tasks, where the child process gets `VIDERE_HOME` in its own environment). This avoids env races between parallel test threads.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p videre-core home::`
Expected: COMPILE ERROR (`load_config`, `Config`, `resolve_db_in`, `resolve_db`, `set_default_db`, `unset_default_db` not found).

- [ ] **Step 4: Implement**

Prepend to `crates/videre-core/src/home.rs` (above the test module):

```rust
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Root of videre's per-user state: $VIDERE_HOME if set, else $HOME/.videre.
pub fn videre_home() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os("VIDERE_HOME") {
        return Ok(PathBuf::from(h));
    }
    match std::env::var_os("HOME") {
        Some(h) => Ok(PathBuf::from(h).join(".videre")),
        None => bail!("cannot locate videre home: neither VIDERE_HOME nor HOME is set"),
    }
}

/// Default JSONL output path (used by `dedupe --output` with no value).
pub fn default_jsonl() -> Result<PathBuf> {
    Ok(videre_home()?.join("hashes.jsonl"))
}

#[derive(Debug, Default, PartialEq)]
pub struct Config {
    pub default_db: Option<PathBuf>,
}

fn config_path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

/// Load <home>/config.toml. A missing file is the default config; a file that
/// does not parse is a hard error (silent fallback would mask typos).
pub fn load_config(home: &Path) -> Result<Config> {
    let path = config_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let table: toml::Table = text
        .parse()
        .with_context(|| format!("malformed config {}", path.display()))?;
    let default_db = match table.get("default_db") {
        None => None,
        Some(toml::Value::String(s)) => Some(PathBuf::from(s)),
        Some(other) => bail!(
            "malformed config {}: default_db must be a string, got {}",
            path.display(),
            other.type_str()
        ),
    };
    Ok(Config { default_db })
}

/// Resolution for a given home: config default_db, else <home>/hashes.db.
pub fn resolve_db_in(home: &Path) -> Result<PathBuf> {
    Ok(load_config(home)?
        .default_db
        .unwrap_or_else(|| home.join("hashes.db")))
}

/// Full chain: explicit CLI path > config default_db > <home>/hashes.db.
/// Explicit paths are used verbatim and never consult home or config.
pub fn resolve_db(explicit: Option<&Path>) -> Result<PathBuf> {
    match explicit {
        Some(p) => Ok(p.to_path_buf()),
        None => resolve_db_in(&videre_home()?),
    }
}

/// Write default_db (absolutized) into <home>/config.toml, creating the home
/// dir. Unknown keys already in the file are preserved. The db need not exist
/// yet (you may set it before the first scan).
pub fn set_default_db(home: &Path, db: &Path) -> Result<()> {
    let abs = std::path::absolute(db)
        .with_context(|| format!("cannot absolutize {}", db.display()))?;
    std::fs::create_dir_all(home).with_context(|| format!("create {}", home.display()))?;
    let path = config_path(home);
    let mut table: toml::Table = match std::fs::read_to_string(&path) {
        Ok(t) => t
            .parse()
            .with_context(|| format!("malformed config {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    table.insert(
        "default_db".to_string(),
        toml::Value::String(abs.to_string_lossy().into_owned()),
    );
    std::fs::write(&path, toml::to_string_pretty(&table)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Remove default_db from <home>/config.toml. Missing file or key is a no-op.
pub fn unset_default_db(home: &Path) -> Result<()> {
    let path = config_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let mut table: toml::Table = text
        .parse()
        .with_context(|| format!("malformed config {}", path.display()))?;
    if table.remove("default_db").is_some() {
        std::fs::write(&path, toml::to_string_pretty(&table)?)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p videre-core home::`
Expected: PASS (7 tests).

Run: `cargo test --workspace`
Expected: PASS, 150 total (143 baseline + 7).

- [ ] **Step 6: Commit**

```bash
git add crates/videre-core/Cargo.toml crates/videre-core/src/lib.rs crates/videre-core/src/home.rs Cargo.lock
git commit -m "feat: videre_core::home module with VIDERE_HOME resolution and config.toml load/save"
```

---

### Task 2: `resolve_reader_db` helper + `videre config` subcommand

**Files:**
- Modify: `crates/videre/src/commands/mod.rs`, `crates/videre/src/main.rs`
- Create: `crates/videre/src/commands/config.rs`
- Test: `crates/videre/tests/config.rs` (new file)

- [ ] **Step 1: Write the failing integration tests**

Create `crates/videre/tests/config.rs`:

```rust
use std::process::Command;
use tempfile::tempdir;

fn videre_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

#[test]
fn config_show_works_with_empty_home() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre config");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("default_db: (not set)"), "{stdout}");
    assert!(stdout.contains("hashes.db"), "{stdout}");
}

#[test]
fn config_set_and_unset_db_roundtrip() {
    let home = tempdir().unwrap();
    let set = Command::new(videre_bin())
        .arg("config").arg("set").arg("db").arg("/tmp/custom.db")
        .env("VIDERE_HOME", home.path())
        .status()
        .expect("failed to run videre config set");
    assert!(set.success());
    assert!(home.path().join("config.toml").exists());

    let show = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(stdout.contains("default_db: /tmp/custom.db"), "{stdout}");

    let unset = Command::new(videre_bin())
        .arg("config").arg("unset").arg("db")
        .env("VIDERE_HOME", home.path())
        .status()
        .unwrap();
    assert!(unset.success());
    let show2 = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&show2.stdout).contains("default_db: (not set)"));
}

#[test]
fn config_set_rejects_unknown_key() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("config").arg("set").arg("nope").arg("/x")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "unknown config key must be rejected");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --test config`
Expected: all 3 FAIL (clap: unrecognized subcommand `config`).

- [ ] **Step 3: Implement the config command and the reader helper**

Create `crates/videre/src/commands/config.rs`:

```rust
use anyhow::Result;
use std::path::PathBuf;
use videre_core::home;

#[derive(clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(clap::Subcommand)]
enum ConfigAction {
    /// Set a config key (keys: db)
    Set {
        #[arg(value_parser = ["db"])]
        key: String,
        value: PathBuf,
    },
    /// Remove a config key (keys: db)
    Unset {
        #[arg(value_parser = ["db"])]
        key: String,
    },
}

pub fn run(args: ConfigArgs) -> Result<()> {
    let home = home::videre_home()?;
    match args.action {
        None => show(&home),
        Some(ConfigAction::Set { value, .. }) => home::set_default_db(&home, &value),
        Some(ConfigAction::Unset { .. }) => home::unset_default_db(&home),
    }
}

fn show(home: &std::path::Path) -> Result<()> {
    let config_file = home.join("config.toml");
    let config = home::load_config(home)?;
    println!("home:        {}", home.display());
    println!(
        "config:      {}{}",
        config_file.display(),
        if config_file.exists() { "" } else { " (absent)" }
    );
    match &config.default_db {
        Some(db) => println!("default_db: {}", db.display()),
        None => println!("default_db: (not set)"),
    }
    println!("resolved db: {}", home::resolve_db_in(home)?.display());
    println!("jsonl:       {}", home.join("hashes.jsonl").display());
    Ok(())
}
```

In `crates/videre/src/commands/mod.rs`, add `pub mod config;` at the top of the module list (it sorts first alphabetically) and the shared reader helper at the bottom:

```rust
/// Reader-side db resolution. Explicit paths keep their command's existing
/// semantics untouched; defaulted paths must already exist (SQLite would
/// otherwise create an empty db on open and silently serve an empty library).
pub(crate) fn resolve_reader_db(
    explicit: Option<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    match explicit {
        Some(p) => Ok(p),
        None => {
            let db = videre_core::home::resolve_db(None)?;
            anyhow::ensure!(
                db.exists(),
                "no database found at {}; run 'videre dedupe <dir>' first",
                db.display()
            );
            Ok(db)
        }
    }
}
```

In `crates/videre/src/main.rs`, add to the `Command` enum (after `Watch`):

```rust
    /// Show or edit videre's config and default paths (~/.videre)
    Config(commands::config::ConfigArgs),
```

and the dispatch arm:

```rust
        Command::Config(args) => commands::config::run(args),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --test config`
Expected: PASS (3 tests).

Run: `cargo test --workspace`
Expected: PASS, 153 total (150 + 3).

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/mod.rs crates/videre/src/commands/config.rs crates/videre/src/main.rs crates/videre/tests/config.rs
git commit -m "feat: videre config subcommand and shared reader db resolution"
```

---

### Task 3: `--db` on fix-dates, prune, embed, faces

**Files:**
- Modify: `crates/videre/src/commands/fix_dates.rs`, `crates/videre/src/commands/prune.rs`, `crates/videre/src/commands/embed.rs`, `crates/videre/src/commands/faces.rs`
- Test: `crates/videre/tests/prune.rs`, `crates/videre/tests/faces_pipeline.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/videre/tests/prune.rs`. The file already has a binary-path helper (the
`current_exe()` pop/pop/push pattern used by every test file here); read its actual name
first (it may be `videre_bin`, `bin`, or `prune_bin`) and use that name where this code says
`videre_bin`:

```rust
#[test]
fn missing_default_db_prints_friendly_error() {
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(videre_bin())
        .arg("prune")
        .arg("--dry-run")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre prune");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no database found at"), "{stderr}");
    assert!(stderr.contains("videre dedupe"), "{stderr}");
}
```

- [ ] **Step 2: Run tests to verify the new one fails**

Run: `cargo test -p videre --test prune`
Expected: the new test FAILS (today a bare `prune --dry-run` is a clap error, "required argument DB", whose message does not contain "no database found"). The 6 existing tests still pass.

- [ ] **Step 3: Convert the four commands**

In each of the four files, the `db: PathBuf` field (with its doc comment) becomes:

```rust
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,
```

At the top of each `run()`, resolve first, then use the local `db` everywhere `args.db` was used (mechanical replace within the function and any helpers that received `&args.db`):

```rust
    let db = super::resolve_reader_db(args.db.clone())?;
```

(If `args` is consumed rather than borrowed in a file, `args.db.clone()` can be `args.db.take()`-style; simplest is to destructure or clone, matching what compiles cleanly. Behavior requirement: explicit `--db` paths flow through untouched.)

Per-file notes:
- `fix_dates.rs` and `prune.rs` and `report.rs` (report is Task 4) keep their existing `if !db.exists() { eprintln!("Error: {:?} does not exist", db); std::process::exit(1); }` check, now operating on the resolved `db`. For defaulted paths this is unreachable (the helper already checked); for explicit paths it preserves today's exact stderr text and exit code.
- `faces.rs` keeps its `anyhow::bail!("{:?} does not exist", db)` check the same way.
- `embed.rs` has no existence check today; do not add one for explicit paths (preserves today's semantics). The helper covers the defaulted case.

- [ ] **Step 4: Port the test spawn sites**

- `crates/videre/tests/prune.rs`: the `run_prune` helper's `cmd.arg("prune").arg(db)` becomes `cmd.arg("prune").arg("--db").arg(db)`. Grep the file for any other `.arg("prune")` spawn and treat it the same.
- `crates/videre/tests/faces_pipeline.rs`: both spawn sites `.arg("faces").arg(&db)` become `.arg("faces").arg("--db").arg(&db)`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p videre --test prune --test faces_pipeline`
Expected: PASS (7 prune + 2 faces_pipeline).

Run: `cargo test --workspace`
Expected: PASS, 154 total (153 + 1).

Verify by hand: `cargo run -q -p videre --bin videre -- prune --help` shows `--db <DB>` as an option (not a positional) and the default note; same for `fix-dates`, `embed`, `faces`.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/fix_dates.rs crates/videre/src/commands/prune.rs crates/videre/src/commands/embed.rs crates/videre/src/commands/faces.rs crates/videre/tests/prune.rs crates/videre/tests/faces_pipeline.rs
git commit -m "feat: fix-dates, prune, embed, faces take --db and resolve the default database"
```

---

### Task 4: `--db` on search and report

**Files:**
- Modify: `crates/videre/src/commands/search.rs`, `crates/videre/src/commands/report.rs`
- Test: `crates/videre/tests/person_search.rs`, `crates/videre/tests/report.rs`, `crates/videre/tests/faces_server.rs`

- [ ] **Step 1: Write the failing integration tests**

Append to `crates/videre/tests/person_search.rs`:

```rust
#[test]
fn search_json_missing_default_db_yields_json_error() {
    let home = tempdir().unwrap();
    let out = Command::new(bin())
        .arg("search")
        .arg("beach")
        .arg("--json")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("stdout must be one valid JSON object even on error");
    assert_eq!(doc["schema_version"], 1);
    let msg = doc["error"]["message"].as_str().unwrap();
    assert!(msg.contains("no database found"), "{msg}");
}

#[test]
fn config_default_db_redirects_bare_search_and_explicit_db_wins() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let home = tempdir().unwrap();

    let set = Command::new(bin())
        .arg("config").arg("set").arg("db").arg(&db)
        .env("VIDERE_HOME", home.path())
        .status()
        .unwrap();
    assert!(set.success());

    // bare search resolves the configured db
    let out = Command::new(bin())
        .arg("search").arg("--person").arg("Alice")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("/tmp/alice1.jpg"));

    // explicit --db wins for one invocation and does not modify config
    let cfg_before = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let dir2 = tempdir().unwrap();
    let db2 = make_db(dir2.path());
    let out2 = Command::new(bin())
        .arg("search").arg("--db").arg(&db2).arg("--person").arg("Bob")
        .env("VIDERE_HOME", home.path())
        .output()
        .unwrap();
    assert!(out2.status.success());
    assert!(String::from_utf8_lossy(&out2.stdout).contains("/tmp/bob.jpg"));
    let cfg_after = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert_eq!(cfg_before, cfg_after, "explicit --db must not modify config");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --test person_search`
Expected: the 2 new tests FAIL (with today's CLI, `search beach --json` parses "beach" as the db positional and errors differently, and `config set` inside the second test hits the missing subcommand from Task 2 only if Task 2 was skipped; with Task 2 done, the failure is the search misparse). The 7 existing tests still pass at this point; they are ported in Step 4 because Step 3's CLI change breaks their positional-db spawns.

- [ ] **Step 3: Convert search and report**

`crates/videre/src/commands/search.rs`:
- The `db: PathBuf` field becomes:

```rust
    /// SQLite database with embeddings (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,
```

- `collect_hits` resolves first (this routes the missing-db failure through the existing error channels: text mode gets main's `error: ...` line, `--json` mode gets the JSON error object):

```rust
fn collect_hits(args: &SearchArgs) -> Result<(QueryJson, Vec<SearchHitJson>)> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)
        .with_context(|| format!("open {}", db.display()))?;
    ...
```

- The empty-corpus `ensure!` message swaps `args.db.display()` for `db.display()`. Nothing else in the file changes.

`crates/videre/src/commands/report.rs`:
- The `db: PathBuf` field becomes the same `#[arg(long)] db: Option<PathBuf>` shape with doc comment `/// SQLite database (default: resolved from ~/.videre; see 'videre config')`.
- Top of `run()`:

```rust
    let db = super::resolve_reader_db(args.db.clone())?;
```

- Then mechanically replace every other use of `args.db` in the file with `db` (existence check, default output name construction, db opens; grep `args.db` to find them all). The `-o/--output` default stays `<db>_report.html` computed from the resolved path.

- [ ] **Step 4: Port the test spawn sites**

Insert `.arg("--db")` immediately before the db argument at every spawn site:
- `crates/videre/tests/person_search.rs`: 7 existing sites of `.arg("search")` followed by `.arg(&db)` (grep `arg(&db)`).
- `crates/videre/tests/report.rs`: `cmd.arg("report").arg(db)` (in the run helper, line ~78) and the direct spawns near lines 182-195.
- `crates/videre/tests/faces_server.rs`: 3 sites of `.arg("report")` followed by `.arg(&db)`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p videre --test person_search --test report --test faces_server`
Expected: PASS (9 person_search + 10 report + 5 faces_server).

Run: `cargo test --workspace`
Expected: PASS, 156 total (154 + 2).

Verify by hand: `cargo run -q -p videre --bin videre -- search --help` shows `--db` and no DB positional; `videre search "sunset"` against an empty `VIDERE_HOME` prints the friendly error.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/search.rs crates/videre/src/commands/report.rs crates/videre/tests/person_search.rs crates/videre/tests/report.rs crates/videre/tests/faces_server.rs
git commit -m "feat: search and report take --db and resolve the default database"
```

---

### Task 5: writer defaults for dedupe and watch

**Files:**
- Modify: `crates/videre/src/commands/dedupe.rs`, `crates/videre/src/commands/watch.rs`
- Test: `crates/videre/tests/integration.rs`, `crates/videre/tests/watch.rs`

- [ ] **Step 1: Write the failing integration tests**

Append to `crates/videre/tests/integration.rs`:

```rust
#[test]
fn bare_dedupe_writes_default_sqlite_db() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // REMOVE candidate still printed to stdout (pipe contract intact)
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);

    let db = home.path().join("hashes.db");
    assert!(db.exists(), "bare dedupe must create the default db");
    assert!(!home.path().join("hashes.jsonl").exists(), "no jsonl by default");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn bare_output_flag_writes_default_jsonl() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    // The bare --output must come AFTER the directory: clap's optional-value
    // arg would otherwise consume the directory as the flag's value.
    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg(scan_dir.path())
        .arg("--output")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let jsonl = home.path().join("hashes.jsonl");
    assert!(jsonl.exists(), "bare --output must target the default jsonl");
    assert_eq!(fs::read_to_string(&jsonl).unwrap().lines().count(), 1);
    assert!(!home.path().join("hashes.db").exists(), "no sqlite db when --output used");
}
```

Also add `.env("VIDERE_HOME", ...)` isolation to the two existing tests that spawn dedupe without any output flag, so they can never touch the real `~/.videre`:
- `missing_directory_exits_nonzero`: add a `let home = tempdir().unwrap();` and `.env("VIDERE_HOME", home.path())` on the spawn.
- `json_error_object_for_missing_directory`: same treatment.

(Both fail on the missing directory before any output resolution happens, but the isolation makes that fact irrelevant to safety.)

Append to `crates/videre/tests/watch.rs` a bare-watch test. Mirror the file's existing scan-stage test (same tempdir setup, same spawn/wait/kill pattern and helpers), with two changes: no `--output-sqlite` argument, and `.env("VIDERE_HOME", home.path())` on the spawn (where `home` is a fresh `tempdir()`). Assert that `home.path().join("hashes.db")` appears and gains `file_hashes` rows within the same timeout the neighboring test uses, then kill the child as that test does.

- [ ] **Step 2: Run tests to verify the new ones fail**

Run: `cargo test -p videre --test integration --test watch`
Expected: `bare_dedupe_writes_default_sqlite_db` FAILS (today bare dedupe writes JSONL to /tmp/hashes, creates no db); `bare_output_flag_writes_default_jsonl` FAILS (today `--output` requires a value: clap error); the bare-watch test FAILS (today `--output-sqlite` is required). All previously existing tests still pass.

- [ ] **Step 3: Implement dedupe's output target**

In `crates/videre/src/commands/dedupe.rs`:

The two output fields become:

```rust
    /// JSONL output file (appended). Bare --output targets ~/.videre/hashes.jsonl.
    /// Note: place a bare --output AFTER the directory. Cannot be used with --output-sqlite
    #[arg(long, num_args = 0..=1, conflicts_with = "output_sqlite")]
    output: Option<Option<PathBuf>>,

    /// SQLite output file (upserted by path). When neither --output nor
    /// --output-sqlite is given, records go to the resolved default db
    #[arg(long)]
    output_sqlite: Option<PathBuf>,
```

Add below `gather_records`:

```rust
enum OutputTarget {
    Sqlite(PathBuf),
    Jsonl(PathBuf),
}

/// Where records go. Explicit flags behave exactly as before; the bare default
/// is SQLite at the resolved db, and a bare --output is JSONL at the default
/// jsonl path. Defaulted destinations get their parent dir created (that is
/// how ~/.videre comes into existence on first use).
fn output_target(args: &DedupeArgs) -> anyhow::Result<OutputTarget> {
    if let Some(ref db) = args.output_sqlite {
        return Ok(OutputTarget::Sqlite(db.clone()));
    }
    match &args.output {
        Some(Some(path)) => Ok(OutputTarget::Jsonl(path.clone())),
        Some(None) => {
            let path = videre_core::home::default_jsonl()?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(OutputTarget::Jsonl(path))
        }
        None => {
            let db = videre_core::home::resolve_db(None)?;
            if let Some(parent) = db.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(OutputTarget::Sqlite(db))
        }
    }
}
```

In `run_text`, replace the whole `if let Some(ref db_path) = args.output_sqlite { ... } else { ... }` persist block with:

```rust
    match output_target(&args) {
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
        Ok(OutputTarget::Sqlite(db_path)) => {
            if let Err(e) = sqlite_output::write_records(&records, &db_path) {
                eprintln!("Error writing to {:?}: {}", db_path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
            }
        }
        Ok(OutputTarget::Jsonl(path)) => {
            if let Err(e) = output::append_records(&records, &path) {
                eprintln!("Error writing to {:?}: {}", path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), path);
            }
        }
    }
```

In `run_json`, replace the equivalent persist block with:

```rust
    match output_target(args)? {
        OutputTarget::Sqlite(db_path) => {
            sqlite_output::write_records(&records, &db_path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))?;
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
            }
        }
        OutputTarget::Jsonl(path) => {
            output::append_records(&records, &path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", path, e))?;
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), path);
            }
        }
    }
```

Explicit-flag invocations keep byte-identical stderr text ("Error writing to ...", "Wrote N record(s) to ..."); only the bare-default destination changes, which is the approved break.

- [ ] **Step 4: Implement watch's optional --output-sqlite**

In `crates/videre/src/commands/watch.rs`:

```rust
    /// SQLite database to populate (same file videre report reads).
    /// Default: resolved from ~/.videre; see 'videre config'
    #[arg(long)]
    output_sqlite: Option<PathBuf>,
```

At the top of `run()`, resolve once (watch is a writer: create the parent dir for defaulted paths):

```rust
    let db: PathBuf = match &args.output_sqlite {
        Some(p) => p.clone(),
        None => {
            let db = videre_core::home::resolve_db(None)?;
            if let Some(parent) = db.parent() {
                std::fs::create_dir_all(parent)?;
            }
            db
        }
    };
```

Then thread `&db` through: `run_cycle` and every stage helper that currently reads `args.output_sqlite` gains a `db: &Path` parameter, and those reads become `db`. Grep the file for `output_sqlite` to find every use; the change is mechanical (the helpers already take `&WatchArgs`, so add the second parameter alongside).

- [ ] **Step 5: Run tests**

Run: `cargo test -p videre --test integration --test watch`
Expected: PASS (11 integration + 6 watch).

Run: `cargo test --workspace`
Expected: PASS, 159 total (156 + 3).

Verify by hand that explicit forms are unchanged:

```bash
D=$(mktemp -d); H=$(mktemp -d); printf same > "$D/a.jpg"; printf same > "$D/b.jpg"
VIDERE_HOME=$H cargo run -q -p videre --bin videre -- dedupe --silent --output /tmp/hd_check.jsonl "$D"
# expected: one REMOVE path on stdout; /tmp/hd_check.jsonl written; no $H/hashes.db
VIDERE_HOME=$H cargo run -q -p videre --bin videre -- dedupe --silent "$D" | head -1
# expected: one REMOVE path on stdout; $H/hashes.db now exists
rm -rf "$D" "$H" /tmp/hd_check.jsonl
```

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/dedupe.rs crates/videre/src/commands/watch.rs crates/videre/tests/integration.rs crates/videre/tests/watch.rs
git commit -m "feat: dedupe defaults to the resolved sqlite db; watch --output-sqlite optional"
```

---

### Task 6: Documentation

**Files:**
- Modify: `README.md`, `CLAUDE.md`

- [ ] **Step 1: Update README.md**

Read the file first and match its style. Changes:
- Quickstart and all command examples lead with the bare forms (`videre dedupe ~/Photos`, then bare `videre report`, `videre search "sunset on beach"`, `videre faces`, `videre watch ~/Photos`), with `--db`/`--output-sqlite` shown as the explicit-path variants.
- New section (near the top, after the quickstart) titled `## The ~/.videre home directory` covering: the layout (`hashes.db`, `hashes.jsonl`, `config.toml`), the resolution order (explicit > `config.toml` `default_db` > `~/.videre/hashes.db`), `VIDERE_HOME` override, lazy creation by writers, the reader error (`no database found at ...; run 'videre dedupe <dir>' first`), and `videre config` / `videre config set db <path>` / `videre config unset db`.
- The dedupe flags list: `--output` documented with its optional value and the token-order caveat (bare `--output` goes after the directory); `--output-sqlite` documented as the explicit target with the bare default being the resolved db.
- The six reader commands' examples switch from positional db to `--db`.
- A short "Breaking changes" note listing the three deliberate breaks for anyone upgrading.

- [ ] **Step 2: Update CLAUDE.md**

- The Usage block for dedupe reflects the new `--output [path]` / `--output-sqlite` semantics and default.
- The "Build & run" example list switches to bare forms with `--db` variants.
- The "Output behavior" section: bare dedupe writes SQLite to the resolved default db; JSONL only with `--output`.
- New short section documenting `~/.videre`, `VIDERE_HOME`, the resolution order, and `videre config` (mirror the README content, condensed).
- The project structure section adds `commands/config.rs` and `videre-core/src/home.rs`.
- Mention the subcommand count is now ten (dedupe, report, fix-dates, prune, embed, search, faces, watch, config, and mcp when it lands; write "nine" plus config however the current text counts: verify against the actual current wording and keep it accurate).

- [ ] **Step 3: Verify and commit**

```bash
cargo run -q -p videre --bin videre -- --help | grep -E 'config'
cargo run -q -p videre --bin videre -- config --help
```

Both must succeed and match the documented wording. Then:

```bash
git add README.md CLAUDE.md
git commit -m "docs: document ~/.videre home directory, default db resolution, and videre config"
```

---

### Task 7: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full suite**

Run: `cargo test --workspace`
Expected: PASS, 159 tests, 0 failed.

- [ ] **Step 2: Release-binary smoke test**

```bash
cargo build --release
H=$(mktemp -d); D=$(mktemp -d); printf same > "$D/a.jpg"; printf same > "$D/b.jpg"
export VIDERE_HOME=$H

./target/release/videre config                       # shows (not set) + resolved paths
./target/release/videre report; echo "exit=$?"       # friendly 'no database found' error, exit 1
./target/release/videre dedupe --silent "$D"         # one REMOVE path; creates $H/hashes.db
ls "$H"                                              # hashes.db present, no jsonl
./target/release/videre report                       # writes ${H}/hashes.db_report.html
./target/release/videre prune --dry-run              # runs against the default db
./target/release/videre search "beach"; echo "exit=$?"   # 'no embeddings ... run videre embed first', exit 1 (proves resolution)
./target/release/videre config set db "$H/hashes.db"
./target/release/videre config                       # default_db now set
./target/release/videre config unset db

unset VIDERE_HOME; rm -rf "$H" "$D"
```

Each command's outcome must match the comment. The real `~/.videre` must not exist afterward unless it existed before (check before/after if unsure).

- [ ] **Step 3: Record results**

PASS/FAIL per step; any FAIL loops back to the owning task.
