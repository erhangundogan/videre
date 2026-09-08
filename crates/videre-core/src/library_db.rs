//! The library database: the schema it is written with, the one entry point
//! allowed to create it, and the reader path that never creates anything.
//!
//! Only [`initialize`] creates the main database. It builds the schema in a
//! unique temporary file inside the library's own state directory, closes it
//! fully checkpointed, and publishes it by rename while holding the
//! library's activity (exclusive) and init locks, so no other videre process
//! ever observes a half-built library and no run can overwrite an existing
//! one. [`open_existing`] refuses an uninitialized library outright: it opens
//! without `CREATE`, so a reader can never bring a database (or even a lock
//! file) into being, the same readers-create-nothing rule the paths and
//! config layers follow.
//!
//! An existing database is checked before it is trusted: it must carry the
//! SQLite header, and if it has tables of its own but no `file_hashes` it is
//! somebody else's database and is refused unchanged. Older supported
//! schemas are upgraded in place, under exclusive activity plus init, by
//! inspecting `PRAGMA table_info` and adding exactly the columns that are
//! missing, never by firing every `ALTER` and swallowing the errors. A
//! successful preparation is recorded as `PRAGMA user_version = 1`; a
//! version from a future videre is refused with an actionable error, and the
//! required tables and columns are verified after preparation so a helper
//! that swallowed an error cannot produce a false ready marker.
//!
//! Every indexed path in an opened database is validated against the
//! canonical root's components, once per context, before anything is
//! written and before any row is served: a database holding a path outside
//! its root belongs to a different library and is refused whole, with its
//! bytes untouched, because serving half of one library plus half of
//! another is silently wrong rather than loudly incomplete. Success is
//! memoized per context via the context's `index_validated` memo; failure
//! never is.

use crate::io_timeout::STAT_TIMEOUT;
use crate::library::{bounded_op, root_cause_is_not_found, LibraryContext};
use crate::library_locks::ActivityMode;
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::io::Read;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The schema version this build writes and understands. `1` is the first
/// versioned schema: every pre-marker database reads as `0` and is treated
/// as an older supported library, validated then prepared.
const SCHEMA_VERSION: i64 = 1;

/// The first sixteen bytes of every SQLite database file. A library
/// candidate without them is not a SQLite file, whatever its name.
const SQLITE_MAGIC: [u8; 16] = *b"SQLite format 3\0";

/// How long every open waits for a concurrent writer before failing, so two
/// videre processes on one library surface as a bounded error rather than
/// the immediate `database is locked` default.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The complete current `file_hashes` schema: the columns the scan writes,
/// the newer optional columns older libraries predate, and the location
/// columns the location passes add. One list is the source for both the
/// `CREATE TABLE` and the add-missing upgrade, so the two cannot drift.
const FILE_HASHES_COLUMNS: &[(&str, &str)] = &[
    ("path", "TEXT PRIMARY KEY"),
    ("hash", "TEXT NOT NULL"),
    ("size_bytes", "INTEGER"),
    ("created_at", "TEXT"),
    ("modified_at", "TEXT"),
    ("ext", "TEXT"),
    ("mime", "TEXT"),
    ("phash", "INTEGER"),
    ("exif_date", "TEXT"),
    ("gps_lat", "REAL"),
    ("gps_lon", "REAL"),
    ("width", "INTEGER"),
    ("height", "INTEGER"),
    ("duration_secs", "REAL"),
    ("codec", "TEXT"),
    ("location_name", "TEXT"),
    ("location_cluster_id", "INTEGER"),
];

/// The complete current `faces` schema, for the same add-missing treatment:
/// `face_db` creates the table but swallows its own column migrations
/// (deliberately, for the read paths they were written for), so the missing
/// columns are added explicitly here where an error must be an error.
const FACES_COLUMNS: &[(&str, &str)] = &[
    ("id", "INTEGER PRIMARY KEY"),
    ("hash", "TEXT NOT NULL"),
    ("bbox", "TEXT NOT NULL"),
    ("landmark", "TEXT"),
    ("embedding", "BLOB NOT NULL"),
    ("cluster_id", "INTEGER"),
    ("person_label", "TEXT"),
    ("confirmed", "INTEGER DEFAULT 0"),
    ("is_primary", "INTEGER DEFAULT 0"),
    ("det_score", "REAL"),
    ("blur", "REAL"),
];

/// The optional tables a prepared library carries, created by the same
/// helpers the commands use rather than a parallel copy of their DDL. All
/// are optional in the sense that a command that never runs leaves them
/// empty, never absent.
const REQUIRED_TABLES: &[&str] = &[
    "file_hashes",
    "people",
    "faces",
    "faces_scanned",
    "marks",
    "photo_tags",
    "classifications",
    "location_clusters",
    "pipeline_runs",
];

/// Sequence counter making the build temporary's name unique within a
/// process; the pid makes it unique across processes.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Build the `file_hashes` DDL from the one column list, so the created
/// table and the upgrade expectations are the same source of truth.
fn file_hashes_ddl() -> String {
    let columns: Vec<String> = FILE_HASHES_COLUMNS
        .iter()
        .map(|(name, decl)| format!("    {name:<20} {decl}"))
        .collect();
    format!(
        "CREATE TABLE IF NOT EXISTS file_hashes (\n{}\n);",
        columns.join(",\n")
    )
}

