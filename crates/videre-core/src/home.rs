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

/// Directory holding `flock` sidecar lock files: `<home>/locks`.
///
/// Locks used to sit next to the database as `<db path>.<command>.lock`, which
/// scattered them into whatever directory the database lived in, cluttering
/// `~/.videre` for the default database, and the user's own folders for any
/// `--db` elsewhere. Collecting them here keeps that state in one place.
///
/// Only the path is computed; creating the directory is the caller's job, so
/// readers (`videre stats` probing liveness) never bring videre's home into
/// existence just by looking, same lazily-created-by-writers rule the rest of
/// the home directory follows.
pub fn locks_dir() -> Result<PathBuf> {
    Ok(videre_home()?.join("locks"))
}

#[derive(Debug, Default, PartialEq)]
pub struct Config {
    pub default_db: Option<PathBuf>,
    pub default_path: Option<PathBuf>,
    /// Embedding model id, e.g. `google/siglip-base-patch16-224`. A plain
    /// string, not a path: it must never be absolutized.
    pub default_model: Option<String>,
    /// Assumed floor read rate in MB/s, used to scale the I/O timeout to file
    /// size. Only needed on a mount slower than the default.
    pub min_read_rate_mb_s: Option<u64>,
    /// Default XMP precedence for scan/watch/import: `db`, `file`, or `newest`.
    /// A `--xmp` flag overrides it per run. Absent means `db`.
    pub xmp_precedence: Option<String>,
    /// Whether `videre watch` runs the XMP export stage each cycle. Opt-in;
    /// absent means off. `watch --export-xmp` also turns it on for that run.
    pub export_xmp_on_watch: Option<bool>,
}

/// Path of the config file inside a given home dir: <home>/config.toml.
pub fn config_path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

fn path_key(table: &toml::Table, file: &Path, key: &str) -> Result<Option<PathBuf>> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(PathBuf::from(s))),
        Some(other) => bail!(
            "malformed config {}: {} must be a string, got {}",
            file.display(),
            key,
            other.type_str()
        ),
    }
}

/// Read a positive-integer-valued key.
///
/// Stored as a TOML integer rather than a string, so it round-trips as a
/// number. Zero is rejected here rather than clamped: as a read rate it means
/// an unbounded timeout, which is the hang the timeout exists to prevent, and
/// silently substituting a different number would hide a typo in the config.
fn positive_int_key(table: &toml::Table, file: &Path, key: &str) -> Result<Option<u64>> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n > 0 => Ok(Some(*n as u64)),
        Some(toml::Value::Integer(n)) => bail!(
            "malformed config {}: {} must be greater than 0, got {}",
            file.display(),
            key,
            n
        ),
        Some(other) => bail!(
            "malformed config {}: {} must be an integer, got {}",
            file.display(),
            key,
            other.type_str()
        ),
    }
}

/// Read a boolean-valued key. A typo (a string `"true"` rather than a bare
/// `true`) is a hard error, matching the other typed keys.
fn bool_key(table: &toml::Table, file: &Path, key: &str) -> Result<Option<bool>> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::Boolean(b)) => Ok(Some(*b)),
        Some(other) => bail!(
            "malformed config {}: {} must be a boolean, got {}",
            file.display(),
            key,
            other.type_str()
        ),
    }
}

/// Read a string-valued key. Separate from `path_key` because a model id is
/// not a path and must survive verbatim.
fn string_key(table: &toml::Table, file: &Path, key: &str) -> Result<Option<String>> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => bail!(
            "malformed config {}: {} must be a string, got {}",
            file.display(),
            key,
            other.type_str()
        ),
    }
}

/// Read the `xmp_precedence` key, validating it against the known values so a
/// typo is caught at load time rather than silently ignored during a scan.
fn xmp_precedence_key(table: &toml::Table, file: &Path) -> Result<Option<String>> {
    match string_key(table, file, "xmp_precedence")? {
        None => Ok(None),
        Some(s) => {
            crate::marks::XmpPrecedence::parse(&s)
                .with_context(|| format!("malformed config {}", file.display()))?;
            Ok(Some(s))
        }
    }
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
    Ok(Config {
        default_db: path_key(&table, &path, "default_db")?,
        default_path: path_key(&table, &path, "default_path")?,
        default_model: string_key(&table, &path, "default_model")?,
        min_read_rate_mb_s: positive_int_key(&table, &path, "min_read_rate_mb_s")?,
        xmp_precedence: xmp_precedence_key(&table, &path)?,
        export_xmp_on_watch: bool_key(&table, &path, "export_xmp_on_watch")?,
    })
}

