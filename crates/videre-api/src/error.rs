/// Errors returned by videre-api operations. Each consumer maps these to its
/// own convention (axum -> StatusCode, Tauri -> a serializable error).
#[derive(Debug)]
pub enum Error {
    /// The target row/label does not exist (e.g. rename of an unknown person).
    NotFound,
    /// The requested change collides with existing state (e.g. rename onto an
    /// existing person).
    Conflict,
    /// Caller-supplied input was rejected (e.g. an empty label after sanitizing).
    Invalid,
    /// Underlying database failure.
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::Conflict => write!(f, "conflict"),
            Error::Invalid => write!(f, "invalid input"),
            Error::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