/// Whether `table` has a column named `column`.
fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Add the expected columns that are absent, one explicit `ALTER` each,
/// decided by inspecting `PRAGMA table_info` rather than by attempting every
/// `ALTER` and swallowing the errors. The one tolerable failure is a column
/// that appeared between the inspection and the `ALTER` (another connection
/// upgraded first): present afterwards is success, still missing is a real
/// error, so no error is dismissed wholesale.
fn add_missing_columns(
    conn: &Connection,
    table: &str,
    expected: &[(&str, &str)],
) -> rusqlite::Result<()> {
    for (name, decl) in expected {
        if column_exists(conn, table, name)? {
            continue;
        }
        let alter = format!("ALTER TABLE {table} ADD COLUMN {name} {decl}");
        if let Err(e) = conn.execute_batch(&alter) {
            if !column_exists(conn, table, name)? {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Create or upgrade the scan table, the one schema implementation the
/// scanner calls: `crates/videre/src/sqlite_output.rs` delegates here rather
/// than owning a duplicate of the DDL. Idempotent, safe on every open, and
/// additive: an existing older table keeps its rows and gains only the
/// columns it lacks.
pub fn ensure_scan_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&file_hashes_ddl())?;
    add_missing_columns(conn, "file_hashes", FILE_HASHES_COLUMNS)
}

/// Verify the tables and columns a prepared library must have. Runs after
/// the DDL helpers, several of which swallow their own errors by design
/// (they were written for read paths), so a swallowed error surfaces here
/// instead of producing a ready marker over a missing table or column.
fn verify_schema(conn: &Connection) -> Result<()> {
    for table in REQUIRED_TABLES {
        if !crate::db::table_exists(conn, table)? {
            bail!("required table {table} is missing after schema preparation");
        }
    }
    let mut required: Vec<(&str, &str)> = Vec::new();
    for (name, _) in FILE_HASHES_COLUMNS {
        required.push(("file_hashes", name));
    }
    for (name, _) in FACES_COLUMNS {
        required.push(("faces", name));
    }
    // classifications' legacy pre-model shape is dropped and rebuilt by its
    // own helper, but the rebuild is still verified rather than assumed.
    required.push(("classifications", "model_id"));
    required.push(("classifications", "hash"));
    for (table, column) in &required {
        if !column_exists(conn, table, column)? {
            bail!("required column {table}.{column} is missing after schema preparation");
        }
    }
    Ok(())
}

/// Whether the schema is already complete, used to decide whether an open
/// needs the upgrade path at all.
fn schema_complete(conn: &Connection) -> Result<bool> {
    Ok(verify_schema(conn).is_ok())
}

/// Prepare the complete supported schema on `conn`: the scan table plus the
/// optional faces/people/marks/tags/classification/location/run tables,
/// created by the existing helpers the commands already use, then verified.
fn prepare_schema(conn: &Connection) -> Result<()> {
    ensure_scan_schema(conn)?;
    // people and faces_scanned come with the faces table.
    crate::face_db::create_faces_table(conn)?;
    crate::marks::ensure_marks_table(conn)?;
    crate::tags::ensure_photo_tags_table(conn)?;
    crate::classify::ensure_classifications_table(conn)?;
    crate::location_cluster::ensure_location_clusters_table(conn)?;
    crate::pipeline_runs::ensure_pipeline_runs_table(conn)?;
    add_missing_columns(conn, "faces", FACES_COLUMNS)?;
    verify_schema(conn)?;
    Ok(())
}

/// The schema version recorded in the database file.
fn user_version(conn: &Connection) -> Result<i64> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .context("read the library schema version")?;
    Ok(version)
}

/// Whether the file begins with the SQLite header. The caller has already
/// handled existence and size; this is the supported-library check that
/// comes before any upgrade touches a file.
fn is_sqlite_file(path: &Path) -> Result<bool> {
    let owned = path.to_path_buf();
    match bounded_op(path, "read", STAT_TIMEOUT, move || {
        let mut file = std::fs::File::open(&owned)?;
        let mut header = [0u8; 16];
        // A file shorter than the header is not a database either; the
        // mismatch is folded into the answer rather than surfaced as an
        // error, because the caller's question is exactly "is this one?".
        match file.read_exact(&mut header) {
            Ok(()) => Ok(Some(header)),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }) {
        Ok(Some(header)) => Ok(header == SQLITE_MAGIC),
        Ok(None) => Ok(false),
        Err(e) => Err(e),
    }
}

/// What an inspection of the database file found.
enum DbFile {
    /// No file: nothing has been initialized.
    Absent,
    /// A zero-byte file: the debris of an interrupted creation, never a
    /// library, and never exposed as successfully initialized.
    ZeroByte,
    /// A non-empty file carrying the SQLite header.
    Present,
}

/// Inspect the database file, refusing redirected state on the way in: a
/// symlinked or multiply-linked database would silently couple two
/// libraries, and neither may be used.
fn inspect_db_file(ctx: &LibraryContext) -> Result<DbFile> {
    crate::library_locks::reject_redirect(&ctx.paths.db, "the library database")?;
    let owned = ctx.paths.db.clone();
    let len = match bounded_op(&ctx.paths.db, "read", STAT_TIMEOUT, move || {
        std::fs::symlink_metadata(&owned).map(|m| m.len())
    }) {
        Ok(len) => len,
        Err(e) if root_cause_is_not_found(&e) => return Ok(DbFile::Absent),
        Err(e) => return Err(e),
    };
    if len == 0 {
        return Ok(DbFile::ZeroByte);
    }
    if !is_sqlite_file(&ctx.paths.db)? {
        bail!(
            "{} is not a SQLite database; not a videre library",
            ctx.paths.db.display()
        );
    }
    Ok(DbFile::Present)
}

/// Open a connection without `CREATE`: opening must never bring a database
/// into being, so readers and signal handlers reach for this rather than
/// `Connection::open`. `NOFOLLOW` backs up the lstat redirect checks with
/// the kernel's own refusal to open a symlink's target; `NO_MUTEX` matches
/// the default `rusqlite::Connection::open` uses, leaving threading to the
/// caller.
pub(crate) fn open_without_create(path: &Path) -> rusqlite::Result<Connection> {
    use rusqlite::OpenFlags;
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
}

/// Open an existing database with the busy timeout every library open uses.
fn open_existing_conn(ctx: &LibraryContext) -> Result<Connection> {
    let conn = open_without_create(&ctx.paths.db)
        .with_context(|| format!("open {}", ctx.paths.db.display()))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .context("set the library database busy timeout")?;
    Ok(conn)
}

/// The WAL behaviour every library open applies, idempotent and persistent
/// in the file (see `db::open_wal` for why it matters).
fn set_wal(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("set the library database to WAL journal mode")
}

/// Refuse a SQLite file that is not a videre library: tables of its own but
/// no `file_hashes` means the database belongs to some other program, and
/// preparing videre's schema inside it would corrupt both.
fn require_supported_library(conn: &Connection, db: &Path) -> Result<()> {
    if crate::db::table_exists(conn, "file_hashes")? {
        return Ok(());
    }
    let user_tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        user_tables == 0,
        "{} is not a videre library: a SQLite database with tables of its own and no file_hashes",
        db.display()
    );
    Ok(())
}

// Counts rows the containment validation has judged, in tests, so a scale
// test can prove the validation really ran over every row while the guard
// layer's resolution counter stays flat: zero per-row filesystem probes.
// Thread-local for the same reason as library_guard's counter: libtest runs
// each test on its own thread, so a process-global counter would attribute
// neighbouring tests' validations to whoever read it.
#[cfg(test)]
thread_local! {
    static ROWS_VALIDATED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test-only read of the containment row counter.
#[cfg(test)]
pub(crate) fn count_rows_validated() -> u64 {
    ROWS_VALIDATED.with(std::cell::Cell::get)
}

/// Test-only bump of the containment row counter.
#[cfg(test)]
fn note_row_validated() {
    ROWS_VALIDATED.with(|c| c.set(c.get() + 1));
}

/// Validate every indexed path against the canonical root, once per context,
/// before any write and before any row is served.
///
/// Containment is by path components, not string prefix: `<root>-sibling`
/// starts with the same bytes as `<root>` but is a different directory, and
/// a prefix comparison would let it through. A `.` or `..` component is
/// refused the same way, because `<root>/../outside` does start with
/// `<root>` lexically and videre never writes such a path, so a row carrying
/// one is hand-injected and foreign. A file that no longer exists
/// under the root remains valid, because the question is which library the
/// row belongs to, not whether its media is currently mounted. A foreign row
/// refuses the whole library: serving it would mix two libraries silently.
/// Success is memoized via the context's `index_validated` memo, never
/// failure, so a refused library is rechecked on every attempt.
fn validate_row_containment(ctx: &LibraryContext, conn: &Connection) -> Result<()> {
    if ctx.index_validated() {
        return Ok(());
    }
    if crate::db::table_exists(conn, "file_hashes")? {
        let root = ctx.paths.root.clone();
        let mut stmt = conn
            .prepare("SELECT path FROM file_hashes")
            .context("read the indexed paths")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            #[cfg(test)]
            note_row_validated();
            let stored = Path::new(&path);
            let dot_component = stored
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir));
            if dot_component || !stored.starts_with(&root) {
                bail!(
                    "the database at {} indexes {}, which is outside the library root {}; it belongs to a different library, so nothing was read or changed",
                    ctx.paths.db.display(),
                    path,
                    root.display()
                );
            }
        }
    }
    ctx.mark_index_validated();
    Ok(())
}

