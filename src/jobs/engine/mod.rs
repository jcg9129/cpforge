//! Ready-job execution and immutable in-memory reporting.
//!
//! [`Engine::new`] consumes the ready capability and fixes an ordered plan,
//! exposed by [`Engine::plan`] (plan index, stage, role, build dependency). A
//! test request uses only the ready job's frozen manifest. Iterations use the
//! validated target run or an already resolved stage; command iterations with
//! tests must form contiguous ranges in frozen-manifest order.
//!
//! Before [`Engine::run`], clone [`Engine::cancellation`] and optionally call
//! `events().subscribe()`; enable output chunks with
//! [`Engine::output_events`]. `run` takes an [`Executor`], a [`Persistence`]
//! service and [`PhaseHooks`], and returns an `Arc<FinalizedReport>` whose
//! data is reachable only through [`FinalizedReport::data`]. One creation
//! thread performs blocking run setup for the executor; cancellation may arrive
//! from any thread. The verdict lock covers admission, check arbitration,
//! per-test derivation, V and F, and is never held across execution, comparison
//! I/O, callbacks or settlement. Cancellation records intent only; it never
//! assigns a verdict.
//!
//! Event consumers drain [`EventReceiver::recv`] until it returns `None`, then
//! await [`EventReceiver::terminal`]. Sequence numbers start at zero and may
//! have gaps where events were dropped; closure, not `seq`, is the barrier.
//!
//! `JobPersistence` in `jobs::persist` supplies the disk-backed service;
//! construct it and start `SupervisorExecutor` before passing the ready job to
//! this engine.
//! `SupervisorExecutor` supports Linux and macOS. An executor must keep the
//! [`RunResources`] guard with the physical resources of the run (writers,
//! the root) until they are released, and file sinks add every successful
//! write to [`RunResources::retained_counter`]. Persistence owns the metadata
//! reservation of each queued artefact until its last write returns.
//!
//! [`PhaseHooks::fact`] receives the frozen primary verdict and a cancellation
//! cutoff: the first call reads intent under the verdict lock and later calls
//! return the same answer. [`PhaseHooks::hooks`] receives a start check that
//! reads the lock on every call; call it immediately before each hook start.
//! [`MemoryPersistence`] performs no writes and settles every reference as
//! unavailable, so the terminal event carries no report id. After F, S publishes
//! the immutable report once; `FinalizedReport::persistence` retains that result
//! separately from the frozen value and the terminal slot is then filled.

mod cancel;
mod cwd;
mod events;
mod executor;
mod judge_files;
mod judging;
mod persistence;
mod report_value;
mod role_builds;
mod spec;
pub mod stress;
mod testlib;
mod types;
pub(crate) mod warnings;

pub use cancel::*;
pub use events::*;
pub use executor::*;
pub use persistence::*;
pub use report_value::*;
pub use types::*;

use std::fs;
use std::future::{Future, poll_fn};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, SystemTime};

use crate::build::{self, PlannedTarget, ValidatedBuild};
use crate::config::{CapPolicy, Comparator, Platform};
use crate::jobs::ReadyExecutionJob;
use crate::report::{
    self, BuildResult, CheckAdmission, Comparison, Coverage, ExecutionResult, InternalFailure,
    InternalLocus, Invariant, JobKind, JobVerdict, NotStartedReason, PostBuild, RunClass,
    SkipReason,
};
use crate::storage::budget::{JobBudget, Reservation};
use crate::storage::job::JobOwner;
use crate::tests_store::FrozenAnswer;

const METADATA: u64 = 1024 * 1024;
const MAX_EXECUTIONS: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    TooManyExecutions,
    InvalidIteration,
    BuildRequired,
    ReportSize,
}
impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid execution plan: {self:?}")
    }
}
impl std::error::Error for PlanError {}

/// Callback hooks for A and H. `cutoff` returns whether intent was set at its
/// first invocation; repeats return the same snapshot. `start` must be checked
/// separately immediately before each hook start, and returns a skip reason.
pub trait PhaseHooks {
    /// Phase A computes input evidence even for ineligible test jobs.
    fn fingerprints(&mut self) -> Option<([u8; 32], [u8; 32])> {
        None
    }
    /// Publication evidence is copied before hooks, settlement and closure.
    fn fact_evidence(&self) -> Option<(Fact, Vec<WireWarning>)> {
        None
    }
    fn fact<'a>(
        &'a mut self,
        _primary: JobVerdict,
        _cutoff: &'a mut dyn FnMut() -> bool,
    ) -> JobFuture<'a, ()> {
        Box::pin(async {})
    }
    fn hooks<'a>(
        &'a mut self,
        _primary: JobVerdict,
        _start: &'a dyn Fn() -> Option<SkipReason>,
    ) -> JobFuture<'a, ()> {
        Box::pin(async {})
    }
}
#[derive(Debug, Default)]
pub struct NoHooks;
impl PhaseHooks for NoHooks {}

/// Deterministic, feature-gated observation/failure points. They are outside
/// the verdict lock, allowing a harness to arrange cancellation on either side.
#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeAdmission(u32),
    AfterAdmission(u32),
    #[cfg(feature = "storage-test-hooks")]
    BeforeRunMkdir(u32),
    #[cfg(feature = "storage-test-hooks")]
    BeforeRecipeCompletion(u32),
    #[cfg(feature = "storage-test-hooks")]
    BeforeCwdMeasure(u32),
    #[cfg(feature = "storage-test-hooks")]
    FileIo {
        execution: u32,
        point: &'static str,
        size: usize,
    },
    #[cfg(feature = "storage-test-hooks")]
    BeforeCwdClear(u32),
    BeforeCheck(u32),
    AfterCheckAdmission(u32),
    #[cfg(feature = "storage-test-hooks")]
    BeforeRecordEnqueue(u32),
    BeforeClass(u32),
    BeforeSummary(u32),
    BeforeTestVerdict(u32),
    AfterPrimaryFreeze,
    BeforeHookStart(u32),
    Settlement,
    Closure,
    BeforeSnapshot,
    AfterSnapshot,
}
#[cfg(any(test, feature = "storage-test-hooks"))]
type Observer = dyn Fn(Point) + Send + Sync;

#[derive(Debug)]
pub struct Engine<'probe> {
    ready: ReadyExecutionJob<'probe>,
    plan: Vec<PlannedExecution>,
    after_test: Vec<PlannedExecution>,
    disabled_hooks: Vec<Hook>,
    kind: JobKind,
    comparator: Option<Comparator>,
    report_shape: Option<report::wire::ReportShape>,
    file_io: Option<report::FileIo>,
    keep_going: bool,
    output_events: bool,
    capture_enabled: bool,
    cancel: CancelState,
    events: Events,
    progress: Option<crate::progress::ProgressQueue>,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    observer: Option<Observation>,
}

#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Clone)]
pub(crate) struct Observation(pub(crate) Arc<Observer>);
#[cfg(any(test, feature = "storage-test-hooks"))]
impl std::fmt::Debug for Observation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Observation")
    }
}

impl<'probe> Engine<'probe> {
    pub fn new(
        ready: impl Into<ReadyExecutionJob<'probe>>,
        request: JobRequest,
    ) -> Result<Self, PlanError> {
        let ready = ready.into();
        let cancel = CancelState::at(ready.started);
        Self::with_cancellation(ready, request, cancel)
    }

