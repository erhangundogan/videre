pub mod classify;
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
pub mod import_takeout;
pub mod locations;
pub mod mark;
pub mod mark_export;
pub mod mcp;
pub mod prune;
pub mod scan;
pub mod search;
pub mod selection_args;
pub mod stats;
pub mod tag;
pub mod watch;

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

/// Shared by `dedupe --json` and the MCP `find_duplicates` tool so the two
/// surfaces cannot silently diverge in shape.
pub(crate) fn build_find_duplicates(
    db: &std::path::Path,
    include_similar: bool,
) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let conn = videre_core::db::open_wal(db)?;
    build_find_duplicates_from(&conn, include_similar)
}

/// The connection-based twin, for directory-local commands that already hold a
/// validated connection to the selected library and must not reopen the file.
pub(crate) fn build_find_duplicates_from(
    conn: &rusqlite::Connection,
    include_similar: bool,
) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let records = videre::sqlite_output::load_records_from(conn)?;
    let total_files = records.len();
    let duplicate_groups = videre::output::find_duplicate_groups(&records)
        .into_iter()
        .map(videre::types::DupGroupJson::from)
        .collect();
    let similar_groups = include_similar.then(|| {
        videre::output::find_similar_groups(&records, 10)
            .into_iter()
            .map(videre::types::SimilarGroupJson::from)
            .collect()
    });
    Ok(videre::types::FindDuplicatesJson {
        schema_version: videre::types::SCHEMA_VERSION,
        total_files,
        duplicate_groups,
        similar_groups,
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
