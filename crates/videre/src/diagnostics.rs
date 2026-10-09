//! The documents `videre status --json` and `videre stats --json` print, built
//! in one place so the gallery's Diagnostics page shows exactly what the CLI
//! does. The caller opens the connection and holds whatever lock it needs.

use videre::types::{StatsJson, StatusJson, SCHEMA_VERSION};
use videre_core::library::LibraryContext;

/// `videre status --json`. Attaches the default model's embeddings for the
/// coverage numbers, read-only.
pub fn status_json(
    conn: &rusqlite::Connection,
    library: &LibraryContext,
) -> anyhow::Result<StatusJson> {
    videre_core::embeddings_db::attach_for_read_or_placeholder_in(
        conn,
        library,
        &library.settings.default_model,
    )?;
    Ok(StatusJson {
        schema_version: SCHEMA_VERSION,
        report: videre_core::status_report::compute_status_in(conn, library)?,
    })
}

/// `videre stats --json`, listing at most `mismatches` mismatched files
/// (`None` for all of them).
pub fn stats_json(
    conn: &rusqlite::Connection,
    library: &LibraryContext,
    mismatches: Option<usize>,
) -> anyhow::Result<StatsJson> {
    Ok(StatsJson {
        schema_version: SCHEMA_VERSION,
        library: videre_core::library_stats::compute_full_in(conn, library)?,
        by_type: videre_core::library_stats::by_type(conn, usize::MAX)?,
        mismatches: videre_core::library_stats::mismatched_files(conn, mismatches)?,
        disk_use: videre_core::disk::usage_in(library),
    })
}