    /// Use the invocation state installed before registration, preserving any
    /// cancellation received during admission or capture.
    pub fn with_cancellation(
        ready: impl Into<ReadyExecutionJob<'probe>>,
        request: JobRequest,
        mut cancel: CancelState,
    ) -> Result<Self, PlanError> {
        // Report timing runs from admission (step 1) to F, as in `new`; the
        // shared verdict state and token are those installed earlier.
        let ready = ready.into();
        cancel.start = ready.started;
        let mut plan = Vec::new();
        let has_build = matches!(&ready.planned_target, Some(PlannedTarget::Build(_)));
        let (kind, comparator, keep_going, needs_build) = match &request {
            JobRequest::Build => (JobKind::Build, None, true, true),
            JobRequest::Test { comparator } => {
                (JobKind::Test, Some(comparator.clone()), true, true)
            }
            JobRequest::Iterations {
                kind,
                keep_going,
                needs_build,
                ..
            } => (kind.job_kind(), None, *keep_going, *needs_build),
        };
        let primary_count = match &request {
            JobRequest::Build => 0,
            JobRequest::Test { .. } => ready.manifest.entries().len(),
            JobRequest::Iterations { iterations, .. } => iterations.len(),
        };
        if primary_count.saturating_add(usize::from(has_build && needs_build)) > MAX_EXECUTIONS {
            return Err(PlanError::TooManyExecutions);
        }
        if has_build && needs_build {
            plan.push(PlannedExecution {
                index: 0,
                stage: Stage::Build,
                role: Role::Build,
                depends_on_build: false,
                recipe: Recipe::TargetRun,
                stdin: None,
                test: None,
            });
        }
        match request {
            JobRequest::Build => {}
            JobRequest::Test { .. } => {
                for test in ready.manifest.entries() {
                    plan.push(PlannedExecution {
                        index: plan.len() as u32,
                        stage: Stage::Run,
                        role: Role::Test {
                            test: test.id.to_string(),
                        },
                        depends_on_build: has_build,
                        recipe: Recipe::TargetRun,
                        stdin: Some(test.input.path.clone()),
                        test: Some(test.clone()),
                    });
                }
            }
            JobRequest::Iterations {
                kind, iterations, ..
            } => {
                for iteration in iterations {
                    let stage = match (&kind, &iteration.role) {
                        (CommandKind::Cmd, Role::Command { iteration, .. }) if *iteration > 0 => {
                            Stage::Command
                        }
                        (CommandKind::Run, Role::Raw) => Stage::Run,
                        (CommandKind::Hook, Role::Hook { name, trigger })
                            if crate::names::Identifier::new(name.clone()).is_ok()
                                && matches!(trigger.as_str(), "on_switch" | "after_test") =>
                        {
                            Stage::Hook
                        }
                        _ => return Err(PlanError::InvalidIteration),
                    };
                    if has_build
                        && !needs_build
                        && (iteration.depends_on_build
                            || matches!(&iteration.recipe, Recipe::TargetRun))
                    {
                        return Err(PlanError::BuildRequired);
                    }
                    let test = match &iteration.role {
                        Role::Command { test: Some(id), .. } => Some(
                            ready
                                .manifest
                                .entries()
                                .iter()
                                .find(|test| test.id.as_str() == id)
                                .ok_or(PlanError::InvalidIteration)?
                                .clone(),
                        ),
                        _ => None,
                    };
                    plan.push(PlannedExecution {
                        index: plan.len() as u32,
                        stage,
                        role: iteration.role,
                        depends_on_build: has_build
                            && (iteration.depends_on_build
                                || matches!(&iteration.recipe, Recipe::TargetRun)),
                        recipe: iteration.recipe,
                        stdin: iteration.stdin,
                        test,
                    });
                }
            }
        }
        let report_shape = if let Some(judging) = &ready.judging {
            if kind != JobKind::Test {
                return Err(PlanError::InvalidIteration);
            }
            plan = role_builds::plan(judging, &ready.manifest)?;
            Some(judging.plan.shape)
        } else {
            None
        };
        let mut previous_test = None;
        for execution in &plan {
            if let Some(test) = &execution.test {
                let position = ready
                    .manifest
                    .entries()
                    .iter()
                    .position(|entry| entry.id == test.id)
                    .expect("frozen test");
                if previous_test.is_some_and(|previous| position < previous) {
                    return Err(PlanError::InvalidIteration);
                }
                previous_test = Some(position);
            } else if execution.stage != Stage::Build && previous_test.is_some() {
                return Err(PlanError::InvalidIteration);
            }
        }
        if plan.len() > MAX_EXECUTIONS {
            return Err(PlanError::TooManyExecutions);
        }
        let engine = Self {
            ready,
            plan,
            after_test: Vec::new(),
            disabled_hooks: Vec::new(),
            kind,
            comparator,
            report_shape,
            file_io: None,
            keep_going,
            output_events: false,
            capture_enabled: needs_build || kind == JobKind::Run,
            cancel,
            events: Events::new(),
            progress: None,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            observer: None,
        };
        // A judged plan must fit the report reservation held since planning.
        if engine.ready.judging.is_some() {
            engine.validate_report_reservation()?;
        }
        Ok(engine)
    }

    /// Attach already prepared hooks to phase H, sharing this job's resources.
    pub fn after_test(mut self, iterations: Vec<Iteration>) -> Result<Self, PlanError> {
        if self.kind != JobKind::Test {
            return Err(PlanError::InvalidIteration);
        }
        if self
            .plan
            .len()
            .saturating_add(self.after_test.len())
            .saturating_add(iterations.len())
            > MAX_EXECUTIONS
        {
            return Err(PlanError::TooManyExecutions);
        }
        for iteration in iterations {
            if !matches!(&iteration.role, Role::Hook { name, trigger }
                if crate::names::Identifier::new(name.clone()).is_ok() && trigger == "after_test")
                || iteration.depends_on_build
                || iteration.stdin.is_some()
                || !matches!(&iteration.recipe, Recipe::Resolved(_))
            {
                return Err(PlanError::InvalidIteration);
            }
            self.after_test.push(PlannedExecution {
                index: (self.plan.len() + self.after_test.len()) as u32,
                stage: Stage::Hook,
                role: iteration.role,
                recipe: iteration.recipe,
                depends_on_build: false,
                stdin: None,
                test: None,
            });
        }
        self.validate_report_reservation()?;
        Ok(self)
    }

    /// Apply the declaration only to this test job's solution runs.
    /// Without a declaration this changes nothing (other job kinds pass `None`).
    pub fn file_io(mut self, io: Option<report::FileIo>) -> Result<Self, PlanError> {
        let Some(io) = io else {
            return Ok(self);
        };
        if self.kind != JobKind::Test {
            return Err(PlanError::InvalidIteration);
        }
        let mut shape = self.report_shape.unwrap_or_default();
        shape.output_file = io.output.is_some();
        self.report_shape = Some(shape);
        // The post-run cwd/ step removes the I/O scratch, so it always runs.
        self.ready.cwd_bytes.get_or_insert(u64::MAX);
        self.file_io = Some(io);
        self.validate_report_reservation()?;
        Ok(self)
    }

    /// Future judging/I/O planners supply the frozen shape before admission.
    pub fn report_shape(mut self, shape: report::wire::ReportShape) -> Result<Self, PlanError> {
        if self.kind != JobKind::Test {
            return Err(PlanError::InvalidIteration);
        }
        if self
            .ready
            .judging
            .as_ref()
            .is_some_and(|j| j.plan.shape != shape)
        {
            return Err(PlanError::InvalidIteration);
        }
        self.report_shape = Some(shape);
        self.validate_report_reservation()?;
        Ok(self)
    }

    fn validate_report_reservation(&self) -> Result<(), PlanError> {
        let (tests, aggregates) =
            report_populations(self.kind, &self.plan, self.ready.manifest.entries().len());
        let bytes = report::wire::report_reservation(
            self.plan.len() + self.after_test.len(),
            tests,
            aggregates,
            self.report_shape,
        )
        .map_err(|_| PlanError::ReportSize)?;
        if self
            .ready
            .judging
            .as_ref()
            .is_some_and(|j| bytes > j.report_reservation.bytes())
        {
            return Err(PlanError::ReportSize);
        }
        Ok(())
    }

    pub fn disabled_hooks(
        mut self,
        names: Vec<String>,
        trigger: &'static str,
        reason: &'static str,
    ) -> Self {
        self.disabled_hooks = names
            .into_iter()
            .map(|name| Hook {
                name,
                trigger,
                execution: None,
                outcome: HookResult::NotPlanned { reason },
            })
            .collect();
        self
    }

    /// Attach the invocation's stderr queue before running. It is closed at E.
    pub fn progress(mut self, progress: crate::progress::ProgressQueue) -> Self {
        progress.planned_tests(self.ready.manifest.entries().len());
        self.progress = Some(progress);
        self
    }

    pub fn plan(&self) -> &[PlannedExecution] {
        &self.plan
    }
    pub fn cancellation(&self) -> CancelState {
        self.cancel.clone()
    }
    pub fn events(&self) -> Events {
        self.events.clone()
    }
    pub fn output_events(mut self, enabled: bool) -> Self {
        self.output_events = enabled;
        self
    }
    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub fn observe(mut self, observer: impl Fn(Point) + Send + Sync + 'static) -> Self {
        self.observer = Some(Observation(Arc::new(observer)));
        self
    }

    /// Run P, V, A/H, W, E, F and S. Panics in executor futures, derivations,
    /// comparisons and extension callbacks become explicit internal evidence.
    pub async fn run(
        self,
        executor: &mut dyn Executor,
        persistence: &mut dyn Persistence,
        hooks: &mut dyn PhaseHooks,
    ) -> Arc<FinalizedReport> {
        // The job state machine's future is large; keep it on the heap so
        // callers on small thread stacks (test threads, runtime workers) do
        // not overflow.
        Box::pin(self.run_phases(executor, persistence, hooks)).await
    }

