use std::sync::Mutex;

/// The app's single open connection to the videre library database, shared
/// across all command invocations. Opened once at startup from the same
/// default-db resolution the CLI uses.
pub struct DbState(pub Mutex<rusqlite::Connection>);

impl DbState {
    pub fn open() -> anyhow::Result<Self> {
        let path = videre_core::home::resolve_db(None)?;
        if !path.exists() {
            anyhow::bail!(
                "no database found at {}; run 'videre scan <dir>' first",
                path.display()
            );
        }
        let conn = videre_core::db::open_wal(&path)?;
        Ok(DbState(Mutex::new(conn)))
    }
}
