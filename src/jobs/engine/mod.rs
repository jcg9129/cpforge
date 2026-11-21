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
//! thread drives the executor; cancellation may arrive from any thread. The
//! verdict lock covers admission, check arbitration, per-test derivation, V
//! and F, and is never held across execution, comparison I/O, callbacks or
//! settlement. Cancellation records intent only; it never assigns a verdict.
//!
//! Event consumers drain [`EventReceiver::recv`] until it returns `None`, then
//! await [`EventReceiver::terminal`]. Sequence numbers start at zero and may
//! have gaps where events were dropped; closure, not `seq`, is the barrier.
//!
//! `JobPersistence` in `jobs::persist` supplies the disk-backed service;
//! construct it from the ready job before passing that job to this engine.
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
mod events;
mod executor;
mod persistence;
mod report_value;
mod spec;
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
use crate::jobs::ReadyJob;
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
    BeforeCheck(u32),
    AfterCheckAdmission(u32),
    BeforeClass(u32),
    BeforeSummary(u32),
    BeforeTestVerdict(u32),
    AfterPrimaryFreeze,
    Settlement,
    Closure,
    BeforeSnapshot,
    AfterSnapshot,
}
#[cfg(any(test, feature = "storage-test-hooks"))]
type Observer = dyn Fn(Point) + Send + Sync;

#[derive(Debug)]
pub struct Engine<'probe> {
    ready: ReadyJob<'probe>,
    plan: Vec<PlannedExecution>,
    kind: JobKind,
    comparator: Option<Comparator>,
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
struct Observation(Arc<Observer>);
#[cfg(any(test, feature = "storage-test-hooks"))]
impl std::fmt::Debug for Observation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Observation")
    }
}

impl<'probe> Engine<'probe> {
    pub fn new(ready: ReadyJob<'probe>, request: JobRequest) -> Result<Self, PlanError> {
        let cancel = CancelState::at(ready.started);
        Self::with_cancellation(ready, request, cancel)
    }

    /// Use the invocation state installed before registration, preserving any
    /// cancellation received during admission or capture.
    pub fn with_cancellation(
        ready: ReadyJob<'probe>,
        request: JobRequest,
        mut cancel: CancelState,
    ) -> Result<Self, PlanError> {
        // Report timing runs from admission (step 1) to F, as in `new`; the
        // shared verdict state and token are those installed earlier.
        cancel.start = ready.started;
        let mut plan = Vec::new();
        let has_build = matches!(&ready.planned_target, PlannedTarget::Build(_));
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
        Ok(Self {
            ready,
            plan,
            kind,
            comparator,
            keep_going,
            output_events: false,
            capture_enabled: needs_build || kind == JobKind::Run,
            cancel,
            events: Events::new(),
            progress: None,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            observer: None,
        })
    }

