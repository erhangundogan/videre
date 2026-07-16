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
    pub default_path: Option<PathBuf>,
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

/// Write one path-valued key (absolutized) into <home>/config.toml, creating
/// the home dir. Unknown keys already in the file are preserved. The target
/// need not exist yet (you may set it before the first scan).
fn set_path_key(home: &Path, key: &str, value: &Path) -> Result<()> {
    let abs = std::path::absolute(value)
        .with_context(|| format!("cannot absolutize {}", value.display()))?;
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
        key.to_string(),
        toml::Value::String(abs.to_string_lossy().into_owned()),
    );
    std::fs::write(&path, toml::to_string_pretty(&table)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
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

    #[test]
    fn default_path_roundtrips_and_absolutizes() {
        let home = tmp_home("path_roundtrip");
        set_default_path(&home, Path::new("photos")).unwrap();
        let dir = load_config(&home).unwrap().default_path.unwrap();
        assert!(dir.is_absolute(), "saved path must be absolute: {}", dir.display());
        assert!(dir.ends_with("photos"));
        unset_default_path(&home).unwrap();
        assert_eq!(load_config(&home).unwrap().default_path, None);
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