    async fn run_phases(
        self,
        executor: &mut dyn Executor,
        persistence: &mut dyn Persistence,
        hooks: &mut dyn PhaseHooks,
    ) -> Arc<FinalizedReport> {
        let Self {
            mut ready,
            plan,
            after_test,
            disabled_hooks,
            kind,
            comparator,
            report_shape,
            file_io,
            keep_going,
            output_events,
            capture_enabled,
            cancel,
            events,
            progress,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            observer,
        } = self;
        let started_at = ready.started_at;
        let mut warnings = warnings::admission(&ready.prune_report);
        let mut details = report_details(&ready, capture_enabled, comparator.clone(), kind);
        details.shape = report_shape;
        if let Some(target) = &mut details.target {
            target.io = file_io.clone();
        }
        for role in &details.roles {
            if let Some(detail) = role.header_warning() {
                details.warnings.push(WireWarning {
                    kind: WarningKind::TestlibHeader {
                        role: role.role,
                        detail,
                    },
                    message: format!(
                        "{}: testlib header candidate provenance: {}",
                        role.role.wire_token(),
                        match detail {
                            report::TestlibHeaderDetail::NotIdentified => "not_identified",
                            report::TestlibHeaderDetail::NotPinned => "not_pinned",
                            report::TestlibHeaderDetail::Declared => "declared",
                        }
                    ),
                });
            }
        }
        if let Some(Hook {
            outcome: HookResult::NotPlanned { reason },
            ..
        }) = disabled_hooks.first()
        {
            details.warnings.push(WireWarning {
                kind: WarningKind::HooksDisabled { reason },
                message: format!("hooks_disabled: {reason}"),
            });
        }
        details.hooks = disabled_hooks;
        let mut target = TargetState {
            pending: None,
            free: None,
            validated: None,
        };
        match ready.planned_target {
            Some(PlannedTarget::Build(pending)) => target.pending = Some(Box::new(pending)),
            Some(PlannedTarget::BuildFree(free)) => target.free = Some(free),
            None => {}
        }
        let mut judging = ready.judging.take();
        let mut role_targets: Vec<_> = judging.as_mut().map_or_else(Vec::new, |judging| {
            judging
                .roles
                .iter_mut()
                .map(|role| {
                    (
                        role.captured.role,
                        TargetState {
                            pending: role.pending_build.take(),
                            free: None,
                            validated: None,
                        },
                    )
                })
                .collect()
        });
        let staged: std::collections::BTreeMap<_, _> = judging
            .as_ref()
            .into_iter()
            .flat_map(|j| &j.roles)
            .flat_map(|r| &r.runs)
            .map(|r| (r.execution, r.staged.clone()))
            .collect();
        let testlib: std::collections::BTreeMap<_, _> = judging
            .as_ref()
            .into_iter()
            .flat_map(|j| {
                let scoring = j.check.report_config().scoring;
                j.roles
                    .iter()
                    .filter(|role| {
                        role.captured.role == report::RoleName::Checker
                            && role.captured.spec.profile.value == report::JudgeProfile::Testlib
                    })
                    .flat_map(move |role| role.runs.iter().map(move |run| (run.execution, scoring)))
            })
            .collect();
        let context = Context {
            file_io: file_io.as_ref(),
            testlib: &testlib,
            staged: &staged,
            runs: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            frozen_outputs: Default::default(),
            dir: &ready.dir,
            workspace: &ready.workspace,
            problem: ready.problem.as_ref(),
            job_id: ready.id.as_str(),
            owner: &ready.owner,
            budget: &ready.budget,
            platform: ready.platform,
            cancel: &cancel,
            events: &events,
            output_events,
            cwd_bytes: ready.cwd_bytes,
            cwd_walker: Arc::default(),
            #[cfg(any(test, feature = "storage-test-hooks"))]
            observer: observer.as_ref(),
        };
        events.offer(EventPayload::JobStarted {
            job: ready.id.clone(),
            kind,
            sequence: ready.sequence.clone(),
        });
        let mut executions = Vec::with_capacity(plan.len() + after_test.len());
        let mut tests = Vec::new();
        let mut build = None;
        let mut admission_closed = false;
        let mut stopped = false;
        let mut builds_succeeded = true;
        // Reserve report + replacement temporary before admitting any run.
        let (planned_tests, planned_aggregates) =
            report_populations(kind, &plan, ready.manifest.entries().len());
        let report_reservation = if let Some(judging) = judging.take() {
            Some(judging.report_reservation)
        } else {
            report::wire::report_reservation(
                plan.len() + after_test.len(),
                planned_tests,
                planned_aggregates,
                report_shape,
            )
            .ok()
            .and_then(|bytes| ready.budget.reserve(bytes).ok())
        };
        let report_refused = report_reservation.is_none();
        let planned_count = (plan.len() + after_test.len()) as u32;
        persistence.configure(planned_count, cancel.token());
        let generated: Vec<_> = plan
            .iter()
            .filter_map(|p| match &p.recipe {
                Recipe::Deferred(stage) => Some((p.role.clone(), stage.as_ref().clone())),
                _ => None,
            })
            .collect();
        let judged = !details.roles.is_empty();
        let mut plan = plan.into_iter().peekable();
        while let Some(planned) = plan.next() {
            if kind == JobKind::Cmd && executions.len() == 1 {
                crate::storage::crash::barrier("command.between_iterations");
            }
            let earlier_retention_refusal = admission_closed;
            #[cfg(any(test, feature = "storage-test-hooks"))]
            context.phase_point(Point::BeforeAdmission(planned.index));
            let active_target = match planned.role {
                Role::RoleBuild { role } => {
                    &mut role_targets
                        .iter_mut()
                        .find(|(name, _)| *name == role)
                        .expect("prepared role build")
                        .1
                }
                _ => &mut target,
            };
            let input_not_validated = judging::input_not_validated(&planned, &executions);
            let checker_not_eligible = judging::checker_not_eligible(
                &planned,
                &executions,
                file_io.as_ref().is_some_and(|io| io.output.is_some()),
            );
            // Report §2.1 order: cancelled, build_not_succeeded, admission_closed.
            let skip = if cancel.lock().intent.is_some() {
                Some(SkipReason::Cancelled)
            } else if planned.depends_on_build && !builds_succeeded {
                Some(SkipReason::BuildNotSucceeded)
            } else if admission_closed {
                Some(SkipReason::AdmissionClosed)
            } else if input_not_validated {
                Some(SkipReason::InputNotValidated)
            } else if checker_not_eligible {
                Some(SkipReason::NotEligible)
            } else if stopped {
                Some(SkipReason::StopPolicy)
            } else {
                None
            };
            let mut execution = if let Some(reason) = skip {
                skipped(planned, reason)
            } else {
                Box::pin(context.execute(
                    planned,
                    active_target,
                    executor,
                    persistence,
                    report_refused,
                    &mut admission_closed,
                ))
                .await
            };
            execution.result = report::validate_execution(
                execution.result,
                &report::SkipConditions {
                    job: kind,
                    depends_on_unsucceeded_build: execution.plan.depends_on_build
                        && !builds_succeeded,
                    earlier_retention_refusal,
                    submit_not_succeeded: false,
                    input_not_validated,
                    not_eligible: checker_not_eligible,
                    keep_going,
                    earlier_primary_failed: stopped,
                    intent_before_admission: cancel.lock().intent.is_some(),
                    after_test_hook: false,
                    hooks_eligible: false,
                },
            );
            if let Some(invariant) = report::execution_invariant(&execution.result) {
                execution.result.invariant = Some(invariant);
                record_internal(
                    &cancel,
                    invariant,
                    InternalLocus::Execution {
                        index: execution.plan.index,
                    },
                );
            }
            if execution.plan.stage == Stage::Build {
                if let (Role::RoleBuild { role }, Coverage::Skipped { reason }) =
                    (&execution.plan.role, execution.result.coverage)
                {
                    details
                        .roles
                        .iter_mut()
                        .find(|r| r.role == *role)
                        .expect("role report")
                        .build = Some(role_builds::skipped(execution.plan.index, reason));
                    builds_succeeded = false;
                } else {
                    let mut post = PostBuild::default();
                    let pending = active_target.pending.take().expect("prepared build");
                    let id = execution.run.as_ref().map(|_| pending.id().clone());
                    let root = id.as_ref().map(|_| pending.output_root().to_owned());
                    let artifact_count = pending.required_outputs().len();
                    let artifacts: Vec<_> = pending
                        .required_outputs()
                        .iter()
                        .filter_map(|path| {
                            path.strip_prefix(pending.output_root())
                                .ok()
                                .map(|p| p.as_os_str().to_owned())
                        })
                        .take(16)
                        .collect();
                    let producer = &execution.plan.role;
                    let consumers = generated
                        .iter()
                        .filter(|(role, _)| match (producer, role) {
                            (Role::RoleBuild { role: name }, Role::Checker { .. }) => {
                                *name == report::RoleName::Checker
                            }
                            (Role::RoleBuild { role: name }, Role::Validator { .. }) => {
                                *name == report::RoleName::Validator
                            }
                            (
                                Role::Build,
                                Role::Checker { .. }
                                | Role::Validator { .. }
                                | Role::Interactor { .. },
                            )
                            | (Role::RoleBuild { .. }, _) => false,
                            (Role::Build, _) => true,
                            _ => false,
                        })
                        .map(|(_, stage)| stage.clone())
                        .collect();
                    if let Some(ExecutionRecord::Started(outcome)) = execution.evidence.as_deref() {
                        match caught(async {
                            executor
                                .finish_build(
                                    *pending,
                                    build::BuildExecutionOutcome::from(&**outcome),
                                    consumers,
                                    ready.owner.clone(),
                                )
                                .await
                        })
                        .await
                        {
                            Ok(Ok(completion)) => {
                                post = completion.post;
                                active_target.validated = completion.validated;
                            }
                            // A panic or an unavailable creation thread, as a
                            // panicking inline validation before.
                            Ok(Err(_)) | Err(()) => post.invariant = Some(Invariant::BuildEvidence),
                        }
                    }
                    let result = report::build_result(&execution.result, &post);
                    if let BuildResult::InternalError { invariant } = result {
                        record_internal(&cancel, invariant, InternalLocus::Build);
                    }
                    let succeeded = result == BuildResult::Succeeded;
                    builds_succeeded &= succeeded;
                    let built = BuildReport {
                        execution: execution.plan.index,
                        build_id: id,
                        output_root: root,
                        result,
                        artifacts: if succeeded { artifacts } else { Vec::new() },
                    };
                    if let Role::RoleBuild { role } = execution.plan.role {
                        let role_report = details
                            .roles
                            .iter_mut()
                            .find(|r| r.role == role)
                            .expect("role report");
                        role_report.build = Some(role_builds::report(built, artifact_count));
                    } else {
                        build = Some(built);
                    }
                }
            }

            if let Some(test) = execution.plan.test.as_ref().filter(|_| {
                kind == JobKind::Test && !judged && matches!(execution.plan.role, Role::Test { .. })
            }) {
                let test = test.clone();
                let check = context.check(
                    &execution,
                    &test.answer,
                    comparator.as_ref().expect("test comparator"),
                );
                let verdict = catch_unwind(AssertUnwindSafe(|| {
                    #[cfg(any(test, feature = "storage-test-hooks"))]
                    context.point(Point::BeforeTestVerdict(execution.plan.index));
                    let _state = cancel.lock();
                    report::test_verdict(&context.test_result(&execution), &check)
                }))
                .unwrap_or(report::TestVerdict::InternalError {
                    invariant: Invariant::TestCheck,
                });
                if let report::TestVerdict::InternalError { invariant } = verdict {
                    record_internal(
                        &cancel,
                        invariant,
                        InternalLocus::Test {
                            test: test.id.to_string(),
                        },
                    );
                }
                let test_report = TestReport {
                    frozen: test,
                    first_execution: execution.plan.index,
                    execution_count: 1,
                    class: context.test_result(&execution).class,
                    check: Some(check),
                    verdict: Some(verdict),
                    output_file: context.output_file(&execution),
                    score: None,
                    validation: None,
                    judges: Vec::new(),
                };
                if let Some(progress) = &progress {
                    let context = report::DisplayContext {
                        output_file: file_io
                            .as_ref()
                            .and_then(|io| io.output.as_ref())
                            .zip(test_report.output_file.as_ref())
                            .map(|(name, output)| (name.as_str().into(), output.clone())),
                        comparator: comparator.as_ref().map(report::comparator_token),
                        wall_deadline: execution.summary.as_ref().map(|s| s.deadline),
                        measured_wall: execution.evidence.as_deref().and_then(|e| match e {
                            ExecutionRecord::Started(root) => root.timing.wall,
                            ExecutionRecord::NotStarted(_) | ExecutionRecord::Transferred => None,
                        }),
                        budgets: vec![
                            (
                                report::Stream::Stdout,
                                ready
                                    .target
                                    .as_ref()
                                    .expect("test target")
                                    .run
                                    .stdout
                                    .bytes
                                    .value,
                            ),
                            (
                                report::Stream::Stderr,
                                ready
                                    .target
                                    .as_ref()
                                    .expect("test target")
                                    .run
                                    .stderr
                                    .bytes
                                    .value,
                            ),
                        ],
                        cleanup_unverified: execution
                            .summary
                            .as_ref()
                            .is_some_and(|s| s.notes.contains(&report::RunNote::CleanupUnverified)),
                        ..report::DisplayContext::default()
                    };
                    let display = report::label(
                        report::LabelSubject::Test {
                            verdict: test_report.verdict.as_ref().expect("test verdict"),
                            class: test_report.class.as_ref(),
                            check: test_report.check.as_ref(),
                        },
                        &context,
                    );
                    progress.result(&format!(
                        "{}  {}",
                        test_report.frozen.id,
                        report::render_label(&display)
                    ));
                }
                tests.push(test_report);
            }
            if let Some(test) = execution
                .plan
                .test
                .as_ref()
                .filter(|_| kind == JobKind::Cmd)
            {
                if let Some(previous) = tests
                    .last_mut()
                    .filter(|previous| previous.frozen.id == test.id)
                {
                    previous.execution_count += 1;
                } else {
                    tests.push(TestReport {
                        frozen: test.clone(),
                        first_execution: execution.plan.index,
                        execution_count: 1,
                        class: None,
                        check: None,
                        verdict: None,
                        output_file: None,
                        score: None,
                        validation: None,
                        judges: Vec::new(),
                    });
                }
            }
            if kind == JobKind::Cmd
                && !keep_going
                && execution.plan.stage != Stage::Build
                && report::command_failed(&execution.result).unwrap_or(true)
            {
                stopped = true;
            }
            events.offer(EventPayload::Progress {
                done: executions.len() as u32 + 1,
                total: planned_count,
            });
            if let Some(progress) = progress
                .as_ref()
                .filter(|_| !matches!(kind, JobKind::Test | JobKind::Run))
            {
                progress.offer(&format!(
                    "{}/{} executions finished",
                    executions.len() + 1,
                    planned_count
                ));
            }
            if kind == JobKind::Hook {
                record_hook(&mut details, &execution);
            }
            let test = execution.plan.test.clone();
            executions.push(execution);
            if let Some(test) = test.filter(|t| {
                judged
                    && plan
                        .peek()
                        .is_none_or(|next| next.test.as_ref().is_none_or(|next| next.id != t.id))
            }) {
                let test_report = judging::finish_test(
                    &context,
                    test,
                    &executions,
                    comparator.as_ref().expect("test comparator"),
                    details
                        .target
                        .as_ref()
                        .and_then(|t| t.check.as_ref())
                        .expect("judged config"),
                    &details.roles,
                );
                if let Some(report::TestVerdict::InternalError { invariant }) = test_report.verdict
                {
                    record_internal(
                        &cancel,
                        invariant,
                        InternalLocus::Test {
                            test: test_report.frozen.id.to_string(),
                        },
                    );
                }
                if let Some(progress) = &progress {
                    progress.result(&format!(
                        "{}  {}",
                        test_report.frozen.id,
                        report::render_label(&judging::display(
                            &test_report,
                            &executions,
                            comparator.as_ref().expect("test comparator"),
                            ready.target.as_ref().expect("test target"),
                            details
                                .target
                                .as_ref()
                                .and_then(|t| t.check.as_ref())
                                .expect("judged config"),
                            file_io.as_ref(),
                        ))
                    ));
                }
                tests.push(test_report);
            }
        }
        for execution in &executions {
            for detail in &execution.io_warnings {
                details.warnings.push(WireWarning {
                    kind: WarningKind::IoScratch {
                        run: execution
                            .run
                            .as_ref()
                            .expect("admitted I/O")
                            .as_str()
                            .into(),
                        detail: *detail,
                    },
                    message: format!("file I/O scratch: {}", detail.wire_token()),
                });
            }
        }
        for execution in executions.iter().filter(|e| e.judge_scratch_kept) {
            details.warnings.push(WireWarning {
                kind: WarningKind::JudgeScratch {
                    run: execution
                        .run
                        .as_ref()
                        .expect("admitted scratch")
                        .as_str()
                        .to_owned(),
                    detail: report::JudgeScratchDetail::StagedCopyKept,
                },
                message: "judge staged copies could not be removed".into(),
            });
        }
        let scoring = details
            .target
            .as_ref()
            .and_then(|t| t.check.as_ref())
            .map_or(report::Scoring::PassFail, |c| c.scoring);
        details.score =
            report::aggregate_score(scoring, &tests.iter().map(|t| t.score).collect::<Vec<_>>())
                .ok()
                .flatten();
        let primary: Vec<_> = executions.iter().map(|e| e.result.clone()).collect();
        let test_verdicts: Vec<_> = tests.iter().filter_map(|t| t.verdict.clone()).collect();
        let mut result = {
            let mut state = cancel.lock();
            let result = report::job_verdict(&report::JobInput {
                roles: &details.roles,
                scoring,
                kind,
                build: build.as_ref().map(|b| &b.result),
                primary: &primary,
                tests: &test_verdicts,
                selected_tests: if kind == JobKind::Test {
                    tests.len() as u32
                } else {
                    0
                },
                internal_before_freeze: state.internal.as_ref(),
                internal_after_freeze: None,
            });
            state.internal = result.internal.clone();
            state.primary = Some(result.primary_verdict);
            state.phase = if kind == JobKind::Test {
                CancelPhase::Fact
            } else {
                CancelPhase::Settlement
            };
            result
        };
        #[cfg(any(test, feature = "storage-test-hooks"))]
        context.phase_point(Point::AfterPrimaryFreeze);
        if kind == JobKind::Test {
            let eligible = eligible(result.primary_verdict);
            if caught(async { details.fingerprints = hooks.fingerprints() })
                .await
                .is_err()
            {
                record_internal(
                    &cancel,
                    Invariant::JobEvidence,
                    InternalLocus::Phase { phase: "fact" },
                );
            }
            if eligible && cancel.lock().internal.is_none() {
                let mut snapshot = None;
                let mut cutoff = || *snapshot.get_or_insert_with(|| cancel.lock().intent.is_some());
                if caught(async { hooks.fact(result.primary_verdict, &mut cutoff).await })
                    .await
                    .is_err()
                {
                    record_internal(
                        &cancel,
                        Invariant::JobEvidence,
                        InternalLocus::Phase { phase: "fact" },
                    );
                }
            }
            if caught(async {
                if let Some((fact, warnings)) = hooks.fact_evidence() {
                    details.fact = Some(fact);
                    details.warnings.extend(warnings);
                }
            })
            .await
            .is_err()
            {
                record_internal(
                    &cancel,
                    Invariant::JobEvidence,
                    InternalLocus::Phase { phase: "fact" },
                );
            }
            cancel.lock().phase = CancelPhase::Hooks;
            let start = || {
                let state = cancel.lock();
                if !self::eligible(state.primary.expect("primary verdict frozen at V"))
                    || state.internal.is_some()
                {
                    Some(SkipReason::JobNotEligible)
                } else if state.intent.is_some() {
                    Some(SkipReason::Cancelled)
                } else {
                    None
                }
            };
            if caught(async { hooks.hooks(result.primary_verdict, &start).await })
                .await
                .is_err()
            {
                record_internal(
                    &cancel,
                    Invariant::JobEvidence,
                    InternalLocus::Phase { phase: "hooks" },
                );
            }
            for planned in after_test {
                #[cfg(any(test, feature = "storage-test-hooks"))]
                context.phase_point(Point::BeforeHookStart(planned.index));
                let skip = start();
                let mut execution = if let Some(reason) = skip {
                    skipped(planned, reason)
                } else if admission_closed {
                    skipped(planned, SkipReason::AdmissionClosed)
                } else {
                    Box::pin(context.execute(
                        planned,
                        &mut target,
                        executor,
                        persistence,
                        report_refused,
                        &mut admission_closed,
                    ))
                    .await
                };
                execution.result = report::validate_execution(
                    execution.result,
                    &report::SkipConditions {
                        job: kind,
                        depends_on_unsucceeded_build: false,
                        earlier_retention_refusal: admission_closed,
                        submit_not_succeeded: false,
                        input_not_validated: false,
                        not_eligible: false,
                        keep_going: true,
                        earlier_primary_failed: false,
                        intent_before_admission: cancel.lock().intent.is_some(),
                        after_test_hook: true,
                        hooks_eligible: skip != Some(SkipReason::JobNotEligible),
                    },
                );
                if let Some(invariant) = report::execution_invariant(&execution.result) {
                    execution.result.invariant = Some(invariant);
                    record_internal(
                        &cancel,
                        invariant,
                        InternalLocus::Execution {
                            index: execution.plan.index,
                        },
                    );
                }
                record_hook(&mut details, &execution);
                events.offer(EventPayload::Progress {
                    done: executions.len() as u32 + 1,
                    total: planned_count,
                });
                executions.push(execution);
            }
            cancel.lock().phase = CancelPhase::Settlement;
        }
        if let Some(warning) = warnings::cleanup(&executions) {
            warnings.push(warning);
        }
        for execution in &executions {
            if let Some(bytes) = execution
                .summary
                .as_ref()
                .and_then(|s| s.cwd_overflow_bytes)
            {
                if details.warnings.len() == 24 {
                    details.warnings_dropped += 1;
                    continue;
                }
                details.warnings.push(WireWarning {
                    kind: WarningKind::Retention {
                        detail: "cwd_overflow",
                        run: execution.run.as_ref().map(|r| r.as_str().to_owned()),
                        bytes,
                    },
                    message: format!(
                        "CwdOverflow: working directory exceeded its byte limit ({bytes} bytes)"
                    ),
                });
            }
        }
        let aggregates = if kind == JobKind::Cmd {
            aggregate(&executions)
        } else {
            Vec::new()
        };
        if let Some(progress) = progress.as_ref().filter(|_| kind == JobKind::Cmd) {
            for test in &tests {
                let first = test.first_execution as usize;
                let last = first + test.execution_count as usize;
                let primary: Vec<_> = executions[first..last]
                    .iter()
                    .map(|execution| execution.result.clone())
                    .collect();
                let verdict = report::job_verdict(&report::JobInput {
                    roles: &[],
                    scoring: crate::report::Scoring::PassFail,
                    kind,
                    build: None,
                    primary: &primary,
                    tests: &[],
                    selected_tests: 0,
                    internal_before_freeze: None,
                    internal_after_freeze: None,
                });
                let display = report::label(
                    report::LabelSubject::Job(verdict.verdict),
                    &report::DisplayContext::default(),
                );
                let group = aggregates
                    .iter()
                    .find(|group| group.test.as_deref() == Some(test.frozen.id.as_str()))
                    .expect("command test aggregate");
                let mut line = format!(
                    "{}  {}  {}/{} succeeded",
                    test.frozen.id,
                    report::render_label(&display),
                    group.succeeded,
                    group.iterations
                );
                if let Some([min, median, max]) = group.wall {
                    line.push_str(&format!(
                        "; wall min/median/max {} / {} / {} s",
                        min.as_secs_f64(),
                        median.as_secs_f64(),
                        max.as_secs_f64()
                    ));
                }
                progress.result(&line);
            }
        }
        #[cfg(any(test, feature = "storage-test-hooks"))]
        context.phase_point(Point::Settlement);
        let settlement_warnings =
            match caught(async { persistence.settle(&mut executions).await }).await {
                Ok(warnings) => warnings,
                Err(_) => {
                    persistence::unavailable(&mut executions);
                    record_internal(
                        &cancel,
                        Invariant::JobEvidence,
                        InternalLocus::Phase {
                            phase: "settlement",
                        },
                    );
                    Vec::new()
                }
            };
        warnings.extend(settlement_warnings);
        cancel.lock().phase = CancelPhase::Closure;
        #[cfg(any(test, feature = "storage-test-hooks"))]
        context.phase_point(Point::Closure);
        let dropped = events.close();
        if let Some(progress) = progress {
            let lines = progress.close();
            if lines != 0 {
                details.warnings.push(WireWarning {
                    kind: WarningKind::ProgressDropped { lines },
                    message: format!("{lines} progress lines dropped"),
                });
            }
        }
        #[cfg(any(test, feature = "storage-test-hooks"))]
        context.phase_point(Point::BeforeSnapshot);
        let hook_results: Vec<_> = executions
            .iter()
            .skip(primary.len())
            .map(|e| e.result.clone())
            .collect();
        let counts = report::coverage_counts(&primary, &hook_results, &test_verdicts);
        details.hooks_dropped = details.hooks.len().saturating_sub(32) as u32;
        details.hooks.truncate(32);
        details.warnings_dropped += details.warnings.len().saturating_sub(24) as u32;
        details.warnings.truncate(24);

        if let Some(target) = &mut details.target {
            if let Some(root) = executions
                .iter()
                .filter(|execution| execution.plan.stage == Stage::Run)
                .find_map(|execution| match execution.evidence.as_deref() {
                    Some(ExecutionRecord::Started(root)) => Some(root),
                    _ => None,
                })
            {
                target.local_limits.clone_from(&root.limits);
            }
        }
        let mut report = {
            let mut state = cancel.lock();
            if state.internal.is_some() {
                result.verdict = JobVerdict::InternalError;
                result.internal = state.internal.clone();
            }
            let cancellation = state.intent;
            let late_cancellation = cancellation.is_some_and(|c| c.phase != CancelPhase::Primary);
            // Keep the earliest ordinary warnings in serialization order, reserving
            // the final slot only when late cancellation was actually recorded.
            let warning_slots = 24 - usize::from(late_cancellation);
            details.warnings_dropped += warnings.len().saturating_sub(warning_slots) as u32;
            warnings.truncate(warning_slots);
            let detail_slots = warning_slots - warnings.len();
            details.warnings_dropped += details.warnings.len().saturating_sub(detail_slots) as u32;
            details.warnings.truncate(detail_slots);
            if late_cancellation {
                warnings.push(Warning::LateCancellation);
            }
            details.pre_delivery_exit =
                pre_delivery_exit(kind, result.verdict, cancellation, details.score.as_ref());
            let report = FinalizedReport {
                data: Arc::new(ReportData {
                    details,
                    schema: 1,
                    document: "job_report",
                    platform: if cfg!(windows) { "windows" } else { "unix" },
                    outcome: "finished",
                    cpf_version: env!("CARGO_PKG_VERSION"),
                    command: kind,
                    job: JobIdentity {
                        id: ready.id,
                        dir: ready.dir,
                        sequence: ready.sequence,
                        kind,
                    },
                    workspace: ready.workspace,
                    problem: ready.problem,
                    target: ready.target.filter(|_| capture_enabled),
                    comparator,
                    capture: ready.capture_report.filter(|_| capture_enabled),
                    build,
                    executions,
                    tests,
                    aggregates,
                    result: ReportResult {
                        verdict: result.verdict,
                        primary_verdict: result.primary_verdict,
                        partial: counts.partial,
                        counts,
                        cancellation,
                        internal: result.internal,
                    },
                    warnings,
                    events: EventSummary { dropped },
                    timing: Timing {
                        started_at,
                        finished_at: SystemTime::now(),
                        duration: cancel.start.elapsed(),
                    },
                }),
                _owner: ready.owner,
                post_freeze_internal: false,
                persistence: ReportPersistence::Failed {
                    error: PersistError::WorkerUnavailable,
                    message: "report not yet published".into(),
                },
            };
            state.frozen = true;
            crate::process::freeze_report();
            report
        };
        #[cfg(any(test, feature = "storage-test-hooks"))]
        if let Some(observer) = &observer {
            if catch_unwind(AssertUnwindSafe(|| (observer.0)(Point::AfterSnapshot))).is_err() {
                report.post_freeze_internal = true;
            }
        }
        if !report.post_freeze_internal {
            report.persistence =
                match caught(persistence.publish_report(report.data.clone(), report_reservation))
                    .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        report.post_freeze_internal = true;
                        ReportPersistence::Failed {
                            error: PersistError::WorkerUnavailable,
                            message: "report persistence worker panicked".into(),
                        }
                    }
                };
        }
        let published = matches!(report.persistence, ReportPersistence::Published { .. });
        let report = Arc::new(report);
        events.finish(
            report.clone(),
            published.then(|| report.data.job.id.clone()),
        );
        report
    }
}