    /// Attach the invocation's stderr queue before running. It is closed at E.
    pub fn progress(mut self, progress: crate::progress::ProgressQueue) -> Self {
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
            ready,
            plan,
            kind,
            comparator,
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
        let mut target = TargetState {
            pending: None,
            free: None,
            validated: None,
        };
        match ready.planned_target {
            PlannedTarget::Build(pending) => target.pending = Some(pending),
            PlannedTarget::BuildFree(free) => target.free = Some(free),
        }
        let context = Context {
            dir: &ready.dir,
            owner: &ready.owner,
            budget: &ready.budget,
            platform: ready.platform,
            cancel: &cancel,
            events: &events,
            output_events,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            observer: observer.as_ref(),
        };
        events.offer(EventPayload::JobStarted {
            job: ready.id.clone(),
            kind,
            sequence: ready.sequence.clone(),
        });
        let mut executions = Vec::with_capacity(plan.len());
        let mut tests = Vec::new();
        let mut build = None;
        let mut admission_closed = false;
        let mut stopped = false;
        // Reserve report + replacement temporary before admitting any run.
        let report_bytes = (64 * 1024 + 12 * 1024 * plan.len() as u64) * 2;
        let report_reservation = ready.budget.reserve(report_bytes).ok();
        let report_refused = report_reservation.is_none();
        let planned_count = plan.len() as u32;
        for planned in plan {
            let earlier_retention_refusal = admission_closed;
            #[cfg(any(test, feature = "storage-test-hooks"))]
            context.phase_point(Point::BeforeAdmission(planned.index));
            let skip = if admission_closed {
                Some(SkipReason::AdmissionClosed)
            } else if planned.depends_on_build
                && !matches!(
                    build.as_ref().map(|b: &BuildReport| &b.result),
                    Some(BuildResult::Succeeded)
                )
            {
                Some(SkipReason::BuildNotSucceeded)
            } else if stopped {
                Some(SkipReason::StopPolicy)
            } else {
                None
            };
            let mut execution = if let Some(reason) = skip {
                skipped(planned, reason)
            } else {
                context
                    .execute(
                        planned,
                        &mut target,
                        executor,
                        persistence,
                        report_refused,
                        &mut admission_closed,
                    )
                    .await
            };
            execution.result = report::validate_execution(
                execution.result,
                &report::SkipConditions {
                    job: kind,
                    depends_on_unsucceeded_build: execution.plan.depends_on_build
                        && !matches!(
                            build.as_ref().map(|b| &b.result),
                            Some(BuildResult::Succeeded)
                        ),
                    earlier_retention_refusal,
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
                let mut post = PostBuild::default();
                let pending = target.pending.take().expect("one planned build");
                let id = execution.run.as_ref().map(|_| pending.id().clone());
                let root = id.as_ref().map(|_| pending.output_root().to_owned());
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
                if let Some(ExecutionRecord::Started(outcome)) = execution.evidence.as_deref() {
                    match catch_unwind(AssertUnwindSafe(|| {
                        build::finish_build(pending, build::BuildExecutionOutcome::from(&**outcome))
                    })) {
                        Ok(validation) => {
                            post = PostBuild::from_validation(validation.as_ref());
                            target.validated = validation.ok();
                        }
                        Err(_) => post.invariant = Some(Invariant::BuildEvidence),
                    }
                }
                let result = report::build_result(&execution.result, &post);
                if let BuildResult::InternalError { invariant } = result {
                    record_internal(&cancel, invariant, InternalLocus::Build);
                }
                let succeeded = result == BuildResult::Succeeded;
                build = Some(BuildReport {
                    execution: execution.plan.index,
                    build_id: id,
                    output_root: root,
                    result,
                    artifacts: if succeeded { artifacts } else { Vec::new() },
                });
            }
            if let Some(test) = execution
                .plan
                .test
                .as_ref()
                .filter(|_| kind == JobKind::Test)
            {
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
                    report::test_verdict(&execution.result, &check)
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
                    class: execution.result.class.clone(),
                    check: Some(check),
                    verdict: Some(verdict),
                };
                if let Some(progress) = &progress {
                    let context = report::DisplayContext {
                        comparator: comparator.as_ref().map(report::comparator_token),
                        wall_deadline: execution.summary.as_ref().map(|s| s.deadline),
                        measured_wall: execution.evidence.as_deref().and_then(|e| match e {
                            ExecutionRecord::Started(root) => root.timing.wall,
                            ExecutionRecord::NotStarted(_) => None,
                        }),
                        budgets: vec![
                            (report::Stream::Stdout, ready.target.run.stdout.bytes.value),
                            (report::Stream::Stderr, ready.target.run.stderr.bytes.value),
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
                    progress.offer(&format!(
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
            executions.push(execution);
        }
        if let Some(warning) = warnings::cleanup(&executions) {
            warnings.push(warning);
        }
        let primary: Vec<_> = executions.iter().map(|e| e.result.clone()).collect();
        let test_verdicts: Vec<_> = tests.iter().filter_map(|t| t.verdict.clone()).collect();
        let mut result = {
            let mut state = cancel.lock();
            let result = report::job_verdict(&report::JobInput {
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
            cancel.lock().phase = CancelPhase::Settlement;
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
        let counts = report::coverage_counts(&primary, &[], &test_verdicts);
        let aggregates = if kind == JobKind::Cmd {
            aggregate(&executions)
        } else {
            Vec::new()
        };
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
            if cancellation.is_some_and(|c| c.phase != CancelPhase::Primary) {
                warnings.push(Warning::LateCancellation);
            }
            let warning_slots = 24_usize.saturating_sub(details.warnings.len());
            details.warnings_dropped = warnings.len().saturating_sub(warning_slots) as u32;
            warnings.truncate(warning_slots);
            details.pre_delivery_exit = pre_delivery_exit(kind, result.verdict, cancellation);
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
                    target: capture_enabled.then_some(ready.target),
                    comparator,
                    capture: capture_enabled.then_some(ready.capture_report),
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
    pending: Option<build::PendingBuild<'probe>>,
    free: Option<build::PreflightedBuildFree>,
    validated: Option<ValidatedBuild>,
}
struct Context<'a> {
    dir: &'a Path,
    owner: &'a JobOwner,
    budget: &'a Arc<JobBudget>,
    platform: Platform,
    cancel: &'a CancelState,
    events: &'a Events,
    output_events: bool,
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
        let directory = self.dir.join(run.as_str());
        let mut failure = None;
        let mut writable = true;
        // Do not inspect, write or remove any part of a colliding run tree.
        for path in [&directory, &directory.join("cwd"), &directory.join("out")] {
            if let Err(error) = fs::create_dir(path) {
                writable = false;
                failure = Some(StartFailure {
                    reason: NotStartedReason::Setup,
                    api: None,
                    os_error: error.raw_os_error(),
                    invalid_limit: None,
                });
                break;
            }
        }
        let stage = catch_unwind(AssertUnwindSafe(|| -> Result<_, ()> {
            if plan.stage == Stage::Build {
                target
                    .pending
                    .as_ref()
                    .ok_or(())?
                    .stage_for_cwd(directory.join("cwd"))
                    .map_err(|_| ())
            } else {
                match &plan.recipe {
                    Recipe::Resolved(stage) => {
                        let mut stage = stage.as_ref().clone();
                        if !stage.shared_cwd {
                            stage.cwd = directory.join("cwd");
                        }
                        Ok(stage)
                    }
                    Recipe::TargetRun => {
                        if let Some(build) = &target.validated {
                            build.run(directory.join("cwd")).map_err(|_| ())
                        } else {
                            target
                                .free
                                .as_ref()
                                .ok_or(())?
                                .run(directory.join("cwd"))
                                .map_err(|_| ())
                        }
                    }
                }
            }
        }))
        .ok()
        .and_then(Result::ok);
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
                stage
                    .as_ref()
                    .and_then(|s| s.stdout.bytes.value.checked_add(s.stderr.bytes.value))
            };
            let reservations = if report_refused {
                None
            } else {
                amount.and_then(|amount| reserve(self.budget, amount))
            };
            if let Some((streams, meta)) = reservations {
                metadata = Some(meta);
                resources = Some(RunResources {
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
                resolved,
                stdin: plan.stdin.clone(),
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
        let enqueued = catch_unwind(AssertUnwindSafe(|| {
            persistence.enqueue(PendingArtifacts {
                finalized: false,
                run: run.clone(),
                execution: plan.index,
                spec: spec.clone(),
                record: None,
                metadata: spec_metadata,
                writable,
                owner: self.owner.clone(),
            })
        }));
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
        let writable = writable && metadata.is_some();
        // Enqueue finalized evidence before deriving anything or comparing.
        let enqueued = catch_unwind(AssertUnwindSafe(|| {
            persistence.enqueue(PendingArtifacts {
                finalized: true,
                run: run.clone(),
                execution: plan.index,
                spec: spec.clone(),
                record: record.clone(),
                metadata,
                writable,
                owner: self.owner.clone(),
            })
        }));
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
        if let Some(record) = &record {
            result.coverage = match record.as_ref() {
                ExecutionRecord::Started(_) => Coverage::Started,
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
                let input = record.input(matches!(plan.role, Role::Test { .. }), policies);
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
                    notes: report::run_notes(&input),
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
            plan,
            run: Some(run),
            program: spec.as_ref().map(|s| s.command.program.clone()),
            // Report §4.5: `spec` is null only for skipped executions.
            spec: Some(ArtifactRef {
                digest: spec.as_ref().and_then(|s| s.digest),
                ..ArtifactRef::memory()
            }),
            record: record.as_ref().map(|_| ArtifactRef::memory()),
            summary,
            result,
            evidence: record,
        }
    }

    fn check(
        &self,
        execution: &ExecutionReport,
        answer: &FrozenAnswer,
        comparator: &Comparator,
    ) -> report::CheckOutcome {
        let mut admission = CheckAdmission::NoIntent;
        let comparison = catch_unwind(AssertUnwindSafe(|| {
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
            let FrozenAnswer::Present { path, .. } = answer else {
                return Comparison::NotRun;
            };
            let read = || -> std::io::Result<_> {
                let answer = fs::read(path)?;
                let capture = execution
                    .evidence
                    .as_deref()
                    .and_then(ExecutionRecord::stdout)
                    .ok_or_else(|| std::io::Error::other("missing stdout capture"))?;
                let output = match &capture.sink {
                    Some(path) => fs::read(path)?,
                    None => capture.retained.clone(),
                };
                Ok(crate::compare::check(
                    comparator,
                    &output,
                    Some(&answer),
                    capture.complete(),
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
        catch_unwind(AssertUnwindSafe(|| {
            let _state = self.cancel.lock();
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
}

fn skipped(plan: PlannedExecution, reason: SkipReason) -> ExecutionReport {
    ExecutionReport {
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
        ExecutionRecord::NotStarted(_) => SummaryEvidence {
            capture_files: [None, None],
            not_started_timing: Some((setup, "polling")),
        },
    }
}

fn report_details(
    ready: &ReadyJob<'_>,
    capture: bool,
    comparator: Option<Comparator>,
    kind: JobKind,
) -> ReportDetails {
    let id = ready
        .problem
        .id
        .strip_prefix("local:")
        .unwrap_or(&ready.problem.id);
    let deadline = &ready.target.run.deadline.milliseconds;
    let source = match &deadline.provenance {
        crate::config::Provenance::Cli { .. } => "cli",
        crate::config::Provenance::Derived { .. } => "derived",
        crate::config::Provenance::BuiltIn => "default",
        _ => "configured",
    };
    ReportDetails {
        problem: Problem {
            identity: format!("local:{id}"),
            name: ready.report.problem_name.clone(),
            kind: ready.report.problem_kind.clone(),
            dir: ready.problem.dir.clone(),
        },
        target: capture.then(|| Target {
            name: ready.report.target_name.clone(),
            language: Some(ready.target.language.value.clone()),
            profile: ready.target.profile.as_ref().map(|p| p.value.clone()),
            build_free: ready.target.build.is_none(),
            compare: comparator,
            run_deadline: Some((deadline.value.saturating_mul(1000), source)),
            published_limits: ready.report.published_limits,
            local_limits: configured_limits(&ready.target.run.limits),
        }),
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
