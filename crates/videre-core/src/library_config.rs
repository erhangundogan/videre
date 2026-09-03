//! Library-scoped settings: the type, its built-in defaults, and the
//! `config.toml` under the library's reserved state directory.
//!
//! `db` and `jsonl` are fixed declarations, not relocation settings: if
//! present they must equal the exact filenames the paths layer already
//! derives, so a config copied from another library can never redirect a
//! process into that library. A missing file means the defaults and creates
//! nothing; a file that fails validation, or does not parse, is an error
//! rather than a silent fallback, because a typo in the config must surface,
//! not vanish. Unknown keys (including nested tables) are preserved by
//! edits, which rewrite by renaming a synced scratch file into place. An
//! I/O failure leaves the prior bytes unchanged; a timeout abandons the
//! writing thread instead, and that thread may still complete the rename
//! after the error has returned.

use crate::embeddings::{validate_model_id, DEFAULT_MODEL_ID};
use crate::library::{bounded_op, root_cause_is_not_found, LibraryContext, LibraryPaths};
use crate::marks::XmpPrecedence;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Settings governing how one library is processed.
///
/// Absent settings mean the built-in default, mirroring the global config's
/// convention where a missing key falls back rather than erroring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryConfig {
    /// Embedding model id, e.g. `google/siglip-base-patch16-224`. A plain
    /// string, not a path: it must never be absolutized, the same rule the
    /// global config's `default_model` follows.
    pub default_model: String,
    /// How a mark read from a file's XMP reconciles with the db on
    /// scan/watch/import; the default is db wins and XMP fills the gaps.
    pub xmp_precedence: XmpPrecedence,
    /// Whether `videre watch` runs the XMP export stage each cycle. Opt-in:
    /// absent means off, matching the global config.
    pub export_xmp_on_watch: bool,
    /// Assumed floor read rate in MB/s used to scale I/O timeouts to file
    /// size; `None` means the built-in default applies
    /// (`io_timeout::MIN_READ_RATE_MB_S_DEFAULT`).
    pub min_read_rate_mb_s: Option<u64>,
}

impl Default for LibraryConfig {
    /// The built-in defaults: the built-in embedding model, db-first XMP
    /// precedence, no export on watch, and the timeout floor left at its
    /// built-in value.
    fn default() -> Self {
        Self {
            default_model: DEFAULT_MODEL_ID.to_string(),
            xmp_precedence: XmpPrecedence::default(),
            export_xmp_on_watch: false,
            min_read_rate_mb_s: None,
        }
    }
}

/// Which supported setting an [`edit`] addresses.
///
/// The fixed declarations `db` and `jsonl` are deliberately absent from
/// this vocabulary: they are not settings, and no edit can redirect library
/// storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKey {
    /// `default_model`, validated by `embeddings::validate_model_id`.
    Model,
    /// `min_read_rate_mb_s`, a positive integer or absent.
    ReadRate,
    /// `xmp_precedence`, one of the spellings `XmpPrecedence::parse` knows.
    Xmp,
    /// `export_xmp_on_watch`, a boolean.
    ExportXmpOnWatch,
}

impl ConfigKey {
    /// The serialized key this variant addresses.
    fn name(self) -> &'static str {
        match self {
            ConfigKey::Model => "default_model",
            ConfigKey::ReadRate => "min_read_rate_mb_s",
            ConfigKey::Xmp => "xmp_precedence",
            ConfigKey::ExportXmpOnWatch => "export_xmp_on_watch",
        }
    }
}

/// The serialized spelling of one precedence value. `XmpPrecedence` has no
/// `Display`; a total match here means a new variant fails to compile
/// rather than serializing as the wrong setting.
fn xmp_precedence_str(p: XmpPrecedence) -> &'static str {
    match p {
        XmpPrecedence::Db => "db",
        XmpPrecedence::File => "file",
        XmpPrecedence::Newest => "newest",
    }
}