struct TargetState<'probe> {
    pending: Option<Box<build::PendingBuild<'probe>>>,
    free: Option<build::PreflightedBuildFree>,
    validated: Option<ValidatedBuild>,
}
fn record_hook(details: &mut ReportDetails, execution: &ExecutionReport) {
    let Role::Hook { name, trigger } = &execution.plan.role else {
        return;
    };
    let outcome = report::hook_outcome(&execution.result);
    let result = HookResult::Planned(outcome.clone());
    if outcome != report::HookOutcome::Succeeded {
        details.warnings.push(WireWarning {
            kind: WarningKind::Hook {
                name: name.clone(),
                outcome: result.clone(),
            },
            message: format!("hook {name}: {}", outcome.wire_token()),
        });
    }
    details.hooks.push(Hook {
        name: name.clone(),
        trigger: if trigger == "after_test" {
            "after_test"
        } else {
            "on_switch"
        },
        execution: Some(execution.plan.index),
        outcome: result,
    });
}

struct Context<'a> {
    file_io: Option<&'a report::FileIo>,
    workspace: &'a Path,
    problem: Option<&'a crate::exec::ProblemBinding>,
    job_id: &'a str,
    dir: &'a Path,
    owner: &'a JobOwner,
    budget: &'a Arc<JobBudget>,
    platform: Platform,
    cancel: &'a CancelState,
    events: &'a Events,
    output_events: bool,
    cwd_bytes: Option<u64>,
    testlib: &'a std::collections::BTreeMap<u32, report::Scoring>,
    staged: &'a std::collections::BTreeMap<u32, Vec<crate::jobs::roles::StagedFile>>,
    runs: std::cell::RefCell<std::collections::BTreeMap<u32, RunId>>,
    frozen_outputs: std::cell::RefCell<std::collections::BTreeMap<u32, u64>>,
    /// Set while this job's post-run cwd walker runs (at most one).
    cwd_walker: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    observer: Option<&'a Observation>,
}