/// Whether the home came from `VIDERE_HOME` rather than the built-in default.
///
/// The distinction decides whether a config's `default_db` applies: see
/// `resolve_db`.
pub fn home_is_explicit() -> bool {
    std::env::var_os("VIDERE_HOME").is_some()
}

/// Resolution for a given home: config default_db, else <home>/hashes.db.
///
/// Does not consider `VIDERE_HOME`; callers wanting the full precedence rule
/// want `resolve_db`.
///
/// :warning: **Nothing outside this module's tests calls this, and nothing
/// should. Never use it to decide or display which database is in play.** It
/// answers "what does this home's config say", not "what will run", and the two
/// differ exactly when `VIDERE_HOME` is set. `commands/config.rs` used it for
/// its `resolved db:` line until 2026-08-24: it printed the configured path
/// while every command opened `<home>/hashes.db`, so a user was told their
/// library was at one path and then told no database existed at another, which
/// the line had never named. The warning above this one was already present and
/// was not enough on its own.
///
/// It survives only because it is `pub` in a published crate, so removing it is
/// a semver break rather than a cleanup. Delete it at the next minor bump; the
/// two tests below are its only remaining users and both belong on
/// `load_config`/`decide_db` instead.
pub fn resolve_db_in(home: &Path) -> Result<PathBuf> {
    Ok(load_config(home)?
        .default_db
        .unwrap_or_else(|| home.join("hashes.db")))
}

/// Full chain: explicit CLI path > `VIDERE_HOME` > config `default_db` >
/// `<home>/hashes.db`.
///
/// **`VIDERE_HOME` outranks the config file**, which is the ordinary
/// precedence for an environment variable against persisted settings, and it
/// was not always so. Before 0.14.1 the config won, so a home whose
/// `config.toml` named an absolute `default_db` wrote there no matter what
/// `VIDERE_HOME` said. That silently defeats the isolation the variable exists
/// to provide: every copied home carries the original's absolute path, so
/// pointing `VIDERE_HOME` at a copy still wrote into the source database.
/// Reported after a 428GB scan aimed at one home landed in another.
///
/// A divergence is announced rather than applied silently, so a deliberate
/// `default_db` is not quietly ignored either.
/// Pure precedence decision, split out so it is testable without touching the
/// process-global `VIDERE_HOME`.
///
/// Mutating that variable in a test corrupts every *other* test that resolves a
/// home concurrently - a `Mutex` protects such tests from each other but not
/// from the rest of the suite, which is exactly how an unrelated
/// `embeddings_db` test started failing. Same split as
/// `heic::resolve_qlmanage_concurrency`.
///
/// Returns the database to use, and the configured path being overridden when
/// there is one to report.
pub(crate) fn decide_db(
    home: &Path,
    home_is_explicit: bool,
    configured: Option<PathBuf>,
) -> (PathBuf, Option<PathBuf>) {
    let in_home = home.join("hashes.db");
    match (home_is_explicit, configured) {
        // The env var is the more immediate signal and outranks the file.
        (true, Some(c)) if c != in_home => (in_home, Some(c)),
        (true, _) => (in_home, None),
        (false, Some(c)) => (c, None),
        (false, None) => (in_home, None),
    }
}

pub fn resolve_db(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        // An explicit path is used verbatim and never consults home or config.
        return Ok(p.to_path_buf());
    }
    let home = videre_home()?;
    let (chosen, overridden) = decide_db(&home, home_is_explicit(), load_config(&home)?.default_db);
    {
        let in_home = chosen.clone();
        if let Some(configured) = overridden {
            eprintln!("videre: VIDERE_HOME is set, using {}", in_home.display());
            eprintln!(
                "  ignoring default_db = {} from that home's config.toml; pass --db to override",
                configured.display()
            );
        }
    }
    Ok(chosen)
}

