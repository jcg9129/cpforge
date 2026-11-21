//! Non-job document inputs and shared report value types.
use crate::jobs::engine as e;
use crate::report::JobKind;
use std::path::PathBuf;

pub use crate::jobs::engine::{
    Fact, FactOutcome, Hook, HookResult, PreDeliveryExit, Problem, ReportPersistence, Target,
    WarningKind, WireWarning as Warning,
};
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub file: Option<PathBuf>,
    pub key: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub message: String,
}
#[derive(Clone, Debug)]
pub struct DocumentError {
    pub kind: &'static str,
    pub message: String,
    pub path: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug)]
pub enum FailureClass {
    Config,
    Workspace,
    Cancelled,
    Internal,
    Usage,
    Partial,
}
impl FailureClass {
    pub(super) fn token(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Workspace => "workspace",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
            Self::Usage => "usage",
            Self::Partial => "partial",
        }
    }
}
#[derive(Clone, Debug)]
pub struct ErrorDocument {
    pub platform: &'static str,
    pub command: String,
    pub class: FailureClass,
    pub exit: i64,
    pub error: DocumentError,
    pub diagnostics: Vec<Diagnostic>,
    pub diagnostics_dropped: u32,
}
#[derive(Clone, Debug)]
pub struct Refusal {
    pub platform: &'static str,
    pub cpf_version: String,
    pub command: JobKind,
    pub job: Option<e::JobIdentity>,
    pub workspace: Option<PathBuf>,
    pub problem: Option<Problem>,
    pub class: FailureClass,
    pub error: DocumentError,
    pub diagnostics: Vec<Diagnostic>,
    pub diagnostics_dropped: u32,
    pub warnings: Vec<Warning>,
    pub warnings_dropped: u32,
    pub events_dropped: u64,
    pub timing: e::Timing,
    pub cancellation: Option<e::Cancellation>,
    pub pre_delivery_exit: PreDeliveryExit,
}
