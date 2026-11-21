//! Supplemental wire evidence absent from the engine's finalized value.
use super::run::SummaryMetadata;
use crate::config::Comparator;
use crate::jobs::engine::{self as e, CancelPhase};
use crate::report::{HookOutcome, JobKind};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Problem {
    pub identity: String,
    pub name: String,
    pub kind: String,
    pub dir: PathBuf,
}
#[derive(Clone, Debug)]
pub struct Target {
    pub name: Option<String>,
    pub language: Option<String>,
    pub profile: Option<String>,
    pub build_free: bool,
    pub compare: Option<Comparator>,
    pub run_deadline: Option<(u64, &'static str)>,
    pub published_limits: Option<(Option<u32>, Option<u64>)>,
    pub local_limits: Vec<crate::supervisor::InstalledLimit>,
}
#[derive(Clone, Debug)]
pub enum FactOutcome {
    Published,
    Superseded,
    NotPublished {
        reason: &'static str,
        message: Option<String>,
    },
    NotAttempted {
        reason: &'static str,
    },
}
#[derive(Clone, Debug)]
pub struct Fact {
    pub outcome: FactOutcome,
    pub warnings: Vec<&'static str>,
}
#[derive(Clone, Debug)]
pub enum HookResult {
    Planned(HookOutcome),
    NotPlanned { reason: &'static str },
}
#[derive(Clone, Debug)]
pub struct Hook {
    pub name: String,
    pub trigger: &'static str,
    pub execution: Option<u32>,
    pub outcome: HookResult,
}
#[derive(Clone, Debug)]
pub enum WarningKind {
    Prune {
        job: Option<crate::storage::job::JobId>,
        detail: &'static str,
    },
    Recovery {
        paths: u32,
        first: Option<PathBuf>,
    },
    Persist {
        artefact: &'static str,
        run: String,
        error: e::PersistError,
    },
    Durability {
        artefact: &'static str,
        detail: &'static str,
    },
    Retention {
        detail: &'static str,
        run: Option<String>,
        bytes: u64,
    },
    CleanupUnverified {
        runs: u32,
    },
    Fact(FactOutcome),
    Hook {
        name: String,
        outcome: HookResult,
    },
    HooksDisabled {
        reason: &'static str,
    },
    LateCancellation {
        phase: CancelPhase,
    },
    ProgressDropped {
        lines: u32,
    },
}
#[derive(Clone, Debug)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
}
/// Already selected at finalization; serialization never selects process status.
#[derive(Clone, Copy, Debug)]
pub struct PreDeliveryExit {
    pub code: i64,
    pub reraise: Option<&'static str>,
}
#[derive(Clone, Debug)]
pub struct ReportMetadata {
    pub problem: Problem,
    pub target: Option<Target>,
    pub fingerprints: Option<([u8; 32], [u8; 32])>,
    pub fact: Option<Fact>,
    pub hooks: Vec<Hook>,
    pub hooks_dropped: u32,
    /// Extra warnings follow engine warnings. An override supplies wire details
    /// (notably the artefact/run of an engine persistence warning) by index.
    pub warnings: Vec<Warning>,
    pub warning_overrides: BTreeMap<usize, Warning>,
    pub warnings_dropped: u32,
    pub summaries: BTreeMap<u32, SummaryMetadata>,
    pub pre_delivery_exit: PreDeliveryExit,
    pub internal_message: String,
    /// Explicit publication observation until the engine retains the manifest.
    pub capture_manifest: Option<PathBuf>,
}
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
#[derive(Clone, Debug)]
pub enum ReportPersistence {
    Published {
        path: PathBuf,
        uncertain: bool,
    },
    Failed {
        error: e::PersistError,
        message: String,
    },
    Refusal,
}
