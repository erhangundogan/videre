//! A concrete two-library fixture for the directory-local command tests.
//!
//! Later tasks (search, inference, annotations, pruning) all need the same
//! thing: two real libraries whose logical state overlaps by content hash but
//! diverges by annotation, plus an unrelated launch directory to prove a run
//! is bound to its selected root rather than its cwd. Building that once here
//! keeps every consumer asserting against the same seeded shape.
//!
//! Seeding goes through the real schemas and core APIs, never the temporarily
//! disabled CLI. State is compared logically with [`snapshot_database`], never
//! by raw SQLite bytes: WAL and checkpointing rewrite bytes without changing
//! any row.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};

use super::TestLibrary;

/// The built-in model, used by library A.
pub const MODEL_A: &str = videre_core::embeddings::DEFAULT_MODEL_ID;
/// The other supported model, chosen by library B's config.
pub const MODEL_B: &str = "google/siglip2-base-patch16-384";

/// Every user table's rows, keyed by table name and ordered deterministically.
///
/// Opens read-only, enumerates user tables from `sqlite_schema`, reads each
/// table's columns in schema order and orders its rows by every column, so two
/// databases with the same logical contents produce equal maps regardless of
/// insertion order or on-disk byte layout. Identifiers are quoted by doubling
/// embedded double quotes; no external string is interpolated as SQL values.
pub fn snapshot_database(path: &Path) -> BTreeMap<String, Vec<Vec<Value>>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|e| panic!("open {} read-only: {e}", path.display()));
    let mut tables: Vec<String> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        for name in rows {
            tables.push(name.unwrap());
        }
    }

    let mut snapshot = BTreeMap::new();
    for table in tables {
        let quoted = format!("\"{}\"", table.replace('"', "\"\""));
        let mut stmt = conn.prepare(&format!("SELECT * FROM {quoted}")).unwrap();
        let column_count = stmt.column_count();
        // Order by every column position so row order never depends on storage.
        let order = (1..=column_count)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {quoted} ORDER BY {order}"))
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                let mut values = Vec::with_capacity(column_count);
                for i in 0..column_count {
                    values.push(row.get::<_, Value>(i)?);
                }
                Ok(values)
            })
            .unwrap();
        let mut collected = Vec::new();
        for row in rows {
            collected.push(row.unwrap());
        }
        snapshot.insert(table, collected);
    }
    snapshot
}

/// Every regular file in a cache directory, keyed by path, with its bytes.
///
/// Cleanup tests compare this before and after an unrelated library's prune to
/// prove one library never treats another's cached images as its orphans.
pub fn snapshot_cache_files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut stack: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    while let Some(path) = stack.pop() {
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(children) = std::fs::read_dir(&path) {
                stack.extend(children.flatten().map(|e| e.path()));
            }
        } else if meta.is_file() {
            if let Ok(bytes) = std::fs::read(&path) {
                out.insert(path, bytes);
            }
        }
    }
    out
}

/// The two seeded libraries plus an unrelated launch directory.
pub struct FeatureFixture {
    /// Library A: two paths of the shared hash plus a distinct EXIF photo, the
    /// built-in model, and its own annotations.
    pub a: TestLibrary,
    /// Library B: one path of the shared hash plus the EXIF photo, the other
    /// model in its config, and different annotations on the shared hash.
    pub b: TestLibrary,
    /// An unrelated directory a command may be launched from; never a library.
    pub c: TestLibrary,
    /// The content hash shared between A and B.
    pub shared_hash: String,
    /// The EXIF photo's hash, present in both and distinct from `shared_hash`.
    pub exif_hash: String,
}

impl FeatureFixture {
    /// Build and seed both libraries. Scans through the real command surface,
    /// then seeds annotations, model stores, classifications and location data
    /// by SQL and core APIs so no disabled CLI command is required.
    pub fn build() -> Self {
        let a = TestLibrary::new();
        let b = TestLibrary::new();
        let c = TestLibrary::new();

        // tiny.jpg is the shared hash; a second copy in A makes it a real
        // duplicate. sample_with_exif.jpg is a second, distinct hash.
        a.copy_fixture("tiny.jpg", "Trips/shared.jpg");
        a.copy_fixture("tiny.jpg", "Trips/shared-copy.jpg");
        a.copy_fixture("sample_with_exif.jpg", "Extra/exif.jpg");
        b.copy_fixture("tiny.jpg", "Trips/shared.jpg");
        b.copy_fixture("sample_with_exif.jpg", "Extra/exif.jpg");

        a.scan();
        b.scan();

        let shared_hash = hash_of(&a, "Trips/shared.jpg");
        let exif_hash = hash_of(&a, "Extra/exif.jpg");
        assert_ne!(shared_hash, exif_hash, "fixtures must not collide");

        seed_library_a(&a, &shared_hash, &exif_hash);
        seed_library_b(&b, &shared_hash, &exif_hash);

        FeatureFixture {
            a,
            b,
            c,
            shared_hash,
            exif_hash,
        }
    }
}

/// The content hash a scan recorded for a relative path.
///
/// Paths are stored under the library's canonical root, which differs from the
/// raw temp path whenever the temp directory sits under a symlink (macOS puts
/// `/var` behind `/private/var`), so the lookup joins onto the canonical root.
fn hash_of(library: &TestLibrary, relative: &str) -> String {
    let full = library.context().paths.root.join(relative);
    let conn = library.conn();
    conn.query_row(
        "SELECT hash FROM file_hashes WHERE path = ?1",
        [full.to_string_lossy().as_ref()],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|e| panic!("hash of {relative}: {e}"))
}

