//! Pure execution preparation: invocation declarations, native placeholder
//! expansion, absolute cwd selection and environment steps 1–3 (§4.3).
//!
//! Construct a [`Context`] from the invocation's plan and caller-allocated paths,
//! capture one [`Baseline`] per job, then call [`prepare`] for each invocation.
//! Source paths are exactly the supplied paths (captured copies when applicable);
//! this module never reads sources, allocates paths or falls back to originals.
//! Contexts retain their originating stage to prevent reuse with another plan's
//! declarations. Supplied undeclared bindings are construction errors.
//!
//! [`PreparedStage`] is not launchable. Program lookup, launchability, generated
//! programs and launch kinds belong to C2b; shell lowering belongs to C2c. Job
//! admission later injects `CPF_*`, stdin, sinks and normalized limits. Neither
//! preparation nor these types persist, hash or construct an execution spec.

mod context;
mod error;
mod prepare;

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::config::{Deadline, Limits, ProgramValue, Sourced, StageKind, StagePlan, StreamCap};

pub use context::{
    CommandBindings, Context, InvocationDirs, LanguageBinding, ProblemBinding, TargetBindings,
    TestBinding,
};
pub use error::{PrepareError, PrepareErrorKind, PrepareErrors};
pub use prepare::prepare;

/// One ordered native environment snapshot, captured once by the job caller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    entries: Vec<(OsString, OsString)>,
}

impl Baseline {
    /// Capture the process environment; preparation itself never reads it.
    pub fn capture() -> Self {
        Self::from_entries(std::env::vars_os().collect())
    }

    /// Explicit ordered snapshot, useful for tests and injected job baselines.
    pub fn from_entries(entries: Vec<(OsString, OsString)>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[(OsString, OsString)] {
        &self.entries
    }
}

/// A request only; no PATH search or filesystem inspection has occurred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramRequest {
    Path(PathBuf),
    Lookup(OsString),
}

/// Shell parts are retained literally for C2c's positional-argument adapter.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)] // Preserve the original interpreter metadata.
pub enum PreparedCommand {
    Argv {
        program: ProgramRequest,
        /// Excludes argv[0]; includes CLI extra arguments.
        args: Vec<OsString>,
    },
    Shell {
        interpreter: ProgramValue,
        script: Sourced<String>,
        args: Vec<OsString>,
    },
}

/// Expanded but unresolved, with the original plan's complete provenance.
/// Environment order is baseline order, replacements in place under the winning
/// spelling, then new names in edit order. Duplicate baseline names use the same
/// rule (last spelling/value, first position). Removed and re-added names append.
/// `environment` is exactly §4.3 steps 1–3; job admission owns step 4 (`CPF_*`).
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedStage {
    pub kind: StageKind,
    pub command: PreparedCommand,
    pub cwd: PathBuf,
    pub shared_cwd: bool,
    pub environment: Vec<(OsString, OsString)>,
    pub deadline: Deadline,
    pub stdout: StreamCap,
    pub stderr: StreamCap,
    /// Unnormalized C1 limits; support/rounding checks remain with admission.
    pub limits: Limits,
    pub plan: StagePlan,
}

pub(super) fn contains_nul(value: &OsStr) -> bool {
    // NUL has the same representation in UTF-8, WTF-8 and native Unix bytes.
    value.as_encoded_bytes().contains(&0)
}

#[cfg(test)]
mod tests;
