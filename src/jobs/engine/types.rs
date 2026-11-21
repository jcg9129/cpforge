use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::Cancellation;
use crate::build::BuildId;
use crate::config::{Comparator, TargetPlan};
use crate::exec::{LaunchCommand, ProblemBinding, ResolvedStage};
use crate::jobs::CaptureCheck;
use crate::report::{
    self, BuildResult, Counts, ExecutionResult, InternalFailure, JobKind, JobVerdict,
};
use crate::storage::inputs::{CapturedSourceCopy, SourceRole};
use crate::storage::job::{Epoch, JobId, JobOwner};
use crate::supervisor::{RootOutcome, StreamCapture};
use crate::tests_store::FrozenTest;

/// Admission identity, distinct from the plan index. Only the engine allocates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunId(pub(super) String);
impl RunId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Build,
    Run,
    Command,
    Hook,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    Build,
    Test {
        test: String,
    },
    Command {
        test: Option<String>,
        iteration: u32,
    },
    Raw,
    Hook {
        name: String,
        trigger: String,
    },
}

/// Generic, already-preflighted iterations. A target run is resolved only
/// through the build capability after successful validation.
#[derive(Clone, Debug)]
pub enum Recipe {
    TargetRun,
    Resolved(Box<ResolvedStage>),
}

#[derive(Clone, Debug)]
pub struct Iteration {
    pub role: Role,
    pub recipe: Recipe,
    pub stdin: Option<PathBuf>,
    pub depends_on_build: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Cmd,
    Run,
    Hook,
}
impl CommandKind {
    pub(super) fn job_kind(self) -> JobKind {
        match self {
            Self::Cmd => JobKind::Cmd,
            Self::Run => JobKind::Run,
            Self::Hook => JobKind::Hook,
        }
    }
}

#[derive(Debug)]
pub enum JobRequest {
    Build,
    /// The ready job's frozen manifest is the only source of tests and answers.
    Test {
        comparator: Comparator,
    },
    Iterations {
        kind: CommandKind,
        iterations: Vec<Iteration>,
        needs_build: bool,
        keep_going: bool,
    },
}

#[derive(Clone, Debug)]
pub struct PlannedExecution {
    pub index: u32,
    pub stage: Stage,
    pub role: Role,
    pub depends_on_build: bool,
    pub(super) recipe: Recipe,
    pub(super) stdin: Option<PathBuf>,
    pub(super) test: Option<FrozenTest>,
}

/// Immutable execution input submitted at admission. Persistence can retain it
/// separately from a supervisor's physical resources.
#[derive(Clone, Debug)]
pub struct ExecutionSpec {
    pub run: RunId,
    pub execution: u32,
    pub role: Role,
    pub stage: Stage,
    pub command: LaunchCommand,
    pub resolved: ResolvedStage,
    pub stdin: Option<PathBuf>,
    pub directory: PathBuf,
    pub output_events: bool,
    /// Digest of the independent binary execution description.
    pub digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug)]
pub struct StartFailure {
    pub reason: report::NotStartedReason,
    pub api: Option<&'static str>,
    pub os_error: Option<i32>,
    /// The supervisor has no schema-1 token for this observation. `reason` is
    /// only an adapter placeholder in that case; retain this raw evidence and
    /// report summary_underivable rather than an ordinary unsupported outcome.
    pub invalid_limit: Option<crate::supervisor::LimitValidation>,
}

