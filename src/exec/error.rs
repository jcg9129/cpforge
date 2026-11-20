use std::fmt;

use crate::config::{Origin, Placeholder, Provenance, StageKind};

/// Distinct invocation and expansion failures, without conflating a missing
/// declaration with a missing value or a target's unavailable capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareErrorKind {
    NoProblem,
    NoWorkspace,
    Undeclared {
        declaration: &'static str,
    },
    HookPlaceholder,
    /// Test and iteration placeholders exist only for commands (§5.2).
    CommandOnly,
    NoFileArtifact,
    NoBuild,
    NoEntry,
    NoTestAnswer,
    SourceCount(usize),
    MissingBinding(&'static str),
    InvalidBinding(&'static str),
    ContextMismatch,
    NotAbsolute,
    EmptyProgram,
    EmptyCommand,
    Nul,
    MultiInScalar,
}

impl fmt::Display for PrepareErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProblem => f.write_str("no selected problem"),
            Self::NoWorkspace => f.write_str("not inside a workspace"),
            Self::Undeclared { declaration } => {
                write!(f, "placeholder not declared for this command (missing {declaration})")
            }
            Self::HookPlaceholder => f.write_str("placeholder is not declared in hook context"),
            Self::CommandOnly => f.write_str("placeholder is available only to commands"),
            Self::NoFileArtifact => f.write_str("target has no file artifact"),
            Self::NoBuild => f.write_str("target has no build"),
            Self::NoEntry => f.write_str("language declares no entry"),
            Self::NoTestAnswer => f.write_str("test has no answer"),
            Self::SourceCount(count) => write!(f, "source requires exactly one source, found {count}"),
            Self::MissingBinding(name) => write!(f, "missing {name} binding"),
            Self::InvalidBinding(reason) => f.write_str(reason),
            Self::ContextMismatch => f.write_str("context belongs to a different stage plan"),
            Self::NotAbsolute => f.write_str("path must be absolute (Windows drive-relative and root-relative forms are not allowed)"),
            Self::EmptyProgram => f.write_str("program must not be empty"),
            Self::EmptyCommand => f.write_str("argv must include a program"),
            Self::Nul => f.write_str("value contains NUL"),
            Self::MultiInScalar => f.write_str("multi placeholder cannot occupy a scalar position"),
        }
    }
}

/// One error tied to the C1 leaf whose expansion or binding failed.
#[derive(Clone, Debug, PartialEq)]
pub struct PrepareError {
    pub stage: StageKind,
    pub field: String,
    pub placeholder: Option<Placeholder>,
    pub kind: PrepareErrorKind,
    pub origin: Origin,
    pub provenance: Provenance,
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?} stage, {}", self.origin, self.stage, self.field)?;
        if let Some(placeholder) = self.placeholder {
            write!(f, " {{{}}}", placeholder.name())?;
        }
        write!(f, ": {}", self.kind)
    }
}

/// All independent errors from one construction or preparation attempt.
#[derive(Clone, Debug, PartialEq)]
pub struct PrepareErrors {
    pub errors: Vec<PrepareError>,
}

impl fmt::Display for PrepareErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index != 0 {
                writeln!(f)?;
            }
            write!(f, "{error}")?;
        }
        Ok(())
    }
}
impl std::error::Error for PrepareErrors {}