/// Finish opening an existing database while the caller holds the
/// appropriate locks: refuse future versions, refuse foreign databases and
/// foreign rows (all read-only checks, so a refusal leaves the file
/// untouched), then prepare and mark the schema when it is still incomplete.
fn open_prepared(ctx: &LibraryContext, conn: &Connection) -> Result<()> {
    let version = user_version(conn)?;
    if version > SCHEMA_VERSION {
        bail!(
            "the library at {} was written by a newer videre (schema version {version}, this build understands up to {SCHEMA_VERSION}); upgrade videre to open it",
            ctx.paths.root.display()
        );
    }
    require_supported_library(conn, &ctx.paths.db)?;
    validate_row_containment(ctx, conn)?;
    set_wal(conn)?;
    if version < SCHEMA_VERSION || !schema_complete(conn)? {
        prepare_schema(conn)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .context("record the library schema version")?;
    }
    verify_schema(conn)?;
    Ok(())
}

/// Remove one path, bounded, naming what it was in the error.
fn remove_file_bounded(path: &Path, what: &str) -> Result<()> {
    let owned = path.to_path_buf();
    bounded_op(path, "remove", STAT_TIMEOUT, move || {
        std::fs::remove_file(&owned)
    })
    .with_context(|| format!("remove {what} {}", path.display()))
}

/// Sync a directory so a rename inside it is durable, the step after the
/// rename that a crash would otherwise undo. Shared with the config layer,
/// whose scratch rename needs the same crash durability.
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    let owned = path.to_path_buf();
    bounded_op(path, "sync", STAT_TIMEOUT, move || {
        let dir = std::fs::File::open(&owned)?;
        dir.sync_all()
    })
    .with_context(|| format!("sync {}", path.display()))
}

/// Remove build temporaries left behind by failed or crashed
/// initializations. Safe under the activity and init locks, which every
/// caller of [`publish_fresh`] holds: no other videre process can be
/// building one at the same moment, so a matching file is by definition
/// dead. A crashed build can leave not only the temporary database but its
/// WAL sidecars (`.tmp-wal`, `.tmp-shm`), so every name carrying a temp
/// build stem is swept, not only the one ending in `.tmp`. Best-effort: a
/// temporary that cannot be removed must not fail the initialization that
/// is about to publish a fresh database.
fn sweep_stale_builds(ctx: &LibraryContext) {
    let state = ctx.paths.state.clone();
    let names = bounded_op(&ctx.paths.state, "read", STAT_TIMEOUT, move || {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&state)? {
            out.push(entry?.file_name());
        }
        Ok(out)
    });
    if let Ok(names) = names {
        for name in names {
            let text = name.to_string_lossy();
            let stale = text.starts_with("hashes.db.")
                && (text.ends_with(".tmp")
                    || text.ends_with(".tmp-wal")
                    || text.ends_with(".tmp-shm"));
            if stale {
                let _ =
                    remove_file_bounded(&ctx.paths.state.join(&name), "a stale build temporary");
            }
        }
    }
}