#[derive(Clone, Debug)]
pub enum ExecutionRecord {
    NotStarted(StartFailure),
    Started(Box<RootOutcome>),
}
impl ExecutionRecord {
    pub(super) fn input(
        &self,
        consumed: bool,
        policies: [crate::supervisor::Exceed; 2],
    ) -> report::RunInput {
        match self {
            Self::NotStarted(reason) => report::RunInput::not_started(reason.reason),
            Self::Started(root) => report::RunInput::from_root(root, consumed, policies),
        }
    }
    pub fn stdout(&self) -> Option<&StreamCapture> {
        match self {
            Self::Started(root) => root.stdout.as_ref(),
            Self::NotStarted(_) => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SummaryEvidence {
    pub capture_files: [Option<PathBuf>; 2],
    pub not_started_timing: Option<(Duration, &'static str)>,
}

#[derive(Clone, Debug)]
pub struct RunSummary {
    pub observation: SummaryEvidence,
    pub class: report::RunClass,
    /// Final supervisor evidence includes timing, streams, limits and measurements.
    pub record: Arc<ExecutionRecord>,
    pub deadline: Duration,
    pub notes: Vec<report::RunNote>,
    pub cwd_overflow_bytes: Option<u64>,
}

/// No path is named before publication is observed. The in-memory service
/// returns `WorkerUnavailable`; the disk-backed service supplies the later variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistError {
    Timeout,
    QueueFull,
    WorkerUnavailable,
    Io { os_error: Option<i32> },
}
#[derive(Clone, Debug)]
pub struct ArtifactRef {
    pub digest: Option<[u8; 32]>,
    pub path: Option<PathBuf>,
    pub error: Option<PersistError>,
}
impl ArtifactRef {
    pub(super) fn memory() -> Self {
        Self {
            digest: None,
            path: None,
            error: Some(PersistError::WorkerUnavailable),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExecutionReport {
    pub plan: PlannedExecution,
    pub run: Option<RunId>,
    pub program: Option<PathBuf>,
    pub spec: Option<ArtifactRef>,
    pub record: Option<ArtifactRef>,
    pub summary: Option<Arc<RunSummary>>,
    /// The derivation evidence preserves class even when summary construction
    /// fails. A serializer emits class only when summary is absent.
    pub result: ExecutionResult,
    pub evidence: Option<Arc<ExecutionRecord>>,
}

#[derive(Clone, Debug)]
pub struct TestReport {
    pub frozen: FrozenTest,
    pub first_execution: u32,
    pub execution_count: u32,
    pub class: Option<report::RunClass>,
    pub check: Option<report::CheckOutcome>,
    pub verdict: Option<report::TestVerdict>,
}

#[derive(Clone, Debug)]
pub struct BuildReport {
    pub execution: u32,
    pub build_id: Option<BuildId>,
    pub output_root: Option<PathBuf>,
    pub result: BuildResult,
    pub artifacts: Vec<OsString>,
}

#[derive(Clone, Debug)]
pub struct Aggregate {
    pub test: Option<String>,
    pub iterations: u32,
    pub succeeded: u32,
    pub wall: Option<[Duration; 3]>,
}

#[derive(Clone, Debug)]
pub struct ReportResult {
    pub verdict: JobVerdict,
    pub primary_verdict: JobVerdict,
    pub partial: bool,
    pub cancellation: Option<Cancellation>,
    pub internal: Option<InternalFailure>,
    pub counts: Counts,
}

#[derive(Clone, Debug)]
pub enum Warning {
    LateCancellation,
    Prune {
        job: Option<JobId>,
        detail: &'static str,
    },
    Recovery {
        paths: u32,
        first: Option<PathBuf>,
    },
    Durability {
        artefact: &'static str,
        detail: &'static str,
    },
    CleanupUnverified {
        runs: u32,
    },
    Persistence {
        artefact: &'static str,
        run: RunId,
        error: PersistError,
    },
}
#[derive(Clone, Debug)]
pub struct JobIdentity {
    pub id: JobId,
    pub dir: PathBuf,
    pub sequence: (Epoch, u64),
    pub kind: JobKind,
}
#[derive(Clone, Debug)]
pub struct CaptureReport {
    pub check: CaptureCheck,
    pub sources: Vec<PathBuf>,
    pub support_files: Vec<PathBuf>,
    pub sources_dropped: u32,
    pub support_files_dropped: u32,
    pub counts: [u32; 2],
    pub bytes: u64,
    pub lists_sha256: [u8; 32],
    /// Present only after no-clobber publication of the full manifest.
    pub manifest: Option<PathBuf>,
}
impl CaptureReport {
    pub(crate) fn from_sources(check: CaptureCheck, files: &[CapturedSourceCopy]) -> Self {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        let mut lists = [Vec::new(), Vec::new()];
        let mut counts = [0_u32; 2];
        for (index, role) in [SourceRole::Source, SourceRole::Support]
            .into_iter()
            .enumerate()
        {
            let selected: Vec<_> = files.iter().filter(|file| file.role == role).collect();
            counts[index] = selected.len() as u32;
            hash.update((selected.len() as u64).to_le_bytes());
            for file in selected {
                let name = file.problem_path.components().join("/");
                hash.update((name.len() as u64).to_le_bytes());
                hash.update(name.as_bytes());
                if lists[index].len() < 16 {
                    lists[index].push(file.problem_path.components().iter().collect::<PathBuf>());
                }
            }
        }
        let [sources, support_files] = lists;
        Self {
            check,
            sources,
            support_files,
            sources_dropped: counts[0].saturating_sub(16),
            support_files_dropped: counts[1].saturating_sub(16),
            counts,
            bytes: files.iter().map(|file| file.len).sum(),
            lists_sha256: hash.finalize().into(),
            manifest: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Timing {
    pub started_at: SystemTime,
    pub finished_at: SystemTime,
    pub duration: Duration,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventSummary {
    pub dropped: u64,
}

#[derive(Clone, Debug)]
pub struct ReportData {
    pub details: super::ReportDetails,
    pub schema: u32,
    pub document: &'static str,
    pub platform: &'static str,
    pub outcome: &'static str,
    pub cpf_version: &'static str,
    pub command: JobKind,
    pub job: JobIdentity,
    pub workspace: PathBuf,
    pub problem: ProblemBinding,
    pub target: Option<TargetPlan>,
    pub comparator: Option<Comparator>,
    pub capture: Option<CaptureReport>,
    pub build: Option<BuildReport>,
    pub executions: Vec<ExecutionReport>,
    pub tests: Vec<TestReport>,
    pub aggregates: Vec<Aggregate>,
    pub result: ReportResult,
    pub warnings: Vec<Warning>,
    pub events: EventSummary,
    pub timing: Timing,
}

/// Frozen at F. There is deliberately no mutable accessor, including when its
/// Arc has a single owner. The lease lives as long as this report or any run.
#[derive(Debug)]
pub struct FinalizedReport {
    pub(super) data: Arc<ReportData>,
    pub(super) _owner: JobOwner,
    pub(super) persistence: super::ReportPersistence,
    pub(super) post_freeze_internal: bool,
}
impl FinalizedReport {
    pub fn post_freeze_internal(&self) -> bool {
        self.post_freeze_internal
    }
    pub fn persistence(&self) -> &super::ReportPersistence {
        &self.persistence
    }
    pub fn data(&self) -> &ReportData {
        &self.data
    }
}

#[cfg(test)]
pub(crate) fn wire_fixture_plan(index: u32, role: Role) -> PlannedExecution {
    PlannedExecution {
        index,
        stage: Stage::Run,
        role,
        depends_on_build: false,
        recipe: Recipe::TargetRun,
        stdin: None,
        test: None,
    }
}

#[cfg(test)]
pub(crate) fn wire_fixture_run(index: u32) -> RunId {
    RunId(format!("r{index:05}"))
}
