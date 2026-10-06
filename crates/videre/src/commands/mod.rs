pub mod classify;
pub mod cluster_settings;
pub mod config;
pub mod dedupe;
pub mod embed;
pub mod export;
pub mod export_jsonl;
pub mod faces;
pub mod fix_dates;
pub mod gallery;
pub mod import;
pub mod import_apple;
pub mod import_lightroom;
pub mod locations;
pub mod mark;
pub mod mark_export;
pub mod mcp;
pub mod pipeline;
pub mod prune;
pub mod scan;
pub mod search;
pub mod selection_args;
pub mod stats;
pub mod status;
pub mod tag;
pub mod watch;
pub(crate) mod watch_report;

/// Prompts on stderr and reads a yes/no answer from stdin. Any input other
/// than "y"/"yes" (case-insensitive) is treated as "no", including EOF (e.g.
/// stdin piped from /dev/null in a non-interactive context), the safe
/// default for a prompt gating a file mutation.
///
/// Shared by every command that modifies the user's files, so the answer to
/// "what counts as yes" cannot drift between them.
pub(crate) fn confirm(prompt: &str) -> anyhow::Result<bool> {
    use std::io::Write;
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Share of a set whose removal is implausible enough to stop for. Both
/// conditions must hold: a percentage alone would block a tiny fixture where a
/// few files were legitimately removed, and a raw count alone would never trip
/// on a small library. Shared by prune (row cleanup) and dedupe trash (file
/// deletion) so one guard governs every bulk removal.
pub(crate) const BULK_DELETE_FRACTION: f64 = 0.20;
pub(crate) const BULK_DELETE_MIN_ROWS: usize = 100;

pub(crate) fn is_bulk_delete(to_remove: usize, total: usize) -> bool {
    to_remove >= BULK_DELETE_MIN_ROWS && (to_remove as f64) > (total as f64) * BULK_DELETE_FRACTION
}

/// The MCP `find_duplicates` tool's document: the same one `dedupe --json`
/// prints, so the two surfaces cannot silently diverge in shape. Never
/// decodes; unchecked resized candidates are counted instead.
pub(crate) fn build_find_duplicates(
    db: &std::path::Path,
    kinds: &[videre::duplicates::Kind],
) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let conn = videre_core::db::open_wal(db)?;
    let found = videre::duplicates::find(&conn, kinds, false, true)?;
    duplicates_document(&conn, kinds, found)
}

/// `found` as the versioned duplicates document.
pub(crate) fn duplicates_document(
    conn: &rusqlite::Connection,
    kinds: &[videre::duplicates::Kind],
    found: videre::duplicates::Found,
) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    let mut kinds = kinds.to_vec();
    kinds.sort();
    kinds.dedup();
    Ok(videre::types::FindDuplicatesJson {
        schema_version: videre::types::DUPLICATES_SCHEMA_VERSION,
        total_files: total_files.max(0) as usize,
        kinds: kinds.iter().map(|k| k.name()).collect(),
        unchecked: found.unchecked,
        groups: found.groups.into_iter().map(Into::into).collect(),
    })
}

/// Validate `--model` at parse time, so a typo fails before anything opens a
/// database or loads a model.
///
/// The value is validated again inside `resolve_model_id`, which is the real
/// guard - this one exists so the error arrives when the user typed it rather
/// than after an unrelated "no database found". `search` already treats
/// `--sort` this way.
pub(crate) fn parse_model_id(s: &str) -> Result<String, String> {
    videre_core::embeddings::validate_model_id(s)
        .map(|_| s.to_string())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::is_bulk_delete;

    #[test]
    fn is_bulk_delete_needs_both_fraction_and_floor() {
        assert!(!is_bulk_delete(3, 5)); // 60% but under the 100 floor
        assert!(!is_bulk_delete(100, 1000)); // at floor but exactly 10% (not > 20%)
        assert!(is_bulk_delete(300, 1000)); // over floor and over 20%
    }
}
