//! Application failure classes, independent of their diagnostic text.
use crate::exit::{ExitCode, ExitInputs, exit_status};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClass {
    Usage,
    Config,
    Workspace,
    Partial,
    Infrastructure,
    Delivery,
    Cancelled,
    Internal,
}
impl FailureClass {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Config => "config",
            Self::Workspace => "workspace",
            Self::Partial => "partial",
            Self::Infrastructure => "infrastructure",
            Self::Delivery => "delivery",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Cli(Box<clap::Error>),
    Config(String),
    Workspace(String),
    Partial(String),
    Infrastructure(String),
    Delivery(String),
    Cancelled(String),
    Internal(String),
}
impl Error {
    pub fn class(&self) -> FailureClass {
        match self {
            Self::Cli(_) => FailureClass::Usage,
            Self::Config(_) => FailureClass::Config,
            Self::Workspace(_) => FailureClass::Workspace,
            Self::Partial(_) => FailureClass::Partial,
            Self::Infrastructure(_) => FailureClass::Infrastructure,
            Self::Delivery(_) => FailureClass::Delivery,
            Self::Cancelled(_) => FailureClass::Cancelled,
            Self::Internal(_) => FailureClass::Internal,
        }
    }
    pub fn exit_code(&self) -> ExitCode {
        if matches!(self, Self::Cli(error) if error.exit_code() == 0) {
            return ExitCode::Success;
        }
        exit_status(&ExitInputs {
            failure: Some(self.class()),
            machine_output: matches!(self, Self::Delivery(_)),
            ..ExitInputs::default()
        })
    }
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli(error) => {
                let message = error.to_string();
                formatter.write_str(message.strip_prefix("error: ").unwrap_or(&message))
            }
            Self::Config(message)
            | Self::Workspace(message)
            | Self::Partial(message)
            | Self::Infrastructure(message)
            | Self::Delivery(message)
            | Self::Cancelled(message)
            | Self::Internal(message) => formatter.write_str(message),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cli(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}
impl From<crate::storage::StorageError> for Error {
    fn from(error: crate::storage::StorageError) -> Self {
        Self::Workspace(error.to_string())
    }
}
impl From<clap::Error> for Error {
    fn from(error: clap::Error) -> Self {
        Self::Cli(Box::new(error))
    }
}
