use anyhow::Result;

/// Placeholder until Phase 5 (XMP export) lands.
pub fn run(_args: &super::mark::MarkArgs, _conn: &rusqlite::Connection) -> Result<()> {
    anyhow::bail!("--export-xmp is not yet implemented")
}
