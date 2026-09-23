//! Known classes of failure, attached where the cause is detected and read
//! where the error is logged. A lower level adds one with
//! `.context(ErrorKind::...)`; the boundary that logs the error finds it with
//! [`ErrorKind::in_chain`] and records its code and remediation. Adding a
//! variant is how a new failure class gets a remediation; nothing matches on
//! error text.

/// One known failure class. The set is deliberately small: most failures are
/// environmental, and a handful of classes covers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    SourceUnavailable,
    PermissionDenied,
    DecodeFailed,
    QuicklookUnavailable,
    ModelUnavailable,
    LibraryBusy,
    LibrarySchema,
    Database,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 8] = [
        ErrorKind::SourceUnavailable,
        ErrorKind::PermissionDenied,
        ErrorKind::DecodeFailed,
        ErrorKind::QuicklookUnavailable,
        ErrorKind::ModelUnavailable,
        ErrorKind::LibraryBusy,
        ErrorKind::LibrarySchema,
        ErrorKind::Database,
    ];

    /// Stable machine code, written to the log. Never rename one: readers
    /// and saved logs depend on it.
    pub fn code(self) -> &'static str {
        match self {
            ErrorKind::SourceUnavailable => "source_unavailable",
            ErrorKind::PermissionDenied => "permission_denied",
            ErrorKind::DecodeFailed => "decode_failed",
            ErrorKind::QuicklookUnavailable => "quicklook_unavailable",
            ErrorKind::ModelUnavailable => "model_unavailable",
            ErrorKind::LibraryBusy => "library_busy",
            ErrorKind::LibrarySchema => "library_schema",
            ErrorKind::Database => "database",
        }
    }

    /// What the user can do about it, when there is something to do.
    pub fn remediation(self) -> Option<&'static str> {
        match self {
            ErrorKind::SourceUnavailable => {
                Some("Reconnect the drive holding the library, then run the command again.")
            }
            ErrorKind::PermissionDenied => Some("Grant read access to the file or folder."),
            ErrorKind::DecodeFailed => {
                Some("The file is unreadable or unsupported; videre skips it after two attempts.")
            }
            ErrorKind::QuicklookUnavailable => Some(
                "HEIC and video need macOS QuickLook; these files are skipped on this platform.",
            ),
            ErrorKind::ModelUnavailable => {
                Some("Check network access for the first run, or the Hugging Face cache location.")
            }
            ErrorKind::LibraryBusy => {
                Some("Another videre command is using this library; retry when it finishes.")
            }
            ErrorKind::LibrarySchema | ErrorKind::Database => None,
        }
    }

    /// The first kind attached anywhere in `err`'s context chain.
    pub fn in_chain(err: &anyhow::Error) -> Option<ErrorKind> {
        err.downcast_ref::<ErrorKind>().copied()
    }
}

impl std::fmt::Display for ErrorKind {
    /// A short phrase that reads naturally inside `{e:#}` output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ErrorKind::SourceUnavailable => "the file could not be read from its drive",
            ErrorKind::PermissionDenied => "permission denied",
            ErrorKind::DecodeFailed => "the file could not be decoded",
            ErrorKind::QuicklookUnavailable => "QuickLook is unavailable",
            ErrorKind::ModelUnavailable => "the model could not be loaded",
            ErrorKind::LibraryBusy => "the library is busy",
            ErrorKind::LibrarySchema => "the library database needs attention",
            ErrorKind::Database => "database error",
        })
    }
}

impl std::error::Error for ErrorKind {}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn codes_are_unique_and_snake_case() {
        let mut seen = std::collections::HashSet::new();
        for kind in ErrorKind::ALL {
            let code = kind.code();
            assert!(seen.insert(code), "duplicate code {code}");
            assert!(
                code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{code}"
            );
        }
    }

    #[test]
    fn a_kind_is_found_anywhere_in_the_context_chain() {
        let err = Err::<(), _>(std::io::Error::other("read timed out"))
            .context(ErrorKind::SourceUnavailable)
            .context("hash /Volumes/Fotoğraflar/İstanbul/Çağla_2019.jpg")
            .unwrap_err();
        assert_eq!(
            ErrorKind::in_chain(&err),
            Some(ErrorKind::SourceUnavailable)
        );
    }

    #[test]
    fn an_error_without_a_kind_has_none() {
        let err = anyhow::anyhow!("plain failure").context("outer");
        assert_eq!(ErrorKind::in_chain(&err), None);
    }

    #[test]
    fn the_label_reads_as_part_of_a_sentence() {
        let err = anyhow::anyhow!("timed out").context(ErrorKind::SourceUnavailable);
        assert_eq!(
            format!("{err:#}"),
            "the file could not be read from its drive: timed out"
        );
    }
}
