/// Errors returned by videre-api operations. Each consumer maps these to its
/// own convention (axum -> StatusCode, other embedders -> their own error type).
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
    /// Any other failure surfaced as a plain message (e.g. from videre-core
    /// functions that return anyhow::Error, like pipeline_runs).
    Other(String),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e.to_string())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::Conflict => write!(f, "conflict"),
            Error::Invalid => write!(f, "invalid input"),
            Error::Db(e) => write!(f, "database error: {e}"),
            Error::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_each_variant() {
        assert_eq!(Error::NotFound.to_string(), "not found");
        assert_eq!(Error::Conflict.to_string(), "conflict");
        assert_eq!(Error::Invalid.to_string(), "invalid input");
        assert_eq!(Error::Other("boom".to_string()).to_string(), "boom");
    }

    #[test]
    fn display_for_db_variant_includes_the_underlying_error() {
        let e = Error::Db(rusqlite::Error::QueryReturnedNoRows);
        assert!(e.to_string().starts_with("database error: "));
        assert!(e.to_string().contains("Query returned no rows"));
    }

    #[test]
    fn from_rusqlite_error_wraps_as_db_variant() {
        let e: Error = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(matches!(e, Error::Db(_)));
    }

    #[test]
    fn from_anyhow_error_wraps_message_as_other_variant() {
        let e: Error = anyhow::anyhow!("something broke").into();
        match e {
            Error::Other(msg) => assert_eq!(msg, "something broke"),
            other => panic!("expected Error::Other, got {other:?}"),
        }
    }
}
