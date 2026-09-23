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
    /// stdout), so `main` must not print it to stderr again.
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

/// What `main` does with a command's result.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub code: i32,
    /// The error text to log, if any.
    pub log: Option<String>,
    /// Whether the error is also printed to stderr.
    pub print: bool,
}

pub fn classify(result: &anyhow::Result<()>) -> Outcome {
    match result {
        Ok(()) => Outcome {
            code: 0,
            log: None,
            print: false,
        },
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(exit) => Outcome {
                code: exit.code,
                log: exit.error.as_ref().map(|e| format!("{e:#}")),
                print: exit.error.is_some() && !exit.shown,
            },
            None => Outcome {
                code: 1,
                log: Some(format!("{e:#}")),
                print: true,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_each_shape_to_its_code_and_logging() {
        assert_eq!(
            classify(&Ok(())),
            Outcome {
                code: 0,
                log: None,
                print: false
            }
        );
        let plain = Err(anyhow::anyhow!("boom"));
        let o = classify(&plain);
        assert_eq!((o.code, o.print, o.log.is_some()), (1, true, true));
        let signal: anyhow::Result<()> = Err(Exit::code(2).into());
        assert_eq!(
            classify(&signal),
            Outcome {
                code: 2,
                log: None,
                print: false
            }
        );
        let shown: anyhow::Result<()> =
            Err(Exit::shown(anyhow::anyhow!("json already printed")).into());
        let o = classify(&shown);
        assert_eq!((o.code, o.print, o.log.is_some()), (1, false, true));
    }
}
