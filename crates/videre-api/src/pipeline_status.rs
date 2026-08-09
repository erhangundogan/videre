//! Facade over videre-core's pipeline run tracking. See
//! docs/superpowers/specs/2026-07-31-dashboard-stats-pass-b-design.md.

use crate::error::Result;
use rusqlite::Connection;
use std::path::Path;
pub use videre_core::pipeline_runs::PipelineRunStatus;

pub fn pipeline_status(conn: &Connection, db_path: &Path) -> Result<Vec<PipelineRunStatus>> {
    Ok(videre_core::pipeline_runs::read_all(conn, db_path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_status_reports_all_tracked_commands_never_run() {
        let conn = Connection::open_in_memory().unwrap();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let statuses = pipeline_status(&conn, db_file.path()).unwrap();
        assert_eq!(
            statuses.len(),
            videre_core::pipeline_runs::TRACKED_COMMANDS.len()
        );
        assert!(statuses.iter().all(|s| s.status.is_none()));
    }
}