/// Publish one completed database build into its final path.
///
/// Kept as one operation so the no-overwrite contract can be tested at the
/// same boundary initialization uses.
fn publish_database(from: &Path, to: &Path) -> Result<()> {
    let rename_from = from.to_path_buf();
    let rename_to = to.to_path_buf();
    bounded_op(to, "publish", STAT_TIMEOUT, move || {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &rename_from,
            rustix::fs::CWD,
            &rename_to,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    })
    .with_context(|| format!("publish {}", to.display()))
}

/// Build a fresh database in a unique state-local temporary file, close it
/// fully checkpointed, and publish it by rename without overwriting an
/// existing destination. Only called with the library's activity (exclusive)
/// and init locks held, which is what makes the absent-destination check
/// and the rename one uninterrupted decision no other videre process can
/// interleave with. On failure the temporary is deliberately left in place:
/// removing it would touch the volume that just failed, and the next
/// successful initialization sweeps it.
fn publish_fresh(ctx: &LibraryContext) -> Result<Connection> {
    sweep_stale_builds(ctx);
    let tmp = ctx.paths.state.join(format!(
        "hashes.db.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let build = || -> Result<()> {
        let conn = Connection::open(&tmp).with_context(|| format!("build {}", tmp.display()))?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        prepare_schema(&conn)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .context("record the library schema version")?;
        set_wal(&conn)?;
        // Checkpoint and close fully, so what gets published is one
        // self-contained file, never a half-written WAL pair.
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    };
    build()?;
    let len = {
        let owned = tmp.clone();
        bounded_op(&tmp, "read", STAT_TIMEOUT, move || {
            std::fs::metadata(&owned).map(|m| m.len())
        })
        .with_context(|| format!("read {}", tmp.display()))?
    };
    anyhow::ensure!(
        len > 0,
        "the built database is empty; refusing to publish it"
    );
    anyhow::ensure!(
        is_sqlite_file(&tmp)?,
        "the built database does not carry the SQLite header; refusing to publish it"
    );
    // Publish only onto an absent destination: under the locks, a
    // destination that appeared is not ours to replace.
    crate::library_locks::reject_redirect(&ctx.paths.db, "the library database")?;
    let owned = ctx.paths.db.clone();
    let exists = match bounded_op(&ctx.paths.db, "read", STAT_TIMEOUT, move || {
        std::fs::symlink_metadata(&owned).map(|_| ())
    }) {
        Ok(()) => true,
        Err(e) if root_cause_is_not_found(&e) => false,
        Err(e) => return Err(e),
    };
    anyhow::ensure!(
        !exists,
        "a database appeared at {} while the library was being initialized",
        ctx.paths.db.display()
    );
    publish_database(&tmp, &ctx.paths.db)?;
    sync_dir(&ctx.paths.state)?;
    let conn = open_existing_conn(ctx)?;
    open_prepared(ctx, &conn)?;
    Ok(conn)
}

/// Initialize the library's database: the only entry point that may create
/// it, its state directory, or its lock files.
///
/// Creates the state directories, takes the library's activity lock
/// exclusive and then its init lock (the acquisition order every writer
/// follows), and either publishes a freshly built database by rename or
/// brings an existing one up to the current schema in place. The config is
/// written last, read-before-write: exactly the five declarations of a
/// fresh library, and only when no config exists, so a repeated initialize
/// never rewrites what a user or a previous run wrote.
pub fn initialize(ctx: &LibraryContext) -> Result<Connection> {
    crate::library_locks::ensure_state_and_locks(ctx)?;
    let _activity = crate::library_locks::try_activity(ctx, ActivityMode::Exclusive)
        .with_context(|| format!("initialize {}", ctx.paths.root.display()))?;
    let _init = crate::library_locks::try_init(ctx)?;
    // Identity rechecked under the locks: the root or its .videre may have
    // been replaced while the directories were being created.
    crate::library_locks::verify_state(ctx)?;
    crate::library_locks::reject_redirect(&ctx.paths.config, "the library config")?;
    crate::library_locks::reject_dir_redirect(&ctx.paths.embeddings, "the embeddings directory")?;
    let conn = match inspect_db_file(ctx)? {
        DbFile::Absent => publish_fresh(ctx)?,
        DbFile::ZeroByte => {
            // Provably empty, so replacing it cannot lose anything, and a
            // zero-byte file must never be exposed as a library.
            remove_file_bounded(&ctx.paths.db, "the empty database")?;
            publish_fresh(ctx)?
        }
        DbFile::Present => {
            let conn = open_existing_conn(ctx)?;
            open_prepared(ctx, &conn)?;
            conn
        }
    };
    crate::library_config::write_initial_if_absent(ctx)?;
    Ok(conn)
}

/// Open the library's existing database, creating nothing: no database, no
/// lock file, not even the locks directory.
///
/// The open holds the activity lock shared, long enough to inspect and
/// validate the library; long-lived readers take their own shared activity
/// lock around their work. A database needing an upgrade is the one case
/// that writes: the shared lock is released, exclusive activity and init
/// are taken, and everything is rechecked under the new locks before the
/// upgrade runs, so two processes cannot both upgrade and a reader never
/// upgrades while another reads.
pub fn open_existing(ctx: &LibraryContext) -> Result<Connection> {
    crate::library_locks::verify_state(ctx)?;
    let _activity = crate::library_locks::try_activity(ctx, ActivityMode::Shared)?;
    crate::library_locks::reject_redirect(&ctx.paths.config, "the library config")?;
    crate::library_locks::reject_dir_redirect(&ctx.paths.embeddings, "the embeddings directory")?;
    match inspect_db_file(ctx)? {
        DbFile::Present => {}
        DbFile::Absent => bail!(
            "library {} has not been initialized: no database at {}",
            ctx.paths.root.display(),
            ctx.paths.db.display()
        ),
        DbFile::ZeroByte => bail!(
            "library {} was never fully initialized: {} is empty; initialize it (for example with videre scan)",
            ctx.paths.root.display(),
            ctx.paths.db.display()
        ),
    }
    // Phase one, under shared activity: read-only inspection and validation.
    // Every refusal below leaves the file exactly as it was.
    let conn = open_existing_conn(ctx)?;
    let version = user_version(&conn)?;
    if version > SCHEMA_VERSION {
        bail!(
            "the library at {} was written by a newer videre (schema version {version}, this build understands up to {SCHEMA_VERSION}); upgrade videre to open it",
            ctx.paths.root.display()
        );
    }
    require_supported_library(&conn, &ctx.paths.db)?;
    validate_row_containment(ctx, &conn)?;
    if version == SCHEMA_VERSION && schema_complete(&conn)? {
        set_wal(&conn)?;
        return Ok(conn);
    }
    // Phase two: the upgrade writes, so shared activity gives way to
    // exclusive activity plus init, and everything is rechecked under the
    // new locks, because another process may have finished the upgrade in
    // the gap.
    drop(conn);
    drop(_activity);
    let _exclusive = crate::library_locks::try_activity(ctx, ActivityMode::Exclusive)?;
    let _init = crate::library_locks::try_init(ctx)?;
    crate::library_locks::verify_state(ctx)?;
    match inspect_db_file(ctx)? {
        DbFile::Present => {}
        DbFile::ZeroByte => bail!(
            "the database at {} became empty while the library was being opened",
            ctx.paths.db.display()
        ),
        DbFile::Absent => bail!(
            "the database at {} disappeared while the library was being opened",
            ctx.paths.db.display()
        ),
    }
    let conn = open_existing_conn(ctx)?;
    open_prepared(ctx, &conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::LibraryContext;
    use crate::library_locks;
    use rusqlite::params;

    /// One library root plus a context on it. All path expectations are built
    /// from the context: construction canonicalizes the root, and on macOS a
    /// tempdir resolves under /private, so the spelling the test created is
    /// not where the state directory lives.
    fn library() -> (tempfile::TempDir, LibraryContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        (temp, ctx)
    }

    /// The regression this module exists for: a reader never brings a
    /// library into being, and initializing twice never disturbs the config
    /// the first initialization wrote.
    #[test]
    fn readers_never_initialize_and_repeated_initialization_preserves_config() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = crate::library::LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        assert!(open_existing(&ctx).is_err());
        assert!(!ctx.paths.state.exists());
        drop(initialize(&ctx).unwrap());
        let before = std::fs::read(&ctx.paths.config).unwrap();
        drop(initialize(&ctx).unwrap());
        assert_eq!(std::fs::read(&ctx.paths.config).unwrap(), before);
        let conn = open_existing(&ctx).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn fresh_database_publication_never_replaces_an_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let built = temp.path().join("built.db");
        let destination = temp.path().join("hashes.db");
        std::fs::write(&built, b"completed build").unwrap();
        std::fs::write(&destination, b"newer initializer").unwrap();

        let err = publish_database(&built, &destination).unwrap_err();

        assert!(format!("{err:#}").contains("publish"), "{err:#}");
        assert_eq!(std::fs::read(&destination).unwrap(), b"newer initializer");
        assert!(built.exists(), "the refused build remains recoverable");
    }

    #[test]
    fn corrupt_database_bytes_are_refused_unchanged() {
        let (_t, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        let garbage = b"definitely not a sqlite database at all";
        std::fs::write(&ctx.paths.db, garbage).unwrap();
        assert!(open_existing(&ctx).is_err());
        let err = initialize(&ctx).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a SQLite database"),
            "{err:#}"
        );
        // Refusal leaves the bytes as they were; whatever the file is, it is
        // not videre's to repair.
        assert_eq!(std::fs::read(&ctx.paths.db).unwrap(), garbage);
    }

    #[test]
    fn a_zero_byte_database_is_never_exposed_as_a_library() {
        let (_t, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        std::fs::write(&ctx.paths.db, b"").unwrap();
        let err = open_existing(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("empty"), "{err:#}");
        // initialize may replace it: a zero-byte file is the debris of an
        // interrupted creation and provably holds nothing.
        let conn = initialize(&ctx).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn a_foreign_sqlite_database_is_refused_unchanged() {
        let (_t, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        {
            let conn = rusqlite::Connection::open(&ctx.paths.db).unwrap();
            conn.execute_batch(
                "CREATE TABLE somebody_elses (id INTEGER PRIMARY KEY);
                 INSERT INTO somebody_elses VALUES (1);",
            )
            .unwrap();
        }
        let before = std::fs::read(&ctx.paths.db).unwrap();
        let err = open_existing(&ctx).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a videre library"),
            "{err:#}"
        );
        let err = initialize(&ctx).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a videre library"),
            "{err:#}"
        );
        assert_eq!(std::fs::read(&ctx.paths.db).unwrap(), before);
    }

    #[test]
    fn an_older_supported_schema_is_upgraded_in_place_with_its_rows() {
        let (_t, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        {
            let conn = rusqlite::Connection::open(&ctx.paths.db).unwrap();
            conn.execute_batch(
                "CREATE TABLE file_hashes (
                    path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                    created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
                    exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER,
                    height INTEGER
                );",
            )
            .unwrap();
            for name in ["a.jpg", "b.jpg"] {
                conn.execute(
                    "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'h', 'jpg')",
                    params![ctx.paths.root.join(name).to_str().unwrap()],
                )
                .unwrap();
            }
        }
        let conn = open_existing(&ctx).unwrap();
        // The newer optional columns were added by inspecting the table, not
        // by firing every ALTER and swallowing the errors.
        for column in [
            "mime",
            "duration_secs",
            "codec",
            "location_name",
            "location_cluster_id",
        ] {
            let present: i64 = conn
                .query_row(
                    &format!(
                        "SELECT count(*) FROM pragma_table_info('file_hashes') WHERE name = '{column}'"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "file_hashes.{column}");
        }
        // The optional tables are prepared under the same protection.
        for table in [
            "people",
            "faces",
            "faces_scanned",
            "marks",
            "photo_tags",
            "classifications",
            "location_clusters",
            "pipeline_runs",
        ] {
            assert!(crate::db::table_exists(&conn, table).unwrap(), "{table}");
        }
        let count: i64 = conn
            .query_row("SELECT count(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "the upgrade must keep the existing rows");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        drop(conn);
        // A second open finds the ready marker: no upgrade work, same state.
        let conn = open_existing(&ctx).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn a_config_without_a_database_is_not_a_library_and_initialize_keeps_the_config() {
        let (_t, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.state).unwrap();
        std::fs::write(&ctx.paths.config, "custom = \"keep\"\n").unwrap();
        assert!(open_existing(&ctx).is_err());
        drop(initialize(&ctx).unwrap());
        assert_eq!(
            std::fs::read_to_string(&ctx.paths.config).unwrap(),
            "custom = \"keep\"\n"
        );
    }

    #[test]
    fn a_database_without_a_config_reads_and_initialize_writes_five_declarations_once() {
        let (_t, ctx) = library();
        drop(initialize(&ctx).unwrap());
        std::fs::remove_file(&ctx.paths.config).unwrap();
        // A reader serves the library with its built-in settings and does not
        // conjure the config back.
        drop(open_existing(&ctx).unwrap());
        assert!(!ctx.paths.config.exists());
        drop(initialize(&ctx).unwrap());
        let text = std::fs::read_to_string(&ctx.paths.config).unwrap();
        let table: toml::Table = toml::from_str(&text).unwrap();
        // Exactly the five fixed config declarations.
        assert_eq!(table.len(), 5, "{text}");
        assert_eq!(table["db"].as_str(), Some("hashes.db"));
        assert_eq!(table["jsonl"].as_str(), Some("hashes.jsonl"));
        assert_eq!(
            table["default_model"].as_str(),
            Some(crate::embeddings::DEFAULT_MODEL_ID)
        );
        assert_eq!(table["xmp_precedence"].as_str(), Some("db"));
        assert_eq!(table["export_xmp_on_watch"].as_bool(), Some(false));
        // And it is never rewritten on a repeated initialize.
        let before = std::fs::read(&ctx.paths.config).unwrap();
        drop(initialize(&ctx).unwrap());
        assert_eq!(std::fs::read(&ctx.paths.config).unwrap(), before);
    }

    #[test]
    fn two_simultaneous_initializers_produce_one_valid_library() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let ctx = ctx.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    initialize(&ctx).map(|conn| {
                        conn.query_row::<i64, _, _>("SELECT count(*) FROM file_hashes", [], |r| {
                            r.get(0)
                        })
                        .unwrap()
                    })
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let winners = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(winners, 1, "exactly one initializer must win: {results:?}");
        for result in &results {
            match result {
                Ok(count) => assert_eq!(*count, 0),
                // The loser fails on the lock coordination, the same wording
                // a held activity lock produces, not on a half-built library
                // and not silently. A panic would have failed the join.
                Err(e) => {
                    let msg = format!("{e:#}");
                    assert!(
                        msg.contains("in use"),
                        "the loser must be refused by the lock, not by anything else: {msg}"
                    );
                }
            }
        }
        // The config is never truncated: exactly the five declarations.
        let table: toml::Table =
            toml::from_str(&std::fs::read_to_string(&ctx.paths.config).unwrap()).unwrap();
        assert_eq!(table.len(), 5);
        drop(open_existing(&ctx).unwrap());
    }

    #[test]
    fn a_held_activity_lock_blocks_initialization_cleanly() {
        let (_t, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        let _held =
            library_locks::try_activity(&ctx, library_locks::ActivityMode::Exclusive).unwrap();
        let err = initialize(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("in use"), "{err:#}");
        drop(_held);
        drop(initialize(&ctx).unwrap());
    }

    #[test]
    fn a_held_exclusive_activity_lock_blocks_readers_too() {
        let (_t, ctx) = library();
        drop(initialize(&ctx).unwrap());
        let _held =
            library_locks::try_activity(&ctx, library_locks::ActivityMode::Exclusive).unwrap();
        assert!(open_existing(&ctx).is_err());
    }

    #[test]
    fn redirected_state_database_and_config_are_refused() {
        let (temp, ctx) = library();
        // An externally linked .videre must not be used: nothing is created
        // through it.
        let elsewhere = temp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &ctx.paths.state).unwrap();
        let err = initialize(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
        assert!(!elsewhere.join("locks").exists());
        std::fs::remove_file(&ctx.paths.state).unwrap();

        // A redirected database must not be used either.
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        let outside_db = temp.path().join("outside.db");
        std::fs::write(&outside_db, b"").unwrap();
        std::os::unix::fs::symlink(&outside_db, &ctx.paths.db).unwrap();
        let err = initialize(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
        assert_eq!(std::fs::read(&outside_db).unwrap(), b"");
        let err = open_existing(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");

        // Nor a redirected config. It is valid TOML, but context construction
        // must refuse it before any settings are loaded through the link.
        std::fs::remove_file(&ctx.paths.db).unwrap();
        let outside_cfg = temp.path().join("outside.toml");
        std::fs::write(&outside_cfg, b"custom = \"x\"\n").unwrap();
        std::os::unix::fs::symlink(&outside_cfg, &ctx.paths.config).unwrap();
        let err = LibraryContext::new(&ctx.paths.root, &temp.path().join("cache")).unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
        assert_eq!(std::fs::read(&outside_cfg).unwrap(), b"custom = \"x\"\n");
    }

    #[test]
    fn multiply_linked_state_files_are_refused() {
        let (temp, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        // A database hard-linked into two names is two libraries silently
        // coupled into one; neither may use it.
        let twin_db = temp.path().join("twin.db");
        {
            let conn = rusqlite::Connection::open(&twin_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL);",
            )
            .unwrap();
        }
        std::fs::hard_link(&twin_db, &ctx.paths.db).unwrap();
        let err = initialize(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("hard-linked"), "{err:#}");
        let err = open_existing(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("hard-linked"), "{err:#}");
        assert!(twin_db.exists());

        // The same holds for the config.
        std::fs::remove_file(&ctx.paths.db).unwrap();
        let twin_cfg = temp.path().join("twin.toml");
        std::fs::write(&twin_cfg, "custom = \"x\"\n").unwrap();
        std::fs::hard_link(&twin_cfg, &ctx.paths.config).unwrap();
        let err = LibraryContext::new(&ctx.paths.root, &temp.path().join("cache")).unwrap_err();
        assert!(format!("{err:#}").contains("hard-linked"), "{err:#}");
    }

    #[test]
    fn a_symlinked_embeddings_directory_is_refused() {
        let (temp, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        // The embeddings directory is a symlink to a directory outside the
        // state directory: opening through it would write this library's
        // embeddings into whatever the link points at.
        let outside = temp.path().join("outside-embeddings");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &ctx.paths.embeddings).unwrap();
        let err = initialize(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
        // The refusal came before any build, and the outside directory was
        // not populated through the link.
        assert!(!ctx.paths.db.exists());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        // A reader refuses the same redirect; the state the failed
        // initialize left behind (directories, lock files) is all it sees.
        let err = open_existing(&ctx).unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
        // An ordinary in-place embeddings directory passes: it is state the
        // embeddings layer itself creates, and a previously initialized
        // library commonly already has one.
        std::fs::remove_file(&ctx.paths.embeddings).unwrap();
        std::fs::create_dir(&ctx.paths.embeddings).unwrap();
        drop(initialize(&ctx).unwrap());
        drop(open_existing(&ctx).unwrap());
    }

    #[test]
    fn a_foreign_row_refuses_the_whole_library_without_touching_it() {
        let (_t, ctx) = library();
        {
            let conn = initialize(&ctx).unwrap();
            // A string prefix of the root is not containment by components:
            // <root>-sibling starts with the same bytes but is another
            // directory.
            let adjacent = format!("{}-sibling/x.jpg", ctx.paths.root.to_str().unwrap());
            conn.execute(
                "INSERT INTO file_hashes (path, hash) VALUES (?1, 'h')",
                params![adjacent],
            )
            .unwrap();
        }
        let before = std::fs::read(&ctx.paths.db).unwrap();
        let before_cfg = std::fs::read(&ctx.paths.config).unwrap();
        // A fresh context has no memoized validation, so the row is checked.
        let fresh = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap();
        let err = open_existing(&fresh).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("outside the library root"), "{msg}");
        assert!(msg.contains("-sibling"), "{msg}");
        assert!(initialize(&fresh).is_err());
        assert_eq!(std::fs::read(&ctx.paths.db).unwrap(), before);
        assert_eq!(std::fs::read(&ctx.paths.config).unwrap(), before_cfg);
    }

    #[test]
    fn a_row_with_a_dot_component_is_refused_as_foreign() {
        let (_t, ctx) = library();
        {
            let conn = initialize(&ctx).unwrap();
            conn.execute(
                "INSERT INTO file_hashes (path, hash) VALUES (?1, 'h')",
                params![ctx.paths.root.join("plain.jpg").to_str().unwrap()],
            )
            .unwrap();
            // `<root>/../outside/x.jpg` starts with `<root>` lexically, so
            // only the `.`/`..` component check can refuse it.
            let escaped = format!("{}/../outside/x.jpg", ctx.paths.root.to_str().unwrap());
            conn.execute(
                "INSERT INTO file_hashes (path, hash) VALUES (?1, 'h')",
                params![escaped],
            )
            .unwrap();
        }
        let before = std::fs::read(&ctx.paths.db).unwrap();
        let before_cfg = std::fs::read(&ctx.paths.config).unwrap();
        let fresh = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap();
        let err = open_existing(&fresh).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("outside the library root"), "{msg}");
        assert!(msg.contains(".."), "{msg}");
        assert!(initialize(&fresh).is_err());
        assert_eq!(std::fs::read(&ctx.paths.db).unwrap(), before);
        assert_eq!(std::fs::read(&ctx.paths.config).unwrap(), before_cfg);
    }

    #[test]
    fn known_missing_media_under_the_root_remains_a_valid_library() {
        let (_t, ctx) = library();
        {
            let conn = initialize(&ctx).unwrap();
            conn.execute(
                "INSERT INTO file_hashes (path, hash) VALUES (?1, 'h')",
                params![ctx.paths.root.join("long-gone.jpg").to_str().unwrap()],
            )
            .unwrap();
        }
        // The file does not exist; the row is still under the root, so the
        // library stays usable (containment, not existence).
        let fresh = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap();
        let conn = open_existing(&fresh).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn root_aliases_open_the_same_initialized_library() {
        let (temp, ctx) = library();
        drop(initialize(&ctx).unwrap());
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&ctx.paths.root, &alias).unwrap();
        let via_alias = LibraryContext::new(&alias, &temp.path().join("cache")).unwrap();
        assert_eq!(via_alias.paths.db, ctx.paths.db);
        let conn = open_existing(&via_alias).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn a_future_schema_version_is_refused_with_an_actionable_error() {
        let (_t, ctx) = library();
        drop(initialize(&ctx).unwrap());
        {
            let conn = rusqlite::Connection::open(&ctx.paths.db).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }
        let err = open_existing(&ctx).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("newer"), "{msg}");
        assert!(msg.contains("version 2"), "{msg}");
        assert!(initialize(&ctx).is_err());
    }

    #[test]
    fn the_published_database_is_self_contained_and_leaves_no_temporaries() {
        let (_t, ctx) = library();
        drop(initialize(&ctx).unwrap());
        let conn = open_existing(&ctx).unwrap();
        // WAL everywhere is the existing behaviour; it persisted in the file
        // when the fresh database was built.
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        drop(conn);
        // A clean close leaves no sidecars and no build temporaries behind.
        let entries: Vec<String> = std::fs::read_dir(&ctx.paths.state)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        for entry in &entries {
            assert!(
                !entry.contains(".tmp") && !entry.ends_with("-wal") && !entry.ends_with("-shm"),
                "leftover {entry}: {entries:?}"
            );
        }
    }

    #[test]
    fn stale_build_wal_sidecars_are_swept_with_their_temporary() {
        let (_t, ctx) = library();
        std::fs::create_dir_all(&ctx.paths.state).unwrap();
        // The debris of a build that crashed mid-write: its temporary
        // database plus the WAL sidecars SQLite had opened beside it.
        let stem = "hashes.db.4242.0.tmp";
        for suffix in ["", "-wal", "-shm"] {
            std::fs::write(ctx.paths.state.join(format!("{stem}{suffix}")), b"debris").unwrap();
        }
        // An unrelated file, and the live database's own name, must not be
        // swept: the rule matches temp build stems, not any suffix.
        std::fs::write(ctx.paths.state.join("notes.txt"), b"keep").unwrap();
        drop(initialize(&ctx).unwrap());
        for suffix in ["", "-wal", "-shm"] {
            assert!(
                !ctx.paths.state.join(format!("{stem}{suffix}")).exists(),
                "sweeping must remove {stem}{suffix}"
            );
        }
        assert!(ctx.paths.state.join("notes.txt").exists());
        assert!(ctx.paths.db.exists());
    }

    #[test]
    fn a_refused_foreign_row_validation_is_never_memoized_as_success() {
        let (_t, ctx) = library();
        // A reader context separate from the initializing one, so its
        // validation memo starts clear: initialize marks its own context's
        // memo, and the refusal below must be observed by a context that has
        // not yet validated anything.
        let reader = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap();
        {
            let conn = initialize(&ctx).unwrap();
            conn.execute(
                "INSERT INTO file_hashes (path, hash) VALUES (?1, 'h')",
                params!["/elsewhere-not-this-library/x.jpg"],
            )
            .unwrap();
        }
        let err = open_existing(&reader).unwrap_err();
        assert!(
            format!("{err:#}").contains("outside the library root"),
            "{err:#}"
        );
        // The refusal left no memo. Had the failure been recorded the way a
        // success is, the next open on this same context would skip
        // validation and quietly serve the mixed library.
        assert!(
            !reader.index_validated(),
            "a refused validation must not be memoized"
        );
        // The operator repair: the foreign row removed directly via SQL, no
        // videre involvement, exactly the fix a copied database needs.
        {
            let conn = rusqlite::Connection::open(&ctx.paths.db).unwrap();
            let removed = conn
                .execute(
                    "DELETE FROM file_hashes WHERE path = '/elsewhere-not-this-library/x.jpg'",
                    [],
                )
                .unwrap();
            assert_eq!(removed, 1);
        }
        // The same context, which has now both refused and could have
        // remembered the refusal, revalidates and succeeds; and so does a
        // context built after the repair.
        drop(open_existing(&reader).unwrap());
        assert!(reader.index_validated());
        let fresh = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap();
        drop(open_existing(&fresh).unwrap());
    }

    #[test]
    fn opening_a_multi_row_library_validates_every_row_without_filesystem_probes() {
        let (_t, ctx) = library();
        // A multi-row library, enough to prove the validation generalises over
        // many rows rather than a single one. Deliberately small: the invariant
        // under test is scale-independent (see the two counters below), so N
        // does not need to mirror a real library. This was once 70,000 rows -
        // not for the invariant, but purely to give a wall-clock quadratic-walk
        // canary teeth. That canary was doubly bad: a timing bound on shared CI
        // is inherently flaky, and its large single-transaction write
        // intermittently tripped SQLITE_IOERR_WRITE on GitHub's macOS runner
        // (whose virtual disk fails writes under load). Both the clock and the
        // heavy write are gone; the deterministic counters prove the same thing
        // at any N, and the *expensive* form of a quadratic regression - per-row
        // filesystem work - is caught outright by the zero-probe assertion.
        const ROWS: u32 = 1_000;
        {
            let mut conn = initialize(&ctx).unwrap();
            // Bare path strings under the root, batched in one transaction. No
            // media files exist, which is the point; containment judges which
            // library a row belongs to, not whether its media is mounted.
            let tx = conn.transaction().unwrap();
            {
                let mut stmt = tx
                    .prepare("INSERT INTO file_hashes (path, hash) VALUES (?1, 'h')")
                    .unwrap();
                for i in 0..ROWS {
                    let path =
                        ctx.paths
                            .root
                            .join(format!("Trips/album-{:02}/img-{:05}.jpg", i / 100, i));
                    stmt.execute(params![path.to_str().unwrap()]).unwrap();
                }
            }
            tx.commit().unwrap();
        }
        // A fresh context has no memoized validation, so this open validates
        // every row. Two deterministic instruments answer the two halves of the
        // claim, at any row count: the guard layer's resolution counter is the
        // filesystem-probe instrument and must not move at all (no per-row
        // stat/canonicalise - the costly quadratic form), and the containment
        // row counter must have judged exactly ROWS rows (each visited once, a
        // linear pass), so a flat probe count cannot be explained by a
        // validation that never ran.
        let fresh = LibraryContext::new(&ctx.paths.root, &ctx.cache.base).unwrap();
        let probes_before = crate::library_guard::count_resolutions();
        let rows_before = count_rows_validated();
        let conn = open_existing(&fresh).unwrap();
        assert!(
            fresh.index_validated(),
            "a successful open records the memo"
        );
        assert_eq!(
            crate::library_guard::count_resolutions() - probes_before,
            0,
            "containment validation must not touch the filesystem per row"
        );
        assert_eq!(
            count_rows_validated() - rows_before,
            u64::from(ROWS),
            "every row must have been judged exactly once (a linear pass)"
        );
        let count: i64 = conn
            .query_row("SELECT count(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, i64::from(ROWS));
    }
}
