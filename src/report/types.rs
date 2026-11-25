//! Evidence adapters and result values. Token methods implement report §3.4.

use std::ffi::OsString;
use std::time::Duration;

use super::OutputRejectReason;
use crate::compare::Diag;
use crate::supervisor::{
    self, CaptureLoss, Cleanup, Exceed, RootOutcome, StopTrigger, Termination,
};

macro_rules! tokens {
    ($(#[$meta:meta])* pub enum $name:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const fn wire_token(self) -> &'static str {
                match self { $(Self::$variant => $token),+ }
            }
        }
        impl TryFrom<&str> for $name {
            type Error = ();
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                match value { $($token => Ok(Self::$variant),)+ _ => Err(()) }
            }
        }
    };
}

tokens! {
    /// Job populations follow report §§1 and 2.10.
    pub enum JobKind { Build => "build", Test => "test", Run => "run", Cmd => "cmd", Hook => "hook" }
}
tokens! {
    /// Contract reasons, including reasons not yet provided by the supervisor.
    pub enum NotStartedReason {
        CancelledBeforeStart => "cancelled_before_start", CreateFailed => "create_failed",
        Limit => "limit", UnsupportedLimit => "unsupported_limit", Unsupported => "unsupported",
        ContainmentUnavailable => "containment_unavailable", Setup => "setup",
        CleanupCapacity => "cleanup_capacity", Worker => "worker", RetentionBudget => "retention_budget",
        SpecTooLarge => "spec_too_large"
    }
}
impl TryFrom<supervisor::session::StartFailureKind> for NotStartedReason {
    type Error = Invariant;
    fn try_from(value: supervisor::session::StartFailureKind) -> Result<Self, Self::Error> {
        use supervisor::session::StartFailureKind as S;
        Ok(match value {
            S::Unsupported => Self::Unsupported,
            S::CancelledBeforeStart => Self::CancelledBeforeStart,
            S::Worker => Self::Worker,
            S::CleanupCapacity => Self::CleanupCapacity,
            S::UnsupportedLimit => Self::UnsupportedLimit,
            S::InvalidLimit => return Err(Invariant::SummaryUnderivable),
            S::Limit => Self::Limit,
            S::CreateFailed => Self::CreateFailed,
            S::Setup => Self::Setup,
        })
    }
}
tokens! {
    pub enum SkipReason {
        BuildNotSucceeded => "build_not_succeeded", AdmissionClosed => "admission_closed",
        SubmitNotSucceeded => "submit_not_succeeded", InputNotValidated => "input_not_validated",
        NotEligible => "not_eligible", StopPolicy => "stop_policy", Cancelled => "cancelled", JobNotEligible => "job_not_eligible"
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    Transferred,
    Started,
    NotStarted { reason: NotStartedReason },
    Skipped { reason: SkipReason },
    Admitted,
}
impl Coverage {
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Transferred => "transferred",
            Self::NotStarted { .. } => "not_started",
            Self::Skipped { .. } => "skipped",
            Self::Admitted => "admitted",
        }
    }
    pub const fn is_cancelled(self) -> bool {
        matches!(
            self,
            Self::Transferred
                | Self::Skipped {
                    reason: SkipReason::Cancelled
                }
                | Self::NotStarted {
                    reason: NotStartedReason::CancelledBeforeStart
                }
        )
    }
}
tokens! {
    pub enum Stream { Stdout => "stdout", Stderr => "stderr", OutputFile => "output_file" }
}
impl From<supervisor::Stream> for Stream {
    fn from(value: supervisor::Stream) -> Self {
        match value {
            supervisor::Stream::Stdout => Self::Stdout,
            supervisor::Stream::Stderr => Self::Stderr,
        }
    }
}
tokens! {
    /// Declaration order is the normative infrastructure sub-kind order.
    pub enum SupervisorErrorKind {
        Register => "register", Wait => "wait", PipeRead => "pipe_read", Kill => "kill",
        Reap => "reap", ReapTimeout => "reap_timeout", Accounting => "accounting", Worker => "worker"
    }
}
impl From<supervisor::SupervisorError> for SupervisorErrorKind {
    fn from(value: supervisor::SupervisorError) -> Self {
        match value {
            supervisor::SupervisorError::Register(_) => Self::Register,
            supervisor::SupervisorError::Wait(_) => Self::Wait,
            supervisor::SupervisorError::Kill(_) => Self::Kill,
            supervisor::SupervisorError::Reap => Self::Reap,
            supervisor::SupervisorError::ReapTimeout => Self::ReapTimeout,
        }
    }
}
tokens! {
    pub enum LossKind {
        WriteError => "write_error", WriteStalled => "write_stalled", PipeRead => "pipe_read",
        NoEofBeforeDrainDeadline => "no_eof_before_drain_deadline", ForwardIncomplete => "forward_incomplete"
    }
}
/// Structured causes contain no unbounded diagnostic text. Declaration order
/// provides the contract's stable sort key; OS error numbers do not affect it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum InfrastructureCause {
    NotStarted { reason: NotStartedReason },
    StatusUnknown,
    CleanupIncomplete { survivors: u32 },
    SupervisorError { error: SupervisorErrorKind },
    CaptureLoss { stream: Stream, loss: LossKind },
}
impl InfrastructureCause {
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::NotStarted { .. } => "not_started",
            Self::StatusUnknown => "status_unknown",
            Self::CleanupIncomplete { .. } => "cleanup_incomplete",
            Self::SupervisorError { .. } => "supervisor_error",
            Self::CaptureLoss { .. } => "capture_loss",
        }
    }
}
/// Only the first eight distinct causes are retained. Construct through `new`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfrastructureCauses {
    causes: Vec<InfrastructureCause>,
    pub causes_dropped: u32,
}
impl InfrastructureCauses {
    pub fn new(causes: impl IntoIterator<Item = InfrastructureCause>) -> Self {
        let mut causes: Vec<_> = causes.into_iter().collect();
        causes.sort_unstable();
        causes.dedup_by_key(|cause| match *cause {
            InfrastructureCause::CleanupIncomplete { .. } => {
                InfrastructureCause::CleanupIncomplete { survivors: 0 }
            }
            other => other,
        });
        let causes_dropped = causes.len().saturating_sub(8).min(u32::MAX as usize) as u32;
        causes.truncate(8);
        Self {
            causes,
            causes_dropped,
        }
    }
    pub fn as_slice(&self) -> &[InfrastructureCause] {
        &self.causes
    }
    pub fn primary(&self) -> Option<&InfrastructureCause> {
        self.causes.first()
    }
}
/// Ordered stream set. An empty set is an orphan when used in a run class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetStreams {
    pub stdout: bool,
    pub stderr: bool,
    /// Only test classes may include this stream; run classes retain stdout/stderr.
    pub output_file: bool,
}
impl BudgetStreams {
    pub fn primary(self) -> Option<Stream> {
        if self.stdout {
            Some(Stream::Stdout)
        } else if self.stderr {
            Some(Stream::Stderr)
        } else if self.output_file {
            Some(Stream::OutputFile)
        } else {
            None
        }
    }
    pub fn streams(self) -> impl Iterator<Item = Stream> {
        [
            self.stdout.then_some(Stream::Stdout),
            self.stderr.then_some(Stream::Stderr),
            self.output_file.then_some(Stream::OutputFile),
        ]
        .into_iter()
        .flatten()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunClass {
    Infrastructure { causes: InfrastructureCauses },
    Cancelled,
    WallDeadline,
    OutputBudget { streams: BudgetStreams },
    Signal { signal: i32, core_dumped: bool },
    NonzeroExit { code: i64 },
    ExitedZero,
}
impl RunClass {
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::Infrastructure { .. } => "infrastructure",
            Self::Cancelled => "cancelled",
            Self::WallDeadline => "wall_deadline",
            Self::OutputBudget { .. } => "output_budget",
            Self::Signal { .. } => "signal",
            Self::NonzeroExit { .. } => "nonzero_exit",
            Self::ExitedZero => "exited_zero",
        }
    }
    pub const fn diagnostic(&self) -> bool {
        matches!(
            self,
            Self::WallDeadline
                | Self::OutputBudget { .. }
                | Self::Signal { .. }
                | Self::NonzeroExit { .. }
        )
    }
}
/// Capture policies live in the spec, not `RootOutcome`; supply the frozen
/// policy here. Consumed means test stdout, never stderr in schema 1.
#[derive(Clone, Debug, Default)]
pub struct StreamEvidence {
    pub exceeded: bool,
    pub losses: Vec<CaptureLoss>,
    pub kill_on_breach: bool,
}
impl StreamEvidence {
    fn from_capture(capture: Option<&supervisor::StreamCapture>, policy: Exceed) -> Self {
        Self {
            exceeded: capture.is_some_and(|capture| capture.exceeded_at.is_some()),
            losses: capture.map_or_else(Vec::new, |capture| capture.losses.clone()),
            kill_on_breach: policy == Exceed::Kill,
        }
    }
    pub fn breached(&self) -> bool {
        self.exceeded || self.losses.contains(&CaptureLoss::BudgetDiscard)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionEvidence {
    Transferred,
    NotStarted(NotStartedReason),
    Started(Termination),
}
impl ExecutionEvidence {
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::Transferred => "transferred",
            Self::NotStarted(_) => "not_started",
            Self::Started(_) => "started",
        }
    }
}
/// Minimal policy adapter for finalized supervisor evidence. It can also name
/// contract facts unavailable in the current supervisor enums.
#[derive(Clone, Debug)]
pub struct RunInput {
    pub execution: ExecutionEvidence,
    pub cleanup: Cleanup,
    pub stop_trigger: Option<StopTrigger>,
    pub wall_violation: bool,
    pub budget_violations: BudgetStreams,
    pub supervisor_errors: Vec<SupervisorErrorKind>,
    pub stdout: StreamEvidence,
    pub stderr: StreamEvidence,
    pub consumes_stdout: bool,
    /// Storage observation, absent from the current supervisor record.
    pub cwd_overflow_bytes: Option<u64>,
}
impl RunInput {
    pub fn from_root(root: &RootOutcome, consumes_stdout: bool, policies: [Exceed; 2]) -> Self {
        Self {
            execution: ExecutionEvidence::Started(root.status),
            cleanup: root.termination.cleanup,
            stop_trigger: root.stop_trigger,
            wall_violation: root
                .violations
                .iter()
                .any(|v| matches!(v, supervisor::Violation::WallDeadline { .. })),
            budget_violations: BudgetStreams {
                stdout: root.violations.iter().any(|v| {
                    matches!(
                        v,
                        supervisor::Violation::OutputBudget {
                            stream: supervisor::Stream::Stdout,
                            ..
                        }
                    )
                }),
                stderr: root.violations.iter().any(|v| {
                    matches!(
                        v,
                        supervisor::Violation::OutputBudget {
                            stream: supervisor::Stream::Stderr,
                            ..
                        }
                    )
                }),
                output_file: false,
            },
            supervisor_errors: root.errors.iter().copied().map(Into::into).collect(),
            stdout: StreamEvidence::from_capture(root.stdout.as_ref(), policies[0]),
            stderr: StreamEvidence::from_capture(root.stderr.as_ref(), policies[1]),
            consumes_stdout,
            cwd_overflow_bytes: None,
        }
    }
    pub fn from_record(
        record: &supervisor::session::RunRecord,
        consumes_stdout: bool,
        policies: [Exceed; 2],
    ) -> Result<Self, Invariant> {
        Ok(match record {
            supervisor::session::RunRecord::Started(root) => {
                Self::from_root(root, consumes_stdout, policies)
            }
            supervisor::session::RunRecord::Transferred => Self::transferred(),
            supervisor::session::RunRecord::NotStarted(reason) => {
                Self::not_started(reason.kind.try_into()?)
            }
        })
    }
    pub fn transferred() -> Self {
        Self {
            execution: ExecutionEvidence::Transferred,
            cleanup: Cleanup::Unverified {
                reason: supervisor::UnverifiedReason::RootNotObserved,
            },
            stop_trigger: Some(StopTrigger::Cancelled),
            ..Self::not_started(NotStartedReason::CancelledBeforeStart)
        }
    }
    pub fn not_started(reason: NotStartedReason) -> Self {
        Self {
            execution: ExecutionEvidence::NotStarted(reason),
            cleanup: Cleanup::Confirmed,
            stop_trigger: None,
            wall_violation: false,
            budget_violations: BudgetStreams {
                stdout: false,
                stderr: false,
                output_file: false,
            },
            supervisor_errors: Vec::new(),
            stdout: StreamEvidence::default(),
            stderr: StreamEvidence::default(),
            consumes_stdout: false,
            cwd_overflow_bytes: None,
        }
    }
}
tokens! {
    /// Invariant tokens for missing evidence and orphan combinations. The
    /// contract fixes the first two spellings; orphan names describe the defect.
    pub enum Invariant {
        RecordMissing => "record_missing", SummaryUnderivable => "summary_underivable",
        CoverageClass => "coverage_class", SkipCondition => "skip_condition",
        TestCoverage => "test_coverage", TestCheck => "test_check", DiagnosticFlag => "diagnostic_flag",
        IncompleteZeroExit => "incomplete_zero_exit", BuildCoverage => "build_coverage",
        BuildEvidence => "build_evidence", CheckEvidence => "check_evidence",
        JobEvidence => "job_evidence"
    }
}
tokens! {
    /// Policy notes supplementing, without replacing, the supervisor's notes.
    pub enum RunNote {
        CleanupUnverified => "cleanup_unverified", CwdOverflow => "cwd_overflow",
        CwdUnmeasuredTimeout => "cwd_unmeasured_timeout",
        CwdUnmeasuredIo => "cwd_unmeasured_io",
        CwdUnmeasuredCancelled => "cwd_unmeasured_cancelled",
        TruncatedStdout => "truncated_stdout", TruncatedStderr => "truncated_stderr"
    }
}
/// Observations remain unchanged even when an invariant is attached. A class
/// retained before summary construction failed is still counted by its class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionResult {
    pub coverage: Coverage,
    pub class: Option<RunClass>,
    pub invariant: Option<Invariant>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildCause {
    Run(RunClass),
    MissingOutput {
        names: Vec<OsString>,
        names_dropped: u32,
    },
    GeneratedProgramInvalid {
        name: OsString,
        reason: String,
    },
    OutputTooLarge {
        bytes: u64,
        cap: u64,
    },
}
impl BuildCause {
    pub fn wire_token(&self) -> &'static str {
        match self {
            Self::Run(class) => class.wire_token(),
            Self::MissingOutput { .. } => "missing_output",
            Self::GeneratedProgramInvalid { .. } => "generated_program_invalid",
            Self::OutputTooLarge { .. } => "output_too_large",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildResult {
    Succeeded,
    Cancelled,
    Infrastructure { causes: InfrastructureCauses },
    Failed { cause: BuildCause },
    InternalError { invariant: Invariant },
}
impl BuildResult {
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Cancelled => "cancelled",
            Self::Infrastructure { .. } => "infrastructure",
            Self::Failed { .. } => "failed",
            Self::InternalError { .. } => "internal_error",
        }
    }
}
/// Independent post-build observations, evaluated in contract order. Storage
/// inspection/measurement errors have no ordinary result in report §2.3; callers
/// must retain them as an invariant rather than inventing a missing output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PostBuild {
    pub missing_outputs: Vec<OsString>,
    pub generated_program_invalid: Option<(OsString, String)>,
    pub output_too_large: Option<(u64, u64)>,
    pub invariant: Option<Invariant>,
}
tokens! {
    pub enum CheckNotAdmitted { RunNotEligible => "run_not_eligible", Cancelled => "cancelled" }
}
tokens! {
    pub enum ComparisonReason {
        TokenMismatch => "token_mismatch", LineMismatch => "line_mismatch", ByteMismatch => "byte_mismatch",
        CountMismatch => "count_mismatch", ClassMismatch => "class_mismatch", IntegerMismatch => "integer_mismatch",
        Tolerance => "tolerance", UlpExcess => "ulp_excess", OutOfRange => "out_of_range",
        IncompleteOutput => "incomplete_output", InvalidAnswer => "invalid_answer"
    }
}
impl From<&Diag> for ComparisonReason {
    fn from(diag: &Diag) -> Self {
        match diag {
            Diag::TokenMismatch { .. } => Self::TokenMismatch,
            Diag::LineMismatch { .. } => Self::LineMismatch,
            Diag::ByteMismatch { .. } => Self::ByteMismatch,
            Diag::CountMismatch { .. } => Self::CountMismatch,
            Diag::ClassMismatch { .. } => Self::ClassMismatch,
            Diag::IntegerMismatch { .. } => Self::IntegerMismatch,
            Diag::Tolerance { .. } => Self::Tolerance,
            Diag::UlpExcess { .. } => Self::UlpExcess,
            Diag::OutOfRange { .. } => Self::OutOfRange,
            Diag::IncompleteOutput => Self::IncompleteOutput,
            Diag::InvalidAnswer { .. } => Self::InvalidAnswer,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Judged { outcome: Box<super::JudgedCheck> },
    NotAdmitted { reason: CheckNotAdmitted },
    Unchecked,
    Passed,
    Failed { diag: Box<Diag> },
    CheckerFailed { diag: Box<Diag> },
    CheckError { os_error: Option<i32> },
    OutputRejected { reason: OutputRejectReason },
    InternalError { invariant: Invariant },
}
impl CheckKind {
    pub fn wire_token(&self) -> &'static str {
        match self {
            Self::Judged { outcome } => match outcome.as_ref() {
                super::JudgedCheck::Rejected { .. } => "rejected",
                super::JudgedCheck::Scored { .. } => "scored",
                super::JudgedCheck::JudgeFailed { .. } => "judge_failed",
                super::JudgedCheck::Unchecked { .. } => "unchecked",
            },
            Self::NotAdmitted { .. } => "not_admitted",
            Self::Unchecked => "unchecked",
            Self::Passed => "passed",
            Self::Failed { .. } => "failed",
            Self::CheckerFailed { .. } => "checker_failed",
            Self::CheckError { .. } => "check_error",
            Self::OutputRejected { .. } => "output_rejected",
            Self::InternalError { .. } => "internal_error",
        }
    }
    /// Fixed schema fields on comparison outcomes, separate from `kind`.
    pub fn reason_token(&self) -> Option<&'static str> {
        match self {
            Self::NotAdmitted { reason } => Some(reason.wire_token()),
            Self::Unchecked => Some("no_answer"),
            Self::OutputRejected { reason } => Some(reason.wire_token()),
            Self::Failed { diag } => Some(ComparisonReason::from(diag.as_ref()).wire_token()),
            Self::CheckerFailed { .. } => Some("invalid_answer"),
            _ => None,
        }
    }
    pub const fn fail_token(&self) -> Option<&'static str> {
        if matches!(self, Self::Failed { .. }) {
            Some("wrong_answer")
        } else {
            None
        }
    }
    pub const fn class_token(&self) -> Option<&'static str> {
        if matches!(self, Self::CheckerFailed { .. }) {
            Some("test_data_error")
        } else {
            None
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckOutcome {
    pub kind: CheckKind,
    pub diagnostic: bool,
}
impl CheckOutcome {
    pub fn wire_token(&self) -> &'static str {
        self.kind.wire_token()
    }
}
tokens! {
    /// Snapshot under the verdict lock; later intent cannot revoke admission.
    pub enum CheckAdmission { NoIntent => "no_intent", IntentBeforeAdmission => "intent_before_admission", IntentAfterAdmission => "intent_after_admission" }
}
/// Reading/comparison happens outside this module. `NotRun` is valid for an
/// absent answer or an ineligible check; an eligible present answer needs data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Comparison {
    NotRun,
    Outcome(crate::compare::CheckOutcome),
    IoError { os_error: Option<i32> },
    InternalError { invariant: Invariant },
}
impl Comparison {
    pub fn wire_token(&self) -> &'static str {
        match self {
            Self::NotRun => "not_run",
            Self::Outcome(outcome) => match outcome {
                crate::compare::CheckOutcome::Unchecked => "unchecked",
                crate::compare::CheckOutcome::Passed => "passed",
                crate::compare::CheckOutcome::Failed(_) => "failed",
                crate::compare::CheckOutcome::CheckerFailed(_) => "checker_failed",
            },
            Self::IoError { .. } => "check_error",
            Self::InternalError { .. } => "internal_error",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestVerdict {
    PresentationError,
    Scored,
    SessionLimit,
    Idle,
    Passed,
    WrongAnswer,
    Unchecked,
    CheckerFailed,
    WallDeadline,
    OutputBudget,
    Signal,
    NonzeroExit,
    Cancelled,
    Infrastructure,
    NotRun { reason: SkipReason },
    InternalError { invariant: Invariant },
}
impl TestVerdict {
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::PresentationError => "presentation_error",
            Self::Scored => "scored",
            Self::SessionLimit => "session_limit",
            Self::Idle => "idle",
            Self::Passed => "passed",
            Self::WrongAnswer => "wrong_answer",
            Self::Unchecked => "unchecked",
            Self::CheckerFailed => "checker_failed",
            Self::WallDeadline => "wall_deadline",
            Self::OutputBudget => "output_budget",
            Self::Signal => "signal",
            Self::NonzeroExit => "nonzero_exit",
            Self::Cancelled => "cancelled",
            Self::Infrastructure => "infrastructure",
            Self::NotRun { .. } => "not_run",
            Self::InternalError { .. } => "internal_error",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookOutcome {
    Succeeded,
    Failed { class: RunClass },
    Cancelled,
    Skipped { reason: SkipReason },
    InternalError { invariant: Invariant },
}
impl HookOutcome {
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
            Self::Skipped { .. } => "skipped",
            Self::InternalError { .. } => "internal_error",
        }
    }
}
/// `cmd` and raw `run` keep coverage and class as their outcome (§2.6).
pub type CommandOutcome = ExecutionResult;

tokens! {
    pub enum JobVerdict {
        InternalError => "internal_error", Infrastructure => "infrastructure", Cancelled => "cancelled",
        BuildFailed => "build_failed", WallDeadline => "wall_deadline", OutputBudget => "output_budget", Signal => "signal",
        NonzeroExit => "nonzero_exit", CheckerFailed => "checker_failed", WrongAnswer => "wrong_answer",
        PresentationError => "presentation_error", Scored => "scored",
        SessionLimit => "session_limit", Idle => "idle", Unchecked => "unchecked", NoTests => "no_tests", Passed => "passed", Succeeded => "succeeded"
    }
}
/// The caller supplies the first recorded internal failure, preserving its locus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InternalLocus {
    Job,
    Build,
    Execution { index: u32 },
    Test { test: String },
    Phase { phase: &'static str },
}
impl InternalLocus {
    pub const fn wire_token(&self) -> &'static str {
        match self {
            Self::Job => "job",
            Self::Build => "build",
            Self::Execution { .. } => "execution",
            Self::Test { .. } => "test",
            Self::Phase { .. } => "phase",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalFailure {
    pub invariant: Invariant,
    pub locus: InternalLocus,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobResult {
    pub primary_verdict: JobVerdict,
    pub verdict: JobVerdict,
    pub internal: Option<InternalFailure>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cov {
    pub planned: u32,
    pub started: u32,
    pub not_started: u32,
    pub transferred: u32,
    pub skipped: u32,
    pub admitted: u32,
    pub invalid: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByClass {
    pub exited_zero: u32,
    pub nonzero_exit: u32,
    pub signal: u32,
    pub wall_deadline: u32,
    pub output_budget: u32,
    pub cancelled: u32,
    pub infrastructure: u32,
    pub skipped: u32,
    pub unknown: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByVerdict {
    pub presentation_error: u32,
    pub scored: u32,
    pub session_limit: u32,
    pub idle: u32,
    pub passed: u32,
    pub wrong_answer: u32,
    pub unchecked: u32,
    pub checker_failed: u32,
    pub wall_deadline: u32,
    pub output_budget: u32,
    pub signal: u32,
    pub nonzero_exit: u32,
    pub cancelled: u32,
    pub infrastructure: u32,
    pub not_run: u32,
    pub internal_error: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub primary: Cov,
    pub hooks: Cov,
    pub tests: u32,
    pub by_verdict: ByVerdict,
    pub by_class: ByClass,
    pub partial: bool,
}

/// Durations are explicit values; no clock is read by display derivation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayContext {
    pub comparator: Option<&'static str>,
    pub cleanup_unverified: bool,
    pub wall_deadline: Option<Duration>,
    pub measured_wall: Option<Duration>,
    pub budgets: Vec<(Stream, u64)>,
    pub signal_name: Option<String>,
    pub ntstatus: Option<String>,
    pub installed_limits: Vec<supervisor::LocalLimit>,
    pub invariant: Option<Invariant>,
    pub measurements: Vec<supervisor::Measurement>,
    pub output_file: Option<(String, super::OutputFile)>,
}

impl PostBuild {
    /// Adapt finalized observations without inspecting the removed output tree
    /// or inferring independent facts from the primary failure cause. Producer
    /// classification uses the run record, separately from these observations.
    pub fn from_validation(
        result: Result<&crate::build::ValidatedBuild, &crate::build::BuildFailure>,
    ) -> Self {
        match result {
            Ok(_) => Self::default(),
            Err(failure) => (*failure.post_build).clone(),
        }
    }

    /// Diagnostics for a single validation failure. Finalization collects all
    /// declared names separately, so a failed ancestor is never used as a name.
    pub(crate) fn from_failure_kind(
        kind: &crate::build::BuildFailureKind,
        output_root: &std::path::Path,
    ) -> Self {
        use crate::build::BuildFailureKind as F;
        let mut post = Self::default();
        match kind {
            F::OutputMissing(path) | F::OutputNotRegular(path) | F::OutputThroughLink(path) => {
                match path.strip_prefix(output_root) {
                    Ok(name) if !name.as_os_str().is_empty() => {
                        post.missing_outputs.push(name.as_os_str().to_owned())
                    }
                    _ => post.invariant = Some(Invariant::BuildEvidence),
                }
            }
            F::GeneratedProgramNotLaunchable(errors) => {
                if let Some(error) = errors.errors.first() {
                    let name = match &error.request {
                        crate::exec::ProgramRequest::Path(path) => path.as_os_str().to_owned(),
                        crate::exec::ProgramRequest::Lookup(name) => name.clone(),
                    };
                    post.generated_program_invalid = Some((name, errors.to_string()));
                } else {
                    post.invariant = Some(Invariant::BuildEvidence);
                }
            }
            // These have independent observations, populated by finalization.
            F::BuildOutputTooLarge | F::ProducerFailed(_) => {}
            F::OutputInspection(_) | F::Measurement(_) | F::IncompleteMeasurement(_) => {
                post.invariant = Some(Invariant::BuildEvidence)
            }
        }
        post
    }
}
