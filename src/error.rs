//! Application errors.

use std::fmt;

use crate::exit::ExitCode;

/// An argument error or an unexpected application failure.
#[derive(Debug)]
pub enum Error {
    Cli(clap::Error),
    Internal(String),
}

impl Error {
    /// Select the process status, preserving clap's successful help/version exits.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Cli(error) if error.exit_code() == 0 => ExitCode::Success,
            Self::Cli(_) => ExitCode::Usage,
            Self::Internal(_) => ExitCode::Internal,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli(error) => {
                let message = error.to_string();
                formatter.write_str(message.strip_prefix("error: ").unwrap_or(&message))
            }
            Self::Internal(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cli(error) => Some(error),
            Self::Internal(_) => None,
        }
    }
}

impl From<clap::Error> for Error {
    fn from(error: clap::Error) -> Self {
        Self::Cli(error)
    }
}
