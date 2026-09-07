//! Facade over videre-core's pipeline run tracking. See
//! docs/superpowers/specs/2026-07-31-dashboard-stats-pass-b-design.md.

use crate::error::Result;
use rusqlite::Connection;
use videre_core::library::LibraryContext;
pub use videre_core::pipeline_runs::PipelineRunStatus;

pub fn pipeline_status(conn: &Connection, ctx: &LibraryContext) -> Result<Vec<PipelineRunStatus>> {
    Ok(videre_core::pipeline_runs::read_all_in(conn, ctx)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_status_reports_all_tracked_commands_never_run() {
        let conn = Connection::open_in_memory().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let ctx = LibraryContext::new(temp.path(), &temp.path().join("cache")).unwrap();

        let statuses = pipeline_status(&conn, &ctx).unwrap();
        assert_eq!(
            statuses.len(),
            videre_core::pipeline_runs::TRACKED_COMMANDS.len()
        );
        assert!(statuses.iter().all(|s| s.status.is_none()));
    }
}
