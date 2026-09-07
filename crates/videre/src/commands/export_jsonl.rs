//! `videre export --jsonl`: a fixed-path, atomically-replaced JSONL snapshot of
//! the selected library's scan inventory at `.videre/hashes.jsonl`.
//!
//! One record per file path, streamed straight out of one read transaction so
//! the snapshot is internally consistent, and published through
//! [`videre_core::atomic_file::publish`] so a partial write can never replace a
//! previous complete snapshot. It is a scan-inventory snapshot, not an
//! annotations or embeddings backup.

use crate::command_context::CommandContext;
use anyhow::Result;
use std::io::Write;
use videre_core::selection::{RowSelection, SelectionCtx};

/// Write (or, when `dry_run`, only count) the JSONL snapshot for `selection`.
///
/// Owns the read transaction and resolves the selection inside it, so the count
/// and the rows come from one consistent view. The transaction is committed
/// inside the writer callback, after every row has been flushed and before the
/// atomic rename, so a selection, read, serialization or transaction-close
/// failure leaves the previous snapshot intact.
pub fn write_snapshot(
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    selection: &RowSelection,
    dry_run: bool,
) -> Result<usize> {
    let transaction = conn.unchecked_transaction()?;
    let resolved = selection.resolve_in(&transaction, &SelectionCtx::default(), &ctx.library)?;
    let hashes = resolved.hashes;
    let query = format!(
        "SELECT {} FROM file_hashes ORDER BY path",
        videre::sqlite_output::FILE_RECORD_COLUMNS
    );

    if dry_run {
        let mut statement = transaction.prepare(&query)?;
        let rows = statement.query_map([], videre::sqlite_output::file_record_from_row)?;
        let mut count = 0usize;
        for row in rows {
            let record = row?;
            if hashes
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&record.hash))
            {
                continue;
            }
            count += 1;
        }
        drop(statement);
        transaction.commit()?;
        return Ok(count);
    }

    let mut count = 0usize;
    videre_core::atomic_file::publish(&ctx.library.paths.jsonl, |file| {
        let mut writer = std::io::BufWriter::new(file);
        let mut statement = transaction.prepare(&query)?;
        let rows = statement.query_map([], videre::sqlite_output::file_record_from_row)?;
        for row in rows {
            let record = row?;
            if hashes
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&record.hash))
            {
                continue;
            }
            serde_json::to_writer(&mut writer, &record)?;
            writer.write_all(b"\n")?;
            count += 1;
        }
        writer.flush()?;
        drop(writer);
        drop(statement);
        // Close the read transaction inside the callback, before publish's
        // atomic rename, so there is no fallible database work after publication.
        transaction.commit()?;
        ctx.library.ensure_root_identity()?;
        Ok(())
    })?;
    Ok(count)
}