/// The table a first edit writes: the two fixed storage declarations and
/// every supported setting at its built-in default, so a fresh library's
/// config documents the storage names instead of leaving them implicit.
/// Only ever the starting point when no file exists; defaults are never
/// written over an existing file.
fn initial_table() -> toml::Table {
    let defaults = LibraryConfig::default();
    let mut table = toml::Table::new();
    table.insert("db".into(), toml::Value::String("hashes.db".into()));
    table.insert("jsonl".into(), toml::Value::String("hashes.jsonl".into()));
    table.insert(
        "default_model".into(),
        toml::Value::String(defaults.default_model),
    );
    table.insert(
        "xmp_precedence".into(),
        toml::Value::String(xmp_precedence_str(defaults.xmp_precedence).into()),
    );
    table.insert(
        "export_xmp_on_watch".into(),
        toml::Value::Boolean(defaults.export_xmp_on_watch),
    );
    table
}

/// Read one string-valued setting; absent means the built-in `default`.
/// A value of the wrong type is a hard error, matching the global config's
/// readers: silent fallback would mask a typo.
fn string_setting(table: &toml::Table, file: &Path, key: &str, default: &str) -> Result<String> {
    match table.get(key) {
        None => Ok(default.to_string()),
        Some(toml::Value::String(s)) => Ok(s.clone()),
        Some(other) => bail!(
            "malformed config {}: {key} must be a string, got {}",
            file.display(),
            other.type_str()
        ),
    }
}

/// Read one boolean-valued setting; absent means the built-in `default`.
/// A string `"true"` where a bare `true` belongs is the typo this catches.
fn bool_setting(table: &toml::Table, file: &Path, key: &str, default: bool) -> Result<bool> {
    match table.get(key) {
        None => Ok(default),
        Some(toml::Value::Boolean(b)) => Ok(*b),
        Some(other) => bail!(
            "malformed config {}: {key} must be a boolean, got {}",
            file.display(),
            other.type_str()
        ),
    }
}

/// Read `min_read_rate_mb_s`: absent, or a positive integer. Zero is
/// rejected rather than clamped: as a read rate it means an unbounded
/// timeout, which is the hang the timeout exists to prevent, and silently
/// substituting a different number would hide a typo (the same rule the
/// global config's `positive_int_key` states).
fn read_rate_setting(table: &toml::Table, file: &Path) -> Result<Option<u64>> {
    const KEY: &str = "min_read_rate_mb_s";
    match table.get(KEY) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n > 0 => Ok(Some(*n as u64)),
        Some(toml::Value::Integer(n)) => bail!(
            "malformed config {}: {KEY} must be greater than 0, got {n}",
            file.display()
        ),
        Some(other) => bail!(
            "malformed config {}: {KEY} must be an integer, got {}",
            file.display(),
            other.type_str()
        ),
    }
}

/// Refuse one way a local config could try to move the library's storage:
/// a `db` or `jsonl` key that does not equal the exact fixed filename.
fn validate_fixed(table: &toml::Table, key: &str, expected: &str) -> Result<()> {
    if let Some(value) = table.get(key) {
        anyhow::ensure!(
            value.as_str() == Some(expected),
            "{key} must be {expected:?}; library storage cannot be redirected"
        );
    }
    Ok(())
}

/// Refuse every way a local config could try to move the library's storage.
///
/// `db` and `jsonl` are fixed declarations relative to the state directory,
/// not settings: if present they must equal the exact filenames the paths
/// layer already derives, so a config copied from another library can never
/// redirect a process into that library, and their absence resolves to the
/// same filenames. `default_db` and `default_path` are the removed global
/// settings; in a local config they can only be a copy-paste mistake, and
/// interpreting them as paths would reintroduce the redirect this layout
/// exists to make impossible. Runs before any setting is read, so a storage
/// error is raised before any library work rather than during it.
fn validate_storage(table: &toml::Table) -> Result<()> {
    validate_fixed(table, "db", "hashes.db")?;
    validate_fixed(table, "jsonl", "hashes.jsonl")?;
    for key in ["default_db", "default_path"] {
        anyhow::ensure!(!table.contains_key(key), "remove obsolete setting {key}");
    }
    Ok(())
}