impl Context<'_> {
    #[cfg(any(test, feature = "storage-test-hooks"))]
    fn point(&self, point: Point) {
        if let Some(observer) = self.observer {
            (observer.0)(point);
        }
    }
    #[cfg(any(test, feature = "storage-test-hooks"))]
    fn phase_point(&self, point: Point) {
        if catch_unwind(AssertUnwindSafe(|| self.point(point))).is_err() {
            record_internal(
                self.cancel,
                Invariant::JobEvidence,
                InternalLocus::Phase {
                    phase: "finalization",
                },
            );
        }
    }

    async fn execute(
        &self,
        plan: PlannedExecution,
        target: &mut TargetState<'_>,
        executor: &mut dyn Executor,
        persistence: &mut dyn Persistence,
        report_refused: bool,
        admission_closed: &mut bool,
    ) -> ExecutionReport {
        let admitted_at = std::time::Instant::now();
        let run = {
            let mut state = self.cancel.lock();
            if state.intent.is_some() {
                return skipped(plan, SkipReason::Cancelled);
            }
            let run = RunId(format!("r{:05}", state.next_run));
            state.next_run += 1;
            run
        };
        #[cfg(any(test, feature = "storage-test-hooks"))]
        self.phase_point(Point::AfterAdmission(plan.index));
        self.runs.borrow_mut().insert(plan.index, run.clone());
        let directory = self.dir.join(run.as_str());
        let planned_cwd = crate::jobs::plan::RunLayout::new(self.dir, plan.index as usize).cwd;
        let staged = self
            .staged
            .get(&plan.index)
            .into_iter()
            .flatten()
            .map(|file| {
                let mut file = file.clone();
                file.destination = file
                    .destination
                    .strip_prefix(&planned_cwd)
                    .expect("planned judge scratch")
                    .components()
                    .fold(directory.join("cwd"), |p, c| p.join(c));
                if let Some(source) = &mut file.source {
                    if let Ok(relative) = source.strip_prefix(self.dir) {
                        let mut components = relative.components();
                        if let Some(index) = components
                            .next()
                            .and_then(|c| c.as_os_str().to_str())
                            .and_then(|c| c.strip_prefix('r'))
                            .and_then(|n| n.parse::<u32>().ok())
                        {
                            // Only the fixed `output` copies a solution capture; copy
                            // its frozen length, never what the file holds now.
                            if file.destination == directory.join("cwd").join("output") {
                                file.bytes = self.frozen_outputs.borrow().get(&index).copied();
                            }
                            if let Some(run) = self.runs.borrow().get(&index) {
                                *source =
                                    components.fold(self.dir.join(run.as_str()), |p, c| p.join(c));
                            }
                        }
                    }
                }
                file
            })
            .collect();
        let io = self
            .file_io
            .filter(|_| matches!(plan.role, Role::Test { .. }));
        let stage_input =
            io.and_then(|io| io.input.as_ref())
                .map(|name| crate::supervisor::StagedInput {
                    name: name.as_str().into(),
                    source: plan
                        .test
                        .as_ref()
                        .expect("frozen solution input")
                        .input
                        .path
                        .clone(),
                });
        #[cfg(any(
            target_os = "linux",
            target_os = "macos",
            all(test, feature = "storage-test-hooks")
        ))]
        let io_state = io.map(|_| crate::supervisor::file_io::FileIoState::default());
        let stdin = if stage_input.is_some() {
            None
        } else if matches!(plan.role, Role::Validator { .. }) {
            Some(directory.join("cwd").join("input"))
        } else {
            plan.stdin.clone()
        };
        let recipe = if plan.stage == Stage::Build {
            target
                .pending
                .as_ref()
                .map(|pending| PreparationRecipe::Target(Box::new(pending.stage_recipe())))
        } else if matches!(plan.recipe, Recipe::TargetRun) {
            target
                .validated
                .as_ref()
                .map(|build| PreparationRecipe::Target(Box::new(build.stage_recipe())))
                .or_else(|| {
                    target
                        .free
                        .as_ref()
                        .map(|free| PreparationRecipe::Target(Box::new(free.stage_recipe())))
                })
        } else {
            Some(PreparationRecipe::Stage(plan.recipe.clone()))
        };
        let preparation = RunPreparation {
            staged,
            budget: self.budget.clone(),
            directory: directory.clone(),
            recipe,
            owner: self.owner.clone(),
            #[cfg(feature = "storage-test-hooks")]
            observation: self.observer.cloned(),
            #[cfg(feature = "storage-test-hooks")]
            execution: plan.index,
        };
        let prepared =
            caught(async { executor.prepare_run(preparation, self.cancel.token()).await }).await;
        let PreparedRun {
            mut stage,
            mut failure,
            writable,
            interrupted,
            scratch,
        } = prepared.unwrap_or(PreparedRun {
            stage: None,
            failure: None,
            writable: false,
            interrupted: false,
            scratch: None,
        });
        if plan.stage == Stage::Hook {
            if let Some(stage) = &mut stage {
                let mut injected = vec![
                    ("CPF_HOOK_DEPTH", std::ffi::OsString::from("1")),
                    ("CPF_WORKSPACE", self.workspace.as_os_str().to_owned()),
                    ("CPF_JOB_ID", std::ffi::OsString::from(self.job_id)),
                ];
                if let Some(problem) = self.problem {
                    injected.push(("CPF_PROBLEM_ID", std::ffi::OsString::from(&problem.id)));
                    injected.push(("CPF_PROBLEM_DIR", problem.dir.as_os_str().to_owned()));
                }
                for (name, value) in injected {
                    // Last, never overridable (config §4.3); Windows names
                    // compare ASCII case-insensitively like the reserved prefix.
                    stage.environment.retain(|(key, _)| {
                        if cfg!(windows) {
                            !key.as_encoded_bytes().eq_ignore_ascii_case(name.as_bytes())
                        } else {
                            key != name
                        }
                    });
                    stage.environment.push((name.into(), value));
                }
            }
        }
        let command = stage
            .as_ref()
            .and_then(|s| s.launch_command(self.platform).ok());
        let policies = stage
            .as_ref()
            .map_or([crate::supervisor::Exceed::Discard; 2], |s| {
                [exceed(s.stdout.policy), exceed(s.stderr.policy)]
            });
        let mut metadata = None;
        let mut resources = None;
        if failure.is_none() && stage.is_some() && command.is_some() {
            let amount = if matches!(plan.role, Role::Raw) {
                Some(0) // Console forwarding retains no output files (E §5.4).
            } else {
                stage.as_ref().and_then(|s| {
                    s.stdout
                        .bytes
                        .value
                        .checked_add(s.stderr.bytes.value)
                        .and_then(|amount| {
                            amount.checked_add(if io.is_some_and(|io| io.output.is_some()) {
                                s.stdout.bytes.value
                            } else {
                                0
                            })
                        })
                })
            };
            let reservations = if report_refused {
                None
            } else {
                amount.and_then(|amount| reserve(self.budget, amount))
            };
            let input_reservation = if stage_input.is_some() {
                Some(
                    self.budget
                        .reserve(plan.test.as_ref().expect("frozen input").input.len),
                )
            } else {
                None
            };
            let reservations =
                reservations.filter(|_| input_reservation.as_ref().is_none_or(Result::is_ok));
            if let Some((streams, meta)) = reservations {
                metadata = Some(meta);
                resources = Some(RunResources {
                    #[cfg(any(
                        target_os = "linux",
                        target_os = "macos",
                        all(test, feature = "storage-test-hooks")
                    ))]
                    file_io: io_state.as_ref().map(|state| {
                        Box::new(crate::supervisor::file_io::FileIoSetup {
                            cwd: directory.join("cwd"),
                            input: stage_input.clone(),
                            output: io.and_then(|io| io.output.as_ref()).map(|name| {
                                crate::supervisor::OutputFileSpec {
                                    name: name.as_str().into(),
                                    budget: stage.as_ref().expect("stage").stdout.bytes.value,
                                }
                            }),
                            destination: directory.join("out").join("output"),
                            reservation: input_reservation.and_then(Result::ok),
                            owner: self.owner.clone(),
                            state: state.clone(),
                            #[cfg(feature = "storage-test-hooks")]
                            observation: self.observer.cloned(),
                            #[cfg(feature = "storage-test-hooks")]
                            execution: plan.index,
                            #[cfg(all(test, feature = "storage-test-hooks"))]
                            hooks: None,
                        })
                    }),
                    reservation: streams,
                    _owner: self.owner.clone(),
                    retained_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                });
            } else {
                *admission_closed = true;
                failure = Some(StartFailure {
                    reason: NotStartedReason::RetentionBudget,
                    api: None,
                    os_error: None,
                    invalid_limit: None,
                });
            }
        }
        let spec = stage.zip(command).map(|(resolved, command)| {
            let mut spec = ExecutionSpec {
                run: run.clone(),
                execution: plan.index,
                stage: plan.stage,
                role: plan.role.clone(),
                command,
                stdin,
                stage_input,
                output_file: io.and_then(|io| io.output.as_ref()).map(|name| {
                    crate::supervisor::OutputFileSpec {
                        name: name.as_str().into(),
                        budget: resolved.stdout.bytes.value,
                    }
                }),
                resolved,
                directory: directory.clone(),
                digest: None,
                output_events: self.output_events,
            };
            spec.digest = Some(spec.spec_digest());
            Arc::new(spec)
        });
        if spec
            .as_ref()
            .is_some_and(|s| crate::report::wire::encode_spec(&s.wire_spec()).is_err())
        {
            failure = Some(StartFailure {
                reason: NotStartedReason::SpecTooLarge,
                api: None,
                os_error: None,
                invalid_limit: None,
            });
        }
        // Specs enter the queue at admission, before process creation. Split
        // the fixed allowance so queued requests own independent reservations.
        let spec_metadata = metadata.as_mut().and_then(|r| r.split(512 * 1024));
        let enqueued = caught(async {
            persistence
                .enqueue(PendingArtifacts {
                    finalized: false,
                    run: run.clone(),
                    execution: plan.index,
                    spec: spec.clone(),
                    record: None,
                    metadata: spec_metadata,
                    writable,
                    owner: self.owner.clone(),
                })
                .await
        })
        .await;
        if enqueued.is_err() {
            record_internal(
                self.cancel,
                Invariant::JobEvidence,
                InternalLocus::Execution { index: plan.index },
            );
        }
        let record = if let Some(failure) = failure {
            Some(Arc::new(ExecutionRecord::NotStarted(failure)))
        } else if let (Some(spec), Some(resources)) = (&spec, resources) {
            // Cancellation after admission is a record, not skipped coverage.
            if self.cancel.lock().intent.is_some() {
                Some(Arc::new(ExecutionRecord::NotStarted(StartFailure {
                    reason: NotStartedReason::CancelledBeforeStart,
                    api: None,
                    os_error: None,
                    invalid_limit: None,
                })))
            } else {
                self.events.offer(EventPayload::RunStarted {
                    run: run.clone(),
                    execution: plan.index,
                    stage: plan.stage,
                    role: plan.role.clone(),
                    digest: spec.digest,
                });
                caught(async {
                    executor
                        .execute(
                            spec.clone(),
                            self.cancel.token(),
                            RunEvents {
                                events: self.events.clone(),
                                run: run.clone(),
                                output: self.output_events,
                            },
                            resources,
                        )
                        .await
                })
                .await
                .ok()
                .map(Arc::new)
            }
        } else {
            None
        };
        if matches!(plan.role, Role::Test { .. }) {
            if let Some(ExecutionRecord::Started(root)) = record.as_deref() {
                let bytes = match &root.output_file {
                    Some(report::OutputFile::Collected { bytes, .. }) => Some(*bytes),
                    None => root.stdout.as_ref().map(|capture| capture.stored),
                    _ => None,
                };
                if let Some(bytes) = bytes {
                    self.frozen_outputs.borrow_mut().insert(plan.index, bytes);
                }
            }
        }
        let writable = writable && metadata.is_some();
        #[cfg(feature = "storage-test-hooks")]
        self.point(Point::BeforeRecordEnqueue(plan.index));
        // Enqueue finalized evidence before deriving anything or comparing.
        let enqueued = caught(async {
            persistence
                .enqueue(PendingArtifacts {
                    finalized: true,
                    run: run.clone(),
                    execution: plan.index,
                    spec: spec.clone(),
                    record: record.clone(),
                    metadata,
                    writable,
                    owner: self.owner.clone(),
                })
                .await
        })
        .await;
        if enqueued.is_err() {
            record_internal(
                self.cancel,
                Invariant::JobEvidence,
                InternalLocus::Execution { index: plan.index },
            );
        }
        let mut result = ExecutionResult {
            coverage: Coverage::Admitted,
            class: None,
            invariant: Some(Invariant::RecordMissing),
        };
        let mut summary = None;
        // Extract judge evidence before removing staged scratch and measuring cwd.
        let judge = if matches!(plan.role, Role::Checker { .. } | Role::Validator { .. }) {
            judge_files::finish(
                self.owner.clone(),
                record.clone(),
                scratch,
                &self.cwd_walker,
                executor,
                self.testlib.get(&plan.index).map(|scoring| {
                    Box::new(judge_files::ReportRequest {
                        directory: directory.clone(),
                        budget: self.budget.clone(),
                        scoring: *scoring,
                        policies,
                    })
                }),
            )
            .await
        } else {
            judge_files::Finished {
                evidence: None,
                stderr: None,
                kept_bytes: 0,
                kept: false,
                cwd: Ok(()),
            }
        };
        if let Some(record) = &record {
            // An invalid-limit refusal derives no summary and keeps its cwd/.
            let invalid_limit = matches!(
                record.as_ref(),
                ExecutionRecord::NotStarted(failure) if failure.invalid_limit.is_some()
            );
            let cwd_result = if let Err(note) = judge.cwd {
                // A scratch worker that did not finish may still own cwd
                // paths; never start a competing measurement or removal.
                Err(note)
            } else if let Some(limit) = self.cwd_bytes.filter(|_| {
                plan.stage != Stage::Build
                    && !invalid_limit
                    && spec.as_ref().is_some_and(|s| !s.resolved.shared_cwd)
            }) {
                cwd::finish(
                    self.owner.clone(),
                    directory.join("cwd"),
                    limit,
                    judge.kept_bytes,
                    self.cancel.token(),
                    &self.cwd_walker,
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    io_state.clone(),
                    #[cfg(feature = "storage-test-hooks")]
                    self.observer.cloned(),
                    #[cfg(feature = "storage-test-hooks")]
                    plan.index,
                )
                .await
            } else {
                Ok(None)
            };
            result.coverage = match record.as_ref() {
                ExecutionRecord::Started(_) => Coverage::Started,
                ExecutionRecord::Transferred => Coverage::Transferred,
                ExecutionRecord::NotStarted(failure) => Coverage::NotStarted {
                    reason: failure.reason,
                },
            };
            result.invariant = None;
            let derived = catch_unwind(AssertUnwindSafe(|| -> Result<(), Invariant> {
                #[cfg(any(test, feature = "storage-test-hooks"))]
                self.point(Point::BeforeClass(plan.index));
                if matches!(record.as_ref(), ExecutionRecord::NotStarted(failure) if failure.invalid_limit.is_some())
                {
                    return Err(Invariant::SummaryUnderivable);
                }
                let mut input = record.input(
                    matches!(plan.role, Role::Test { .. })
                        && io.is_none_or(|io| io.output.is_none()),
                    policies,
                );
                input.cwd_overflow_bytes = cwd_result.ok().flatten();
                let mut notes = report::run_notes(&input);
                if let Err(note) = cwd_result {
                    notes.push(note);
                }
                result.class = Some(report::classify_run(&input));
                #[cfg(any(test, feature = "storage-test-hooks"))]
                self.point(Point::BeforeSummary(plan.index));
                summary = Some(Arc::new(RunSummary {
                    observation: summary_observation(record, self.dir, admitted_at.elapsed()),
                    class: result.class.clone().expect("derived class"),
                    record: record.clone(),
                    deadline: spec.as_ref().map_or(Duration::ZERO, |s| {
                        Duration::from_millis(s.resolved.deadline.milliseconds.value)
                    }),
                    notes,
                    cwd_overflow_bytes: input.cwd_overflow_bytes,
                }));
                Ok(())
            }));
            if !matches!(derived, Ok(Ok(()))) {
                result.invariant = Some(Invariant::SummaryUnderivable);
            }
        }
        if let Some(summary) = &summary {
            self.events.offer(EventPayload::RunFinished {
                run: run.clone(),
                summary: summary.clone(),
            });
        }
        ExecutionReport {
            io_warnings: {
                #[cfg(any(
                    target_os = "linux",
                    target_os = "macos",
                    all(test, feature = "storage-test-hooks")
                ))]
                {
                    io_state
                        .as_ref()
                        .map_or_else(Vec::new, |state| state.warnings())
                }
                #[cfg(not(any(
                    target_os = "linux",
                    target_os = "macos",
                    all(test, feature = "storage-test-hooks")
                )))]
                {
                    Vec::new()
                }
            },
            judge_stderr: judge.stderr,
            judge_evidence: judge.evidence,
            judge_scratch_kept: judge.kept,
            plan,
            run: Some(run),
            program: spec.as_ref().map(|s| s.command.program.clone()),
            // Report §4.5: `spec` is null only for skipped executions and for
            // cancellation that interrupted run preparation (E §3.4).
            spec: (!interrupted).then(|| ArtifactRef {
                digest: spec.as_ref().and_then(|s| s.digest),
                ..ArtifactRef::memory()
            }),
            record: record.as_ref().map(|_| ArtifactRef::memory()),
            summary,
            result,
            evidence: record,
        }
    }

    fn output_file(&self, execution: &ExecutionReport) -> Option<report::OutputFile> {
        self.file_io.and_then(|io| io.output.as_ref())?;
        Some(match execution.evidence.as_deref() {
            Some(ExecutionRecord::Started(root)) => root
                .output_file
                .clone()
                .unwrap_or(report::OutputFile::NotAttempted),
            _ => report::OutputFile::NotAttempted,
        })
    }
    fn test_result(&self, execution: &ExecutionReport) -> ExecutionResult {
        let mut result = execution.result.clone();
        if let (Some(class), Some(output)) = (&result.class, self.output_file(execution)) {
            result.class = Some(report::file_test_class(class, &output));
        }
        result
    }

    fn check(
        &self,
        execution: &ExecutionReport,
        answer: &FrozenAnswer,
        comparator: &Comparator,
    ) -> report::CheckOutcome {
        let (admission, comparison) = self.comparison(execution, answer, comparator);
        catch_unwind(AssertUnwindSafe(|| {
            let _state = self.cancel.lock();
            if let Some(output) = self.output_file(execution) {
                return report::file_test_outcome(
                    &execution.result,
                    admission,
                    matches!(answer, FrozenAnswer::Present { .. }),
                    &comparison,
                    &output,
                )
                .1;
            }
            report::check_outcome(
                &execution.result,
                admission,
                matches!(answer, FrozenAnswer::Present { .. }),
                &comparison,
            )
        }))
        .unwrap_or_else(|_| report::CheckOutcome {
            kind: report::CheckKind::InternalError {
                invariant: Invariant::CheckEvidence,
            },
            diagnostic: matches!(execution.result.coverage, Coverage::Started)
                && execution
                    .result
                    .class
                    .as_ref()
                    .is_some_and(RunClass::diagnostic)
                && admission != CheckAdmission::IntentBeforeAdmission,
        })
    }

    fn comparison(
        &self,
        execution: &ExecutionReport,
        answer: &FrozenAnswer,
        comparator: &Comparator,
    ) -> (CheckAdmission, Comparison) {
        let mut admission = CheckAdmission::NoIntent;
        let comparison = catch_unwind(AssertUnwindSafe(|| {
            if self.file_io.is_some() {
                crate::storage::crash::barrier("file_io.before_check");
            }
            #[cfg(any(test, feature = "storage-test-hooks"))]
            self.point(Point::BeforeCheck(execution.plan.index));
            admission = if self.cancel.lock().intent.is_some() {
                CheckAdmission::IntentBeforeAdmission
            } else {
                CheckAdmission::NoIntent
            };
            #[cfg(any(test, feature = "storage-test-hooks"))]
            self.point(Point::AfterCheckAdmission(execution.plan.index));
            // Pure derivation decides eligibility, including orphan inputs.
            let probe =
                report::check_outcome(&execution.result, admission, false, &Comparison::NotRun);
            if !matches!(probe.kind, report::CheckKind::Unchecked) {
                return Comparison::NotRun;
            }
            if self
                .output_file(execution)
                .is_some_and(|o| !matches!(o, report::OutputFile::Collected { .. }))
            {
                return Comparison::NotRun;
            }
            let FrozenAnswer::Present { path, .. } = answer else {
                return Comparison::NotRun;
            };
            let read = || -> std::io::Result<_> {
                let answer = fs::read(path)?;
                let (output, complete) =
                    if let Some(report::OutputFile::Collected { path, .. }) =
                        self.output_file(execution)
                    {
                        let path = path
                            .split('/')
                            .fold(self.dir.to_path_buf(), |p, c| p.join(c));
                        (fs::read(path)?, true)
                    } else {
                        let capture = execution
                            .evidence
                            .as_deref()
                            .and_then(ExecutionRecord::stdout)
                            .ok_or_else(|| std::io::Error::other("missing stdout capture"))?;
                        (
                            match &capture.sink {
                                Some(path) => fs::read(path)?,
                                None => capture.retained.clone(),
                            },
                            capture.complete(),
                        )
                    };
                Ok(crate::compare::check(
                    comparator,
                    &output,
                    Some(&answer),
                    complete,
                ))
            };
            match read() {
                Ok(outcome) => Comparison::Outcome(outcome),
                Err(error) => Comparison::IoError {
                    os_error: error.raw_os_error(),
                },
            }
        }))
        .unwrap_or(Comparison::InternalError {
            invariant: Invariant::CheckEvidence,
        });
        (admission, comparison)
    }
}

