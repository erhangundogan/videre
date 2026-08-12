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
    })
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