/// Write one string-valued key into <home>/config.toml, creating the home
/// dir. Unknown keys already in the file are preserved.
fn set_string_key(home: &Path, key: &str, value: String) -> Result<()> {
    std::fs::create_dir_all(home).with_context(|| format!("create {}", home.display()))?;
    let path = config_path(home);
    let mut table: toml::Table = match std::fs::read_to_string(&path) {
        Ok(t) => t
            .parse()
            .with_context(|| format!("malformed config {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    table.insert(key.to_string(), toml::Value::String(value));
    std::fs::write(&path, toml::to_string_pretty(&table)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Write one path-valued key, absolutized. The target need not exist yet (you
/// may set it before the first scan).
fn set_path_key(home: &Path, key: &str, value: &Path) -> Result<()> {
    let abs = std::path::absolute(value)
        .with_context(|| format!("cannot absolutize {}", value.display()))?;
    set_string_key(home, key, abs.to_string_lossy().into_owned())
}

/// Remove one key from <home>/config.toml. Missing file or key is a no-op.
fn unset_key(home: &Path, key: &str) -> Result<()> {
    let path = config_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let mut table: toml::Table = text
        .parse()
        .with_context(|| format!("malformed config {}", path.display()))?;
    if table.remove(key).is_some() {
        std::fs::write(&path, toml::to_string_pretty(&table)?)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

pub fn set_default_db(home: &Path, db: &Path) -> Result<()> {
    set_path_key(home, "default_db", db)
}

pub fn unset_default_db(home: &Path) -> Result<()> {
    unset_key(home, "default_db")
}

pub fn set_default_path(home: &Path, dir: &Path) -> Result<()> {
    set_path_key(home, "default_path", dir)
}

pub fn unset_default_path(home: &Path) -> Result<()> {
    unset_key(home, "default_path")
}

/// Write one integer-valued key, as a TOML integer rather than a string so it
/// round-trips through `positive_int_key`.
fn set_int_key(home: &Path, key: &str, value: i64) -> Result<()> {
    std::fs::create_dir_all(home).with_context(|| format!("create {}", home.display()))?;
    let path = config_path(home);
    let mut table: toml::Table = match std::fs::read_to_string(&path) {
        Ok(t) => t
            .parse()
            .with_context(|| format!("malformed config {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    table.insert(key.to_string(), toml::Value::Integer(value));
    std::fs::write(&path, toml::to_string_pretty(&table)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Assumed floor read rate in MB/s. Rejects zero: as a read rate it means an
/// unbounded timeout, which is exactly the hang the timeout exists to prevent.
pub fn set_min_read_rate(home: &Path, mb_s: u64) -> Result<()> {
    if mb_s == 0 {
        bail!("min read rate must be greater than 0 MB/s");
    }
    set_int_key(home, "min_read_rate_mb_s", mb_s as i64)
}

pub fn unset_min_read_rate(home: &Path) -> Result<()> {
    unset_key(home, "min_read_rate_mb_s")
}

pub fn set_default_model(home: &Path, model_id: &str) -> Result<()> {
    set_string_key(home, "default_model", model_id.to_string())
}

pub fn unset_default_model(home: &Path) -> Result<()> {
    unset_key(home, "default_model")
}

/// Set the default XMP precedence, validating it before writing so a bad value
/// never reaches the config file.
pub fn set_xmp_precedence(home: &Path, value: &str) -> Result<()> {
    crate::marks::XmpPrecedence::parse(value)?;
    set_string_key(home, "xmp_precedence", value.to_string())
}

pub fn unset_xmp_precedence(home: &Path) -> Result<()> {
    unset_key(home, "xmp_precedence")
}

/// Write one boolean-valued key, as a TOML boolean so it round-trips through
/// `bool_key`.
fn set_bool_key(home: &Path, key: &str, value: bool) -> Result<()> {
    std::fs::create_dir_all(home).with_context(|| format!("create {}", home.display()))?;
    let path = config_path(home);
    let mut table: toml::Table = match std::fs::read_to_string(&path) {
        Ok(t) => t
            .parse()
            .with_context(|| format!("malformed config {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    table.insert(key.to_string(), toml::Value::Boolean(value));
    std::fs::write(&path, toml::to_string_pretty(&table)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn set_export_xmp_on_watch(home: &Path, value: bool) -> Result<()> {
    set_bool_key(home, "export_xmp_on_watch", value)
}

pub fn unset_export_xmp_on_watch(home: &Path) -> Result<()> {
    unset_key(home, "export_xmp_on_watch")
}

/// The configured default embedding model, if any. None means the built-in
/// default applies (see `videre_core::embeddings::DEFAULT_MODEL_ID`).
pub fn default_model() -> Result<Option<String>> {
    Ok(load_config(&videre_home()?)?.default_model)
}

/// The configured default scan/watch directory, if any (config `path` key,
/// stored as `default_path`). There is no built-in fallback: None means the
/// user must pass a directory explicitly.
pub fn default_path() -> Result<Option<PathBuf>> {
    Ok(load_config(&videre_home()?)?.default_path)
}

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
        assert_eq!(
            resolve_db_in(&home).unwrap(),
            PathBuf::from("/tmp/custom.db")
        );
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
        assert!(
            db.is_absolute(),
            "saved path must be absolute: {}",
            db.display()
        );
        assert!(db.ends_with("rel.db"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn export_xmp_on_watch_roundtrips_as_a_bool() {
        let home = tmp_home("exportxmp");
        assert_eq!(load_config(&home).unwrap().export_xmp_on_watch, None);
        set_export_xmp_on_watch(&home, true).unwrap();
        assert_eq!(load_config(&home).unwrap().export_xmp_on_watch, Some(true));
        unset_export_xmp_on_watch(&home).unwrap();
        assert_eq!(load_config(&home).unwrap().export_xmp_on_watch, None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn set_preserves_unknown_keys() {
        let home = tmp_home("preserve");
        std::fs::write(home.join("config.toml"), "future_key = \"x\"\n").unwrap();
        set_default_db(&home, Path::new("/tmp/a.db")).unwrap();
        let text = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(
            text.contains("future_key"),
            "unknown keys must survive a rewrite: {text}"
        );
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

    #[test]
    fn default_path_roundtrips_and_absolutizes() {
        let home = tmp_home("path_roundtrip");
        set_default_path(&home, Path::new("photos")).unwrap();
        let dir = load_config(&home).unwrap().default_path.unwrap();
        assert!(
            dir.is_absolute(),
            "saved path must be absolute: {}",
            dir.display()
        );
        assert!(dir.ends_with("photos"));
        unset_default_path(&home).unwrap();
        assert_eq!(load_config(&home).unwrap().default_path, None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn default_model_round_trips_verbatim_without_absolutizing() {
        // The regression that reusing set_path_key would cause: a model id
        // contains a slash, so absolutize() turns it into a filesystem path.
        let home = tmp_home("model_roundtrip");
        set_default_model(&home, "google/siglip-base-patch16-224").unwrap();
        assert_eq!(
            load_config(&home).unwrap().default_model,
            Some("google/siglip-base-patch16-224".to_string())
        );
        let text = std::fs::read_to_string(config_path(&home)).unwrap();
        assert!(
            !text.contains("/Users") && !text.contains("//"),
            "model id must be stored verbatim, got: {text}"
        );
        unset_default_model(&home).unwrap();
        assert_eq!(load_config(&home).unwrap().default_model, None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn all_three_keys_coexist_independently() {
        let home = tmp_home("three_keys");
        set_default_db(&home, Path::new("/tmp/a.db")).unwrap();
        set_default_path(&home, Path::new("/tmp/photos")).unwrap();
        set_default_model(&home, "owner/model-224").unwrap();

        let c = load_config(&home).unwrap();
        assert_eq!(c.default_db, Some(PathBuf::from("/tmp/a.db")));
        assert_eq!(c.default_path, Some(PathBuf::from("/tmp/photos")));
        assert_eq!(c.default_model, Some("owner/model-224".to_string()));

        // Unsetting one must not disturb the others.
        unset_default_model(&home).unwrap();
        let c = load_config(&home).unwrap();
        assert_eq!(c.default_db, Some(PathBuf::from("/tmp/a.db")));
        assert_eq!(c.default_path, Some(PathBuf::from("/tmp/photos")));
        assert_eq!(c.default_model, None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn default_model_is_read_as_a_plain_string() {
        let home = tmp_home("model_read");
        std::fs::write(
            config_path(&home),
            "default_model = \"google/siglip-base-patch16-224\"\n",
        )
        .unwrap();
        assert_eq!(
            load_config(&home).unwrap().default_model,
            Some("google/siglip-base-patch16-224".to_string())
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_non_string_default_model_is_a_hard_error() {
        // Same treatment as the path keys: silent fallback would mask a typo.
        let home = tmp_home("model_badtype");
        std::fs::write(config_path(&home), "default_model = 42\n").unwrap();
        let err = load_config(&home).unwrap_err();
        assert!(format!("{err:#}").contains("must be a string"), "{err:#}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn db_and_path_keys_coexist_independently() {
        let home = tmp_home("coexist");
        set_default_db(&home, Path::new("/tmp/a.db")).unwrap();
        set_default_path(&home, Path::new("/tmp/photos")).unwrap();
        let config = load_config(&home).unwrap();
        assert_eq!(config.default_db, Some(PathBuf::from("/tmp/a.db")));
        assert_eq!(config.default_path, Some(PathBuf::from("/tmp/photos")));
        // unsetting one must not disturb the other
        unset_default_db(&home).unwrap();
        let config = load_config(&home).unwrap();
        assert_eq!(config.default_db, None);
        assert_eq!(config.default_path, Some(PathBuf::from("/tmp/photos")));
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(test)]
mod read_rate_tests {
    use super::*;

    fn home() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "videre-rate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_rate_round_trips_as_a_number_not_a_string() {
        // Stored as a TOML string it would parse back as the wrong type and
        // the key would silently do nothing.
        let h = home();
        set_min_read_rate(&h, 50).unwrap();
        let raw = std::fs::read_to_string(config_path(&h)).unwrap();
        assert!(raw.contains("min_read_rate_mb_s = 50"), "got: {raw}");
        assert_eq!(load_config(&h).unwrap().min_read_rate_mb_s, Some(50));
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn zero_is_refused_rather_than_written() {
        // A zero rate means an unbounded timeout, which is the hang the
        // timeout exists to prevent.
        let h = home();
        assert!(set_min_read_rate(&h, 0).is_err());
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_zero_already_in_the_file_is_rejected_on_read() {
        // Hand-edited configs exist; the reader cannot trust the writer.
        let h = home();
        std::fs::write(config_path(&h), "min_read_rate_mb_s = 0\n").unwrap();
        assert!(load_config(&h).is_err());
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_non_integer_is_rejected_with_a_clear_error() {
        let h = home();
        std::fs::write(config_path(&h), "min_read_rate_mb_s = \"fast\"\n").unwrap();
        let e = load_config(&h).unwrap_err().to_string();
        assert!(e.contains("must be an integer"), "got: {e}");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn absent_means_the_built_in_default_applies() {
        let h = home();
        assert_eq!(load_config(&h).unwrap().min_read_rate_mb_s, None);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn unset_removes_it() {
        let h = home();
        set_min_read_rate(&h, 33).unwrap();
        unset_min_read_rate(&h).unwrap();
        assert_eq!(load_config(&h).unwrap().min_read_rate_mb_s, None);
        let _ = std::fs::remove_dir_all(&h);
    }
}

#[cfg(test)]
mod db_precedence_tests {
    use super::*;

    // These test `decide_db` rather than setting VIDERE_HOME, deliberately.
    // The variable is process-global, so mutating it from a test corrupts every
    // other test resolving a home at the same moment; an earlier version of
    // this module did exactly that and made an unrelated embeddings_db test
    // fail. A pure function needs no such coordination.

    #[test]
    fn videre_home_outranks_a_config_default_db() {
        // The reported bug: a home copied from another carries the original's
        // absolute default_db, so pointing VIDERE_HOME at the copy still wrote
        // into the source. A 428GB scan landed in the wrong database this way.
        let home = Path::new("/homes/copy");
        let configured = Some(PathBuf::from("/homes/original/hashes.db"));
        let (chosen, overridden) = decide_db(home, true, configured.clone());
        assert_eq!(chosen, home.join("hashes.db"));
        assert_eq!(
            overridden, configured,
            "the ignored setting must be reportable"
        );
    }

    #[test]
    fn without_the_env_var_the_config_still_wins() {
        // Unchanged for anyone not using VIDERE_HOME: `videre config set db`
        // behaves exactly as before.
        let home = Path::new("/homes/default");
        let configured = Some(PathBuf::from("/elsewhere/hashes.db"));
        let (chosen, overridden) = decide_db(home, false, configured);
        assert_eq!(chosen, PathBuf::from("/elsewhere/hashes.db"));
        assert!(overridden.is_none(), "nothing was overridden");
    }

    #[test]
    fn no_config_falls_back_to_the_home_either_way() {
        let home = Path::new("/homes/x");
        assert_eq!(decide_db(home, true, None).0, home.join("hashes.db"));
        assert_eq!(decide_db(home, false, None).0, home.join("hashes.db"));
    }

    #[test]
    fn a_config_naming_the_home_database_is_not_a_divergence() {
        // Nothing to report when both agree, or every command would print a
        // notice about a setting that changes nothing.
        let home = Path::new("/homes/x");
        let same = Some(home.join("hashes.db"));
        assert!(decide_db(home, true, same).1.is_none());
    }
}