fn skipped(plan: PlannedExecution, reason: SkipReason) -> ExecutionReport {
    ExecutionReport {
        io_warnings: Vec::new(),
        judge_stderr: None,
        judge_evidence: None,
        judge_scratch_kept: false,
        plan,
        run: None,
        program: None,
        spec: None,
        record: None,
        summary: None,
        evidence: None,
        result: ExecutionResult {
            coverage: Coverage::Skipped { reason },
            class: None,
            invariant: None,
        },
    }
}
fn reserve(budget: &Arc<JobBudget>, streams: u64) -> Option<(Reservation, Reservation)> {
    streams.checked_add(METADATA)?;
    let streams = budget.reserve(streams).ok()?;
    let metadata = budget.reserve(METADATA).ok()?;
    Some((streams, metadata))
}
fn eligible(verdict: JobVerdict) -> bool {
    !matches!(
        verdict,
        JobVerdict::Cancelled | JobVerdict::Infrastructure | JobVerdict::InternalError
    )
}
fn record_internal(cancel: &CancelState, invariant: Invariant, locus: InternalLocus) {
    let mut state = cancel.lock();
    if state.internal.is_none() {
        state.internal = Some(InternalFailure { invariant, locus });
    }
}
pub(super) fn exceed(policy: CapPolicy) -> crate::supervisor::Exceed {
    match policy {
        CapPolicy::Kill => crate::supervisor::Exceed::Kill,
        CapPolicy::Discard => crate::supervisor::Exceed::Discard,
    }
}
async fn caught<T>(future: impl Future<Output = T>) -> Result<T, ()> {
    let mut future = Box::pin(future);
    poll_fn(
        move |cx| match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(_) => Poll::Ready(Err(())),
        },
    )
    .await
}
/// Count the entries the plan can serialize, including skipped commands. A
/// `cmd` job opens a `tests[]` entry for each run of consecutive commands
/// on the same test (see `run`), so runs are counted, not distinct ids.
fn report_populations(kind: JobKind, plan: &[PlannedExecution], selected: usize) -> (usize, usize) {
    let tests = match kind {
        JobKind::Test => selected,
        JobKind::Cmd => {
            let mut previous = None;
            let mut runs = 0;
            for id in plan
                .iter()
                .filter_map(|v| v.test.as_ref().map(|v| v.id.as_str()))
            {
                if previous != Some(id) {
                    runs += 1;
                    previous = Some(id);
                }
            }
            runs
        }
        _ => 0,
    };
    let aggregates = plan
        .iter()
        .filter_map(|v| match &v.role {
            Role::Command { test, .. } => Some(test),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    (tests, aggregates)
}
fn aggregate(executions: &[ExecutionReport]) -> Vec<Aggregate> {
    let mut groups: Vec<(Aggregate, Vec<Duration>)> = Vec::new();
    for execution in executions {
        let Role::Command { test, .. } = &execution.plan.role else {
            continue;
        };
        let position = groups
            .iter()
            .position(|(group, _)| &group.test == test)
            .unwrap_or_else(|| {
                groups.push((
                    Aggregate {
                        test: test.clone(),
                        iterations: 0,
                        succeeded: 0,
                        wall: None,
                    },
                    Vec::new(),
                ));
                groups.len() - 1
            });
        let (group, walls) = &mut groups[position];
        group.iterations += 1;
        if execution.result.class == Some(RunClass::ExitedZero) {
            group.succeeded += 1;
            if let Some(ExecutionRecord::Started(root)) = execution.evidence.as_deref() {
                if let Some(wall) = root.timing.wall {
                    walls.push(wall);
                }
            }
        }
    }
    groups
        .into_iter()
        .map(|(mut group, mut walls)| {
            walls.sort_unstable();
            if !walls.is_empty() {
                group.wall = Some([
                    walls[0],
                    walls[(walls.len() - 1) / 2],
                    walls[walls.len() - 1],
                ]);
            }
            group
        })
        .collect()
}

fn summary_observation(record: &ExecutionRecord, job: &Path, setup: Duration) -> SummaryEvidence {
    match record {
        ExecutionRecord::Started(root) => SummaryEvidence {
            capture_files: [&root.stdout, &root.stderr].map(|stream| {
                stream
                    .as_ref()
                    .and_then(|capture| capture.sink.as_ref())
                    .and_then(|path| path.strip_prefix(job).ok())
                    .map(Path::to_owned)
            }),
            not_started_timing: None,
        },
        ExecutionRecord::NotStarted(_) | ExecutionRecord::Transferred => SummaryEvidence {
            capture_files: [None, None],
            not_started_timing: Some((setup, "polling")),
        },
    }
}

fn report_details(
    ready: &ReadyExecutionJob<'_>,
    capture: bool,
    comparator: Option<Comparator>,
    kind: JobKind,
) -> ReportDetails {
    let target = ready.target.as_ref().filter(|_| capture);
    ReportDetails {
        shape: None,
        roles: ready.judging.as_ref().map_or_else(Vec::new, |j| {
            j.roles.iter().map(|r| r.report.clone()).collect()
        }),
        problem: ready
            .problem
            .as_ref()
            .zip(ready.report.as_ref())
            .map(|(problem, report)| Problem {
                // Bindings carry the full identity; a bare name is a local problem.
                identity: if problem.id.contains(':') {
                    problem.id.clone()
                } else {
                    format!("local:{}", problem.id)
                },
                name: report.problem_name.clone(),
                kind: report.problem_kind.clone(),
                dir: problem.dir.clone(),
            }),
        target: target.map(|target| {
            let deadline = &target.run.deadline.milliseconds;
            let source = match &deadline.provenance {
                crate::config::Provenance::Cli { .. } => "cli",
                crate::config::Provenance::Derived { .. } => "derived",
                crate::config::Provenance::BuiltIn => "default",
                _ => "configured",
            };
            Target {
                name: ready
                    .report
                    .as_ref()
                    .and_then(|report| report.target_name.clone()),
                language: Some(target.language.value.clone()),
                profile: target.profile.as_ref().map(|p| p.value.clone()),
                build_free: target.build.is_none(),
                compare: comparator,
                check: ready.judging.as_ref().map(|j| j.check.report_config()),
                io: None,
                run_deadline: Some((deadline.value.saturating_mul(1000), source)),
                published_limits: ready
                    .report
                    .as_ref()
                    .and_then(|report| report.published_limits),
                local_limits: configured_limits(&target.run.limits),
            }
        }),
        score: None,
        judge_fingerprint: ready.judging.as_ref().and_then(|j| j.judge_fingerprint),
        fingerprints: None,
        fact: (kind == JobKind::Test).then_some(Fact {
            outcome: FactOutcome::NotAttempted {
                reason: "not_completed",
            },
            warnings: Vec::new(),
        }),
        hooks: Vec::new(),
        hooks_dropped: 0,
        warnings: Vec::new(),
        warnings_dropped: 0,
        pre_delivery_exit: PreDeliveryExit {
            code: 0,
            reraise: None,
        },
        internal_message: "report derivation invariant failed".into(),
    }
}

fn configured_limits(limits: &crate::config::Limits) -> Vec<crate::supervisor::InstalledLimit> {
    use crate::supervisor::{InstalledLimit, LocalLimit, OuterRestriction};
    let mut result = Vec::new();
    for configured in [
        limits
            .address_space
            .as_ref()
            .map(|v| LocalLimit::AddressSpace { bytes: v.value }),
        limits
            .job_commit
            .as_ref()
            .map(|v| LocalLimit::JobCommit { bytes: v.value }),
    ]
    .into_iter()
    .flatten()
    {
        result.push(InstalledLimit {
            configured,
            inherited: None,
            normalised: 0,
            installed: None,
            mechanism: "not_installed",
            outer: OuterRestriction::Unknown,
        });
    }
    result
}

#[cfg(test)]
mod tests;