/// Validate a whole parsed config table into settings.
///
/// Every supported key is validated, whether or not the caller is about to
/// consume it, so one load answers for the whole file and an invalid value
/// surfaces here, at the entrance, rather than mid-command. Absent keys
/// resolve to the built-in defaults, the same convention the global config
/// follows.
fn config_from_table(table: &toml::Table, file: &Path) -> Result<LibraryConfig> {
    validate_storage(table).with_context(|| format!("malformed config {}", file.display()))?;
    let default_model = string_setting(table, file, "default_model", DEFAULT_MODEL_ID)?;
    validate_model_id(&default_model)
        .with_context(|| format!("malformed config {}", file.display()))?;
    let xmp_default = xmp_precedence_str(XmpPrecedence::default());
    let xmp_precedence =
        XmpPrecedence::parse(&string_setting(table, file, "xmp_precedence", xmp_default)?)
            .with_context(|| format!("malformed config {}", file.display()))?;
    Ok(LibraryConfig {
        default_model,
        xmp_precedence,
        export_xmp_on_watch: bool_setting(table, file, "export_xmp_on_watch", false)?,
        min_read_rate_mb_s: read_rate_setting(table, file)?,
    })
}

/// Read the config file, bounded: a library root can sit on a volume that
/// stopped responding, and an unbounded read there would hang the very
/// command that is only trying to start up. `Ok(None)` means absent, which
/// is the only non-error way to have no config.
fn read_config(path: &Path) -> Result<Option<String>> {
    let owned = path.to_path_buf();
    match bounded_op(path, "read", crate::io_timeout::STAT_TIMEOUT, move || {
        std::fs::read_to_string(owned)
    }) {
        Ok(text) => Ok(Some(text)),
        Err(e) if root_cause_is_not_found(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Load the library's config: built-in defaults when the file is absent
/// (creating nothing), an error when it is corrupt or fails validation.
/// Never a fallback on top of bytes that are there.
pub fn load(paths: &LibraryPaths) -> Result<LibraryConfig> {
    let path = &paths.config;
    let table = match read_config(path)? {
        None => return Ok(LibraryConfig::default()),
        Some(text) => text
            .parse::<toml::Table>()
            .with_context(|| format!("malformed config {}", path.display()))?,
    };
    config_from_table(&table, path)
}

/// Check one incoming value against its key's rules before it can reach
/// the file. The shapes mirror the load-time readers: what `load` would
/// reject, `edit` must refuse to write, or the file and the edit disagree.
fn validate_value(key: ConfigKey, value: &toml::Value) -> Result<()> {
    match (key, value) {
        (ConfigKey::Model, toml::Value::String(s)) => validate_model_id(s),
        (ConfigKey::ReadRate, toml::Value::Integer(n)) if *n > 0 => Ok(()),
        (ConfigKey::Xmp, toml::Value::String(s)) => XmpPrecedence::parse(s).map(|_| ()),
        (ConfigKey::ExportXmpOnWatch, toml::Value::Boolean(_)) => Ok(()),
        (ConfigKey::ReadRate, toml::Value::Integer(n)) => {
            bail!("min_read_rate_mb_s must be greater than 0, got {n}")
        }
        (ConfigKey::Model, other) => {
            bail!("default_model must be a string, got {}", other.type_str())
        }
        (ConfigKey::ReadRate, other) => bail!(
            "min_read_rate_mb_s must be an integer, got {}",
            other.type_str()
        ),
        (ConfigKey::Xmp, other) => {
            bail!("xmp_precedence must be a string, got {}", other.type_str())
        }
        (ConfigKey::ExportXmpOnWatch, other) => bail!(
            "export_xmp_on_watch must be a boolean, got {}",
            other.type_str()
        ),
    }
}

/// Sequence counter making the scratch name unique within a process; the
/// pid makes it unique across processes.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write the table by renaming a synced scratch file into place, the same
/// shape `location::materialize_cities_csv` uses. The scratch file lives in
/// the state directory so the rename never crosses a filesystem, which is
/// what makes it atomic; validation has already completed by the time this
/// runs, and an I/O failure at any step leaves the existing bytes untouched
/// because the rename is the last thing to happen. A timeout is the one
/// exception: the bounded operation abandons its worker thread, which runs
/// on in the background and may still complete the rename after the error
/// has returned, so the untouched-bytes guarantee holds for I/O failures,
/// not timeouts. The scratch file is
/// deliberately not removed on failure: the failing volume is why the write
/// failed, and touching it again from the error path is the unbounded
/// re-stat mistake `TimedOutAfter::describe` exists to prevent.
fn write_config(state: &Path, path: &Path, table: &toml::Table) -> Result<()> {
    use std::io::Write;

    let text = toml::to_string_pretty(table).context("serialize the library config")?;
    let scratch = state.join(format!(
        "config.toml.{}.{}.tmp",
        std::process::id(),
        SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let state = state.to_path_buf();
    let target = path.to_path_buf();
    let owned_scratch = scratch.clone();
    bounded_op(path, "write", crate::io_timeout::STAT_TIMEOUT, move || {
        std::fs::create_dir_all(&state)?;
        let mut file = std::fs::File::create(&owned_scratch)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&owned_scratch, target)
    })
}

/// Edit one supported setting in this library's `config.toml`; `None`
/// removes the key.
///
/// Allowed to create the state directory and an initial config: configuring
/// a library before its first scan is legitimate. It creates no database,
/// writes no defaults over an existing file, and preserves unknown keys,
/// including nested tables, through the rewrite. Unsetting against an
/// absent config is a no-op that creates nothing; an existing file must
/// pass the same validation `load` applies before even a no-op unset
/// returns, so success is never a quiet blessing of a config `load` would
/// reject.
///
/// Concurrency: the library locks module (library_locks.rs, not yet present)
/// adds the shared initialization lock that serializes concurrent first-touch
/// writers; until it exists, no CLI subcommand exposes this entry point. That
/// lock will be released when `edit` returns, while a timed-out write's
/// abandoned worker may still be running, so releasing it waits for no
/// stranded rename.
pub fn edit(ctx: &LibraryContext, key: ConfigKey, value: Option<toml::Value>) -> Result<()> {
    let path = &ctx.paths.config;
    let mut table = match read_config(path)? {
        Some(text) => {
            let table = text
                .parse::<toml::Table>()
                .with_context(|| format!("malformed config {}", path.display()))?;
            // The file as it stands must load before anything is done with
            // it, including nothing: an edit can never launder a broken
            // file into place and leave the failure to surface mid-scan,
            // and a no-op against one must surface the breakage rather
            // than bless it by succeeding.
            config_from_table(&table, path)?;
            table
        }
        None => {
            // Unsetting against an absent config is a no-op: it must
            // succeed and create nothing, not even the state directory.
            if value.is_none() {
                return Ok(());
            }
            initial_table()
        }
    };
    match value {
        Some(v) => {
            validate_value(key, &v)?;
            table.insert(key.name().to_string(), v);
        }
        None => {
            if table.remove(key.name()).is_none() {
                // The key is not there; rewriting the file would move bytes
                // for no setting change at all. The file as a whole has
                // already been validated above, so returning without
                // rewriting cannot leave a broken file unexamined.
                return Ok(());
            }
        }
    }
    write_config(&ctx.paths.state, path, &table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryContext;
    use std::os::unix::fs::PermissionsExt;

    /// Writes to the process's real stderr, bypassing libtest's output
    /// capture. A skip is a passing test, and libtest captures the print
    /// macros for passing tests, so an `eprintln!` skip message is invisible
    /// in a normal `cargo test` run and only appears under `--nocapture`;
    /// writing to fd 2 directly sidesteps the capture. Same pattern as
    /// library.rs's test module, local here for the same reason:
    /// videre-core unit tests have no shared helper. `ManuallyDrop` because
    /// dropping a `File` built from a borrowed fd would close fd 2 for the
    /// rest of the process.
    fn write_past_test_capture(msg: &str) {
        use std::io::Write;
        use std::os::fd::FromRawFd;

        let mut stderr = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(2) });
        let _ = stderr.write_all(msg.as_bytes());
        let _ = stderr.flush();
    }

    /// One library whose config file holds `body`, if any. All path
    /// expectations are built from the context itself: construction
    /// canonicalizes the root, and on macOS a tempdir resolves under
    /// /private, so the spelling the test created is not where the state
    /// directory lives. The TempDir is returned so it outlives the
    /// assertions that read the files inside it.
    fn library_with_config(body: &str) -> (tempfile::TempDir, LibraryContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        std::fs::create_dir(&ctx.paths.state).unwrap();
        if !body.is_empty() {
            std::fs::write(&ctx.paths.config, body).unwrap();
        }
        (temp, ctx)
    }

    #[test]
    fn local_config_rejects_redirects_and_preserves_unknown_fields() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = crate::library::LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        std::fs::create_dir(&ctx.paths.state).unwrap();
        std::fs::write(&ctx.paths.config, "db = \"elsewhere.db\"\n").unwrap();
        assert!(load(&ctx.paths).is_err());
        std::fs::write(&ctx.paths.config, "custom = \"keep\"\n").unwrap();
        edit(&ctx, ConfigKey::ReadRate, Some(toml::Value::Integer(42))).unwrap();
        let text = std::fs::read_to_string(&ctx.paths.config).unwrap();
        let table: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(table["custom"].as_str(), Some("keep"));
        assert_eq!(load(&ctx.paths).unwrap().min_read_rate_mb_s, Some(42));
        assert!(!ctx.paths.db.exists());
    }

    #[test]
    fn an_absent_config_means_defaults_and_creates_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        // Looking at a library must not bring its state directory into
        // being, and reading settings must not conjure a config file.
        assert!(!ctx.paths.state.exists());
        assert_eq!(ctx.settings, LibraryConfig::default());
        assert_eq!(load(&ctx.paths).unwrap(), LibraryConfig::default());
        assert!(!ctx.paths.config.exists());
    }

    #[test]
    fn fixed_declarations_accept_only_the_exact_filenames() {
        for (key, fixed) in [("db", "hashes.db"), ("jsonl", "hashes.jsonl")] {
            // The exact fixed filename passes.
            let (_t, ctx) = library_with_config(&format!("{key} = \"{fixed}\"\n"));
            assert!(load(&ctx.paths).is_ok(), "{key} at its fixed value");
            // Absence resolves to the same filename, so the declarations
            // are optional, not load-bearing.
            let (_t, ctx) = library_with_config("custom = \"x\"\n");
            assert!(load(&ctx.paths).is_ok(), "{key} absent");
            // Any other value, or any other type, is an error before any
            // library work: these are declarations, not settings.
            for body in [
                format!("{key} = \"elsewhere-{key}.db\"\n"),
                format!("{key} = 3\n"),
                format!("{key} = true\n"),
            ] {
                let (_t, ctx) = library_with_config(&body);
                let err = load(&ctx.paths).unwrap_err();
                let msg = format!("{err:#}");
                assert!(msg.contains(key), "{body}: {msg}");
                assert!(msg.contains("cannot be redirected"), "{body}: {msg}");
            }
        }
    }

    #[test]
    fn removed_global_keys_are_rejected_with_an_actionable_error() {
        for key in ["default_db", "default_path"] {
            let (_t, ctx) = library_with_config(&format!("{key} = \"/elsewhere/hashes.db\"\n"));
            let err = load(&ctx.paths).unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains(key), "{key}: {msg}");
            assert!(msg.contains("remove"), "{key}: {msg}");
        }
    }

    #[test]
    fn an_invalid_model_id_is_rejected_at_load() {
        let (_t, ctx) = library_with_config("default_model = \"owner-only-no-slash\"\n");
        let err = load(&ctx.paths).unwrap_err();
        assert!(format!("{err:#}").contains("invalid model id"), "{err:#}");
        // The wrong type is the same class of error as any typed setting.
        let (_t, ctx) = library_with_config("default_model = 42\n");
        let err = load(&ctx.paths).unwrap_err();
        assert!(format!("{err:#}").contains("must be a string"), "{err:#}");
    }

    #[test]
    fn an_unknown_xmp_precedence_is_rejected_and_known_values_load() {
        let (_t, ctx) = library_with_config("xmp_precedence = \"sideways\"\n");
        let err = load(&ctx.paths).unwrap_err();
        assert!(format!("{err:#}").contains("sideways"), "{err:#}");
        for value in ["db", "file", "newest"] {
            let (_t, ctx) = library_with_config(&format!("xmp_precedence = \"{value}\"\n"));
            assert!(load(&ctx.paths).is_ok(), "{value}");
        }
        let (_t, ctx) = library_with_config("xmp_precedence = 3\n");
        let err = load(&ctx.paths).unwrap_err();
        assert!(format!("{err:#}").contains("must be a string"), "{err:#}");
    }

    #[test]
    fn a_non_boolean_export_flag_is_rejected() {
        let (_t, ctx) = library_with_config("export_xmp_on_watch = \"yes\"\n");
        let err = load(&ctx.paths).unwrap_err();
        assert!(format!("{err:#}").contains("must be a boolean"), "{err:#}");
        let (_t, ctx) = library_with_config("export_xmp_on_watch = true\n");
        assert!(load(&ctx.paths).unwrap().export_xmp_on_watch);
    }

    #[test]
    fn read_rate_rejects_zero_negative_noninteger_and_overflow() {
        for body in [
            "min_read_rate_mb_s = 0\n",
            "min_read_rate_mb_s = -5\n",
            "min_read_rate_mb_s = \"fast\"\n",
            // One past i64::MAX is not a TOML integer at all, so it fails
            // the parse; the point is that it is rejected, not defaulted.
            "min_read_rate_mb_s = 9223372036854775808\n",
        ] {
            let (_t, ctx) = library_with_config(body);
            assert!(load(&ctx.paths).is_err(), "{body}");
        }
    }

    #[test]
    fn a_corrupt_file_is_an_error_never_defaults() {
        let (_t, ctx) = library_with_config("not = = toml\n");
        let err = load(&ctx.paths).unwrap_err();
        assert!(format!("{err:#}").contains("malformed config"), "{err:#}");
    }

    #[test]
    fn unset_against_an_absent_config_is_a_noop_creating_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        edit(&ctx, ConfigKey::Model, None).unwrap();
        // Not even the state directory may come into being for a no-op.
        assert!(!ctx.paths.state.exists());
        assert!(!ctx.paths.config.exists());
        assert!(!ctx.paths.db.exists());
    }

    #[test]
    fn a_noop_unset_still_validates_the_existing_file() {
        // The unset targets a key the file does not carry, so no setting
        // would change and no bytes need to move; the file is still one
        // load() and LibraryContext::new reject, and a no-op succeeding
        // against it would bless it.
        let (_t, ctx) = library_with_config("db = \"elsewhere.db\"\n");
        let before = std::fs::read_to_string(&ctx.paths.config).unwrap();
        let err = edit(&ctx, ConfigKey::ReadRate, None).unwrap_err();
        assert!(
            format!("{err:#}").contains("cannot be redirected"),
            "{err:#}"
        );
        assert_eq!(std::fs::read_to_string(&ctx.paths.config).unwrap(), before);
    }

    #[test]
    fn an_edit_preserves_unknown_nested_tables() {
        let (_t, ctx) = library_with_config("[future]\nsub = \"keep\"\n");
        edit(
            &ctx,
            ConfigKey::ExportXmpOnWatch,
            Some(toml::Value::Boolean(true)),
        )
        .unwrap();
        let table: toml::Table =
            toml::from_str(&std::fs::read_to_string(&ctx.paths.config).unwrap()).unwrap();
        assert_eq!(table["future"]["sub"].as_str(), Some("keep"));
        assert_eq!(table["export_xmp_on_watch"].as_bool(), Some(true));
    }

    #[test]
    fn an_edit_does_not_launder_an_already_broken_file() {
        let (_t, ctx) =
            library_with_config("export_xmp_on_watch = \"yes\"\nmin_read_rate_mb_s = 10\n");
        let before = std::fs::read_to_string(&ctx.paths.config).unwrap();
        // The key being edited is valid; the file as a whole is not, and a
        // rewrite would bless the broken half. The edit must fail with the
        // file's own bytes unchanged.
        assert!(edit(&ctx, ConfigKey::ReadRate, Some(toml::Value::Integer(20))).is_err());
        assert_eq!(std::fs::read_to_string(&ctx.paths.config).unwrap(), before);
    }

    #[test]
    fn an_invalid_edit_value_changes_no_bytes() {
        let (_t, ctx) = library_with_config("min_read_rate_mb_s = 10\n");
        let before = std::fs::read_to_string(&ctx.paths.config).unwrap();
        assert!(edit(&ctx, ConfigKey::ReadRate, Some(toml::Value::Integer(0))).is_err());
        assert!(edit(&ctx, ConfigKey::ReadRate, Some(toml::Value::Integer(-3))).is_err());
        assert!(edit(
            &ctx,
            ConfigKey::ReadRate,
            Some(toml::Value::String("fast".into()))
        )
        .is_err());
        assert!(edit(
            &ctx,
            ConfigKey::Model,
            Some(toml::Value::String("no-slash".into()))
        )
        .is_err());
        assert!(edit(
            &ctx,
            ConfigKey::Xmp,
            Some(toml::Value::String("sideways".into()))
        )
        .is_err());
        assert!(edit(
            &ctx,
            ConfigKey::ExportXmpOnWatch,
            Some(toml::Value::Integer(1))
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&ctx.paths.config).unwrap(), before);
    }

    #[test]
    fn a_failed_write_leaves_the_prior_bytes_unchanged() {
        let (_t, ctx) = library_with_config("custom = \"keep\"\n");
        // Root bypasses permission bits entirely (a stock Docker image runs
        // as root), so the behaviour is probed rather than the uid checked,
        // the same probe the integration suite's permissions_are_enforced
        // uses.
        let probe = ctx.paths.root.join("probe");
        std::fs::write(&probe, b"x").unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&probe).is_ok() {
            write_past_test_capture(
                "SKIP: running as root, so chmod 000 does not block creating a file\n",
            );
            return;
        }
        std::fs::set_permissions(&ctx.paths.state, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = edit(&ctx, ConfigKey::ReadRate, Some(toml::Value::Integer(7))).unwrap_err();
        // Restore first, so the tempdir can clean itself up even if an
        // assertion below fails.
        let _ = std::fs::set_permissions(&ctx.paths.state, std::fs::Permissions::from_mode(0o755));
        let msg = format!("{err:#}");
        assert!(msg.contains("write"), "{msg}");
        assert!(msg.contains("config.toml"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&ctx.paths.config).unwrap(),
            "custom = \"keep\"\n"
        );
        // The scratch file was never created, and nothing else appeared.
        let entries: Vec<_> = std::fs::read_dir(&ctx.paths.state).unwrap().collect();
        assert_eq!(entries.len(), 1, "only the config may remain");
    }

    #[test]
    fn a_first_edit_writes_the_five_declarations_and_no_database() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        edit(&ctx, ConfigKey::ReadRate, Some(toml::Value::Integer(42))).unwrap();
        let table: toml::Table =
            toml::from_str(&std::fs::read_to_string(&ctx.paths.config).unwrap()).unwrap();
        // The five exact declarations, so a fresh library's config documents
        // the fixed storage names instead of leaving them implicit.
        assert_eq!(table["db"].as_str(), Some("hashes.db"));
        assert_eq!(table["jsonl"].as_str(), Some("hashes.jsonl"));
        assert_eq!(
            table["default_model"].as_str(),
            Some(crate::embeddings::DEFAULT_MODEL_ID)
        );
        assert_eq!(table["xmp_precedence"].as_str(), Some("db"));
        assert_eq!(table["export_xmp_on_watch"].as_bool(), Some(false));
        assert_eq!(table["min_read_rate_mb_s"].as_integer(), Some(42));
        assert_eq!(load(&ctx.paths).unwrap().min_read_rate_mb_s, Some(42));
        // Configuring a library before its first scan is legitimate; that
        // must not conjure a database.
        assert!(!ctx.paths.db.exists());
    }

    #[test]
    fn a_context_does_not_mutate_when_its_config_is_later_edited() {
        let (_t, ctx) = library_with_config("min_read_rate_mb_s = 10\n");
        let before = ctx.settings.clone();
        edit(&ctx, ConfigKey::ReadRate, Some(toml::Value::Integer(99))).unwrap();
        assert_eq!(ctx.settings, before, "a context is a snapshot, not a view");
        // A context built after the edit sees the new value.
        let fresh = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap();
        assert_eq!(fresh.settings.min_read_rate_mb_s, Some(99));
    }

    #[test]
    fn a_context_refuses_to_load_an_invalid_config() {
        let (_t, ctx) = library_with_config("db = \"elsewhere.db\"\n");
        let err = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cannot be redirected"), "{msg}");
        // Corrupt bytes fail construction the same way: a malformed local
        // config must not silently fall back to defaults.
        let (_t, ctx) = library_with_config("not = = toml\n");
        let err = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap_err();
        assert!(format!("{err:#}").contains("malformed config"), "{err:#}");
    }

    #[test]
    fn unset_removes_only_its_key() {
        let (_t, ctx) =
            library_with_config("default_model = \"owner/custom\"\nmin_read_rate_mb_s = 10\n");
        edit(&ctx, ConfigKey::ReadRate, None).unwrap();
        let cfg = load(&ctx.paths).unwrap();
        assert_eq!(cfg.min_read_rate_mb_s, None);
        assert_eq!(cfg.default_model, "owner/custom");
    }

    #[test]
    fn a_complete_valid_file_loads_every_setting() {
        let (_t, ctx) = library_with_config(
            "db = \"hashes.db\"\n\
             jsonl = \"hashes.jsonl\"\n\
             default_model = \"owner/custom\"\n\
             xmp_precedence = \"file\"\n\
             export_xmp_on_watch = true\n\
             min_read_rate_mb_s = 12\n",
        );
        let cfg = load(&ctx.paths).unwrap();
        assert_eq!(cfg.default_model, "owner/custom");
        assert_eq!(cfg.xmp_precedence, crate::marks::XmpPrecedence::File);
        assert!(cfg.export_xmp_on_watch);
        assert_eq!(cfg.min_read_rate_mb_s, Some(12));
    }
}