/// A deterministic unit-norm vector encoded as little-endian f16 bytes, so the
/// stored blob's length divides by two into a real dimension count.
fn normalized_f16_blob(dims: usize) -> Vec<u8> {
    let value = half::f16::from_f32(1.0 / (dims as f32).sqrt());
    let mut blob = Vec::with_capacity(dims * 2);
    for _ in 0..dims {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

fn insert_model_vectors(
    ctx: &videre_core::library::LibraryContext,
    model: &str,
    rows: &[(&str, usize)],
) {
    let conn = videre_core::library_db::open_existing(ctx).unwrap();
    videre_core::embeddings_db::attach_in(&conn, ctx, model, true).unwrap();
    let items: Vec<(String, Vec<u8>)> = rows
        .iter()
        .map(|(hash, dims)| (hash.to_string(), normalized_f16_blob(*dims)))
        .collect();
    videre_core::embeddings::insert_embeddings(&conn, model, &items).unwrap();
    videre_core::embeddings_db::detach(&conn).unwrap();
}

fn seed_library_a(a: &TestLibrary, shared: &str, exif: &str) {
    let ctx = a.context();
    let conn = a.conn();
    // Path-based updates must use the canonical root the scanner recorded.
    let shared_path = ctx.paths.root.join("Trips/shared.jpg");
    let copy_path = ctx.paths.root.join("Trips/shared-copy.jpg");

    // Dates and GPS: a null date/GPS/dimensions row, a dated duplicate, and a
    // valid Istanbul GPS row on the EXIF photo.
    conn.execute(
        "UPDATE file_hashes SET exif_date=NULL, gps_lat=NULL, gps_lon=NULL,
             width=NULL, height=NULL WHERE path=?1",
        [shared_path.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "UPDATE file_hashes SET exif_date='2025-06-20 09:30:00' WHERE path=?1",
        [copy_path.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "UPDATE file_hashes SET exif_date='2025-05-15 10:00:00',
             gps_lat=41.0082, gps_lon=28.9784, width=4000, height=3000,
             location_name='İstanbul, TR', location_cluster_id=1 WHERE hash=?1",
        [exif],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO location_clusters
             (id, centroid_lat, centroid_lon, name, photo_count, radius_km, created_at)
         VALUES (1, 41.0082, 28.9784, 'İstanbul, TR', 1, 0.5, '2025-06-01 00:00:00')",
        [],
    )
    .unwrap();

    // Marks and tags on the shared hash: A's version.
    conn.execute(
        "INSERT INTO marks (hash, rating, pick, label, liked, updated_at)
         VALUES (?1, 5, 1, 'keep', 1, '2025-06-01 00:00:00')",
        [shared],
    )
    .unwrap();
    videre_core::tags::set_tags(
        &conn,
        &[shared.to_string()],
        &["holiday".to_string(), "beach".to_string()],
    )
    .unwrap();

    // A face on the shared hash, confirmed as Çağla.
    seed_face(&conn, shared, "Çağla", "Çağla Yılmaz");

    // A model-specific classification on the shared hash.
    conn.execute(
        "INSERT INTO classifications (model_id, hash, category, confidence, classified_at)
         VALUES (?1, ?2, 'photo', 0.94, '2025-06-01 00:00:00')",
        [MODEL_A, shared],
    )
    .unwrap();
    drop(conn);

    // Two model stores in A with different counts: the built-in model over both
    // hashes, the other model over one.
    insert_model_vectors(&ctx, MODEL_A, &[(shared, 768), (exif, 768)]);
    insert_model_vectors(&ctx, MODEL_B, &[(shared, 1152)]);
}

fn seed_library_b(b: &TestLibrary, shared: &str, exif: &str) {
    // B's config chooses the other supported model.
    let ctx = b.context();
    videre_core::library_config::edit(
        &ctx,
        videre_core::library_config::ConfigKey::Model,
        Some(toml::Value::String(MODEL_B.to_string())),
    )
    .unwrap();

    let conn = b.conn();
    conn.execute(
        "UPDATE file_hashes SET exif_date='2025-05-15 10:00:00' WHERE hash=?1",
        [shared],
    )
    .unwrap();
    conn.execute(
        "UPDATE file_hashes SET exif_date='2025-06-01 12:00:00',
             gps_lat=41.0082, gps_lon=28.9784 WHERE hash=?1",
        [exif],
    )
    .unwrap();

    // Different marks and tags on the same shared hash.
    conn.execute(
        "INSERT INTO marks (hash, rating, pick, label, liked, updated_at)
         VALUES (?1, 2, 0, 'reject', 0, '2025-06-02 00:00:00')",
        [shared],
    )
    .unwrap();
    videre_core::tags::set_tags(&conn, &[shared.to_string()], &["work".to_string()]).unwrap();

    // The same face id resolves to a different confirmed person.
    seed_face(&conn, shared, "İpek", "İpek Kaya");
    drop(conn);

    insert_model_vectors(&ctx, MODEL_B, &[(shared, 1152)]);
}

/// Insert one confirmed face on `hash`, owned by `person`.
fn seed_face(conn: &Connection, hash: &str, person: &str, full_name: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO people (name, full_name) VALUES (?1, ?2)",
        [person, full_name],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed)
         VALUES (1, ?1, '[10.0,10.0,50.0,50.0]', ?2, 1, ?3, 1)",
        rusqlite::params![hash, vec![0u8; 8], person],
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO faces_scanned (hash) VALUES (?1)",
        [hash],
    )
    .unwrap();
}
