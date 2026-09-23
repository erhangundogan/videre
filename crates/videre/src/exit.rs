//! The binary's only way to end with a non-zero status. Calling
//! `std::process::exit` from a command skips destructors, which would lose
//! buffered log lines; commands return an `Exit` instead and `main` exits
//! after the logging guard has flushed.

use std::fmt;

#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    /// The failure behind the exit, when there is one; it is logged.
    pub error: Option<anyhow::Error>,
    /// The failure was already presented to the user (for example as JSON on
    /// stdout), so it is logged but not printed to stderr again.
    pub shown: bool,
}

impl Exit {
    /// A deliberate status with no failure of its own behind it: a health
    /// check reporting a problem, or a run whose per-item failures were
    /// already reported as they happened. Nothing more is logged.
    pub fn code(code: i32) -> Self {
        Self {
            code,
            error: None,
            shown: false,
        }
    }

    /// A failure the command already showed; logged, not printed again.
    pub fn shown(error: anyhow::Error) -> Self {
        Self {
            code: 1,
            error: Some(error),
            shown: true,
        }
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.error {
            Some(e) => write!(f, "{e:#}"),
            None => write!(f, "exit status {}", self.code),
        }
    }
}

impl std::error::Error for Exit {}
