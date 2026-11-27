//! Streaming stress admission and bounded per-iteration retirement.
//!
//! A planner supplies a fixed set of frozen batch templates. The caller lends
//! this controller to one live iteration, finalizes each admitted run before
//! requesting the next, and acknowledges retirement only after settlement,
//! eviction and confirmed scratch removal. Dropping either lease early closes
//! admission and records an internal failure instead of silently admitting an
//! overlapping iteration.
//!
//! The admission controller delegates retirement I/O and judging outcomes. In
//! particular, `complete` is an acknowledgment of retirement, not a substitute
//! for waiting for resources released or removing and accounting for files.
//! `RetirementBatch` owns those steps; batch classification consumes finalized
//! judging observations under the controller's verdict lock.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod classification;
mod engine;
mod report;
pub(crate) use engine::IterationExecution;
pub use engine::StressRequest;
pub use report::*;
mod retirement;
pub use classification::*;
pub use retirement::*;

use super::CancelState;
use crate::report::{InternalFailure, InternalLocus, Invariant};

const BATCH_RUNS: usize = 5;
const CLASSES: usize = 8;
const SAVE_ATTEMPTS: u64 = 20;

/// Tokens and order from stress §3.1; these never consume a report run id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchRole {
    Generator,
    Validator,
    Brute,
    Candidate,
    Checker,
}

impl BatchRole {
    pub const ALL: [Self; BATCH_RUNS] = [
        Self::Generator,
        Self::Validator,
        Self::Brute,
        Self::Candidate,
        Self::Checker,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::Generator => "generator",
            Self::Validator => "validator",
            Self::Brute => "brute",
            Self::Candidate => "candidate",
            Self::Checker => "checker",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Generator => 0,
            Self::Validator => 1,
            Self::Brute => 2,
            Self::Candidate => 3,
            Self::Checker => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IterationRunId {
    pub iteration: u64,
    pub role: BatchRole,
}

impl fmt::Display for IterationRunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.iteration, self.role.token())
    }
}

impl IterationRunId {
    pub fn directory(self, job: &Path) -> PathBuf {
        job.join("iterations").join(self.to_string())
    }
}

/// A validated, immutable run template and its maximum retained stream bytes.
/// Large resolved specifications should be boxed in `T` on job paths.
#[derive(Clone, Debug)]
pub struct RunTemplate<T> {
    value: T,
    capture_bound: u64,
}

impl<T> RunTemplate<T> {
    pub fn new(value: T, stdout_cap: u64, stderr_cap: u64) -> Result<Self, AdmissionError> {
        Ok(Self {
            value,
            capture_bound: stdout_cap
                .checked_add(stderr_cap)
                .ok_or(AdmissionError::CaptureBound)?,
        })
    }
}

/// Fixed templates, never an iteration plan. Eligibility is supplied after
/// finalized evidence has been judged, including validation and answer checks.
#[derive(Debug)]
pub struct BatchTemplates<T> {
    roles: [Option<RunTemplate<T>>; BATCH_RUNS],
    runs: u64,
    captured_bytes: u64,
}

impl<T> BatchTemplates<T> {
    pub fn new(
        generator: RunTemplate<T>,
        validator: Option<RunTemplate<T>>,
        brute: Option<RunTemplate<T>>,
        candidate: RunTemplate<T>,
        checker: Option<RunTemplate<T>>,
    ) -> Result<Self, AdmissionError> {
        let roles = [Some(generator), validator, brute, Some(candidate), checker];
        let mut captured_bytes = 0u64;
        let mut runs = 0;
        for role in roles.iter().flatten() {
            captured_bytes = captured_bytes
                .checked_add(role.capture_bound)
                .ok_or(AdmissionError::CaptureBound)?;
            runs += 1;
        }
        Ok(Self {
            roles,
            runs,
            captured_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IterationClass {
    Passed,
    GeneratorFailure,
    ValidatorFailure,
    OracleFailure,
    JudgeFailure,
    CandidateFailure,
    Infrastructure,
    Cancelled,
}

impl IterationClass {
    fn index(self) -> usize {
        match self {
            Self::Passed => 0,
            Self::GeneratorFailure => 1,
            Self::ValidatorFailure => 2,
            Self::OracleFailure => 3,
            Self::JudgeFailure => 4,
            Self::CandidateFailure => 5,
            Self::Infrastructure => 6,
            Self::Cancelled => 7,
        }
    }

    pub fn failing(self) -> bool {
        matches!(
            self,
            Self::GeneratorFailure
                | Self::ValidatorFailure
                | Self::OracleFailure
                | Self::JudgeFailure
                | Self::CandidateFailure
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    Cancelled,
    BuildFailed,
    Failure,
    Infrastructure,
    AdmissionClosed,
    CleanupFailed,
    Iterations,
    TimeBudget,
    CounterLimit,
    Replay,
}

impl StopReason {
    pub fn token(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::BuildFailed => "build_failed",
            Self::Failure => "failure",
            Self::Infrastructure => "infrastructure",
            Self::AdmissionClosed => "admission_closed",
            Self::CleanupFailed => "cleanup_failed",
            Self::Iterations => "iterations",
            Self::TimeBudget => "time_budget",
            Self::CounterLimit => "counter_limit",
            Self::Replay => "replay",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stop {
    pub reason: StopReason,
    pub iteration: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchBudget {
    pub iterations: u64,
    pub time: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Schedule {
    Search {
        start_seed: u64,
        budget: SearchBudget,
        keep_going: bool,
    },
    /// The replay index is an identity, independent of the search budget.
    Replay { iteration: u64, seed: u64 },
}

impl Schedule {
    fn validate(self) -> Result<(), AdmissionError> {
        if let Self::Search {
            start_seed, budget, ..
        } = self
        {
            let last = budget
                .iterations
                .checked_sub(1)
                .ok_or(AdmissionError::IterationBudget)?;
            if !(Duration::from_millis(1)..=Duration::from_secs(24 * 60 * 60))
                .contains(&budget.time)
            {
                return Err(AdmissionError::TimeBudget);
            }
            start_seed
                .checked_add(last)
                .ok_or(AdmissionError::SeedRange)?;
        }
        Ok(())
    }

    fn identity(self, admitted: u64) -> (u64, u64) {
        match self {
            Self::Search { start_seed, .. } => (
                admitted,
                start_seed
                    .checked_add(admitted)
                    .expect("validated seed range"),
            ),
            Self::Replay { iteration, seed } => (iteration, seed),
        }
    }
}

/// `stress.clock`: time is observed only at iteration admission, and never
/// interrupts a live iteration. Implementations must be monotonic.
pub trait StressClock: fmt::Debug + Send + Sync {
    fn now(&self) -> Duration;
}

#[derive(Debug)]
pub struct MonotonicClock(Instant);

impl Default for MonotonicClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl StressClock for MonotonicClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StressCounters {
    pub admitted: u64,
    pub retired: u64,
    pub runs_admitted: u64,
    pub runs_started: u64,
    pub captured_bytes: u64,
    pub classes: [u64; CLASSES],
    pub failing: u64,
    pub saved: u64,
    pub promoted: u64,
    pub not_saved: u64,
    pub failures_dropped: u64,
}

impl StressCounters {
    /// Reserve arithmetic headroom for every possible outcome of the entire
    /// next iteration before its first run. Actual increments remain exact.
    fn fits<T>(&self, templates: &BatchTemplates<T>) -> bool {
        [
            self.admitted,
            self.retired,
            self.failing,
            self.saved,
            self.promoted,
            self.not_saved,
            self.failures_dropped,
        ]
        .into_iter()
        .chain(self.classes)
        .all(|value| value.checked_add(1).is_some())
            && self.runs_admitted.checked_add(templates.runs).is_some()
            && self.runs_started.checked_add(templates.runs).is_some()
            && self
                .captured_bytes
                .checked_add(templates.captured_bytes)
                .is_some()
    }
}

/// Admission observations are outside the verdict lock so a harness can set
/// cancellation at exactly the contract's admission points.
#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionPoint {
    IterationAdmit(u64),
    RunAdmit(IterationRunId),
}

#[cfg(any(test, feature = "storage-test-hooks"))]
type Observer = Box<dyn Fn(AdmissionPoint) + Send + Sync>;

pub struct StressAdmission<T> {
    templates: Box<BatchTemplates<T>>,
    schedule: Schedule,
    cancel: CancelState,
    clock: Box<dyn StressClock>,
    first_check: Option<Duration>,
    builds_succeeded: bool,
    previous_stop: Option<StopReason>,
    stop: Option<Stop>,
    counters: StressCounters,
    live: bool,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    observer: Option<Observer>,
}

impl<T: fmt::Debug> fmt::Debug for StressAdmission<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StressAdmission")
            .field("templates", &self.templates)
            .field("schedule", &self.schedule)
            .field("counters", &self.counters)
            .field("live", &self.live)
            .field("stop", &self.stop)
            .finish_non_exhaustive()
    }
}

impl<T: Clone> StressAdmission<T> {
    pub fn new(
        templates: BatchTemplates<T>,
        schedule: Schedule,
        cancel: CancelState,
        builds_succeeded: bool,
        clock: Box<dyn StressClock>,
    ) -> Result<Self, AdmissionError> {
        schedule.validate()?;
        Ok(Self {
            templates: Box::new(templates),
            schedule,
            cancel,
            clock,
            first_check: None,
            builds_succeeded,
            previous_stop: None,
            stop: None,
            counters: StressCounters::default(),
            live: false,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            observer: None,
        })
    }

    pub fn counters(&self) -> StressCounters {
        self.counters
    }

    pub fn stop(&self) -> Option<Stop> {
        self.stop
    }

    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub fn observe(&mut self, observer: impl Fn(AdmissionPoint) + Send + Sync + 'static) {
        self.observer = Some(Box::new(observer));
    }

    /// Route stress admission through the engine's shared observation mechanism.
    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub fn observe_engine(&mut self, observer: impl Fn(super::Point) + Send + Sync + 'static) {
        let observation = super::Observation(std::sync::Arc::new(observer));
        self.observe(move |point| {
            (observation.0)(match point {
                AdmissionPoint::IterationAdmit(k) => super::Point::StressIterationAdmit(k),
                AdmissionPoint::RunAdmit(id) => super::Point::StressRunAdmit(id),
            });
        });
    }

    /// `stress.counters`: inject initial arithmetic state before any admission.
    /// Search jobs only: a replay checks no counter row (stress §5 row 7).
    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub fn set_counters(&mut self, counters: StressCounters) -> Result<(), AdmissionError> {
        if self.first_check.is_some()
            || self.live
            || self.stop.is_some()
            || matches!(self.schedule, Schedule::Replay { .. })
        {
            return Err(AdmissionError::State);
        }
        self.counters = counters;
        Ok(())
    }

    /// Only one lease can exist. Its mutable borrow also prevents callers from
    /// changing counters or templates while execution or retirement is live.
    pub fn admit(&mut self) -> Result<LiveIteration<'_, T>, Stop> {
        if let Some(stop) = self.stop {
            return Err(stop);
        }
        let k = match self.schedule {
            Schedule::Search { .. } => self.counters.admitted,
            Schedule::Replay { iteration, .. } => iteration,
        };
        #[cfg(any(test, feature = "storage-test-hooks"))]
        if let Some(observer) = &self.observer {
            observer(AdmissionPoint::IterationAdmit(k));
        }
        // The §5 rows are read under the verdict lock, in order. The time
        // origin t0 is the beginning of the first check, after the injection
        // barrier.
        let mut state = self.cancel.lock();
        let now = self.clock.now();
        let first = self.first_check.is_none();
        let start = *self.first_check.get_or_insert(now);
        // An abandoned lease is never interpreted as retirement: the iteration
        // was not retired, so the job's evidence is incomplete.
        let abandoned = self.live;
        if abandoned && state.internal.is_none() {
            state.internal = Some(InternalFailure {
                invariant: Invariant::JobEvidence,
                locus: InternalLocus::Job,
            });
        }
        let reason = if state.intent.is_some() {
            Some(StopReason::Cancelled)
        } else if first && !self.builds_succeeded {
            Some(StopReason::BuildFailed)
        } else if abandoned {
            Some(StopReason::Infrastructure)
        } else if let Some(previous) = self.previous_stop {
            Some(previous)
        } else {
            match self.schedule {
                Schedule::Search { budget, .. } => {
                    if self.counters.admitted >= budget.iterations {
                        Some(StopReason::Iterations)
                    } else if now.saturating_sub(start) >= budget.time {
                        Some(StopReason::TimeBudget)
                    } else if !self.counters.fits(&self.templates) {
                        Some(StopReason::CounterLimit)
                    } else {
                        None
                    }
                }
                // Row 7 replaces rows 4-6: one admission from zero counters
                // cannot overflow, since template capture bounds are checked.
                Schedule::Replay { .. } => {
                    (self.counters.admitted >= 1).then_some(StopReason::Replay)
                }
            }
        };
        if let Some(reason) = reason {
            let stop = Stop {
                reason,
                iteration: (reason != StopReason::BuildFailed).then_some(k),
            };
            self.stop = Some(stop);
            return Err(stop);
        }
        let (iteration, seed) = self.schedule.identity(self.counters.admitted);
        self.counters.admitted = self.counters.admitted.checked_add(1).expect("headroom");
        self.live = true;
        drop(state);
        Ok(LiveIteration {
            owner: self,
            iteration,
            seed,
            next: 0,
            pending: None,
            cancelled: None,
            retention_refused: false,
        })
    }
}

/// Only the admitted run's template is cloned; absent and ineligible roles
/// allocate no per-iteration specification or directory.
#[derive(Debug)]
pub struct AdmittedRun<T> {
    pub id: IterationRunId,
    pub seed: u64,
    pub template: T,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunGate {
    /// The next declared role is eligible by stress §3.3.
    Continue,
    /// Final evidence decided the iteration; later roles are never admitted.
    Decided,
}

/// Observations from finalized evidence, not from process start notifications.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunCompletion {
    pub started: bool,
    pub captured_bytes: u64,
    pub gate: RunGate,
    pub retention_refused: bool,
}

#[derive(Debug)]
pub struct LiveIteration<'a, T> {
    owner: &'a mut StressAdmission<T>,
    iteration: u64,
    seed: u64,
    next: usize,
    pending: Option<BatchRole>,
    cancelled: Option<BatchRole>,
    retention_refused: bool,
}

impl<'a, T: Clone> LiveIteration<'a, T> {
    pub fn iteration(&self) -> u64 {
        self.iteration
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn cancellation_token(&self) -> crate::supervisor::CancelToken {
        self.owner.cancel.token()
    }

    pub fn admit_run(&mut self) -> Result<Option<AdmittedRun<T>>, AdmissionError> {
        if self.pending.is_some() {
            return Err(AdmissionError::RunPending);
        }
        while self.next < BATCH_RUNS {
            let role = BatchRole::ALL[self.next];
            let Some(template) = &self.owner.templates.roles[self.next] else {
                self.next += 1;
                continue;
            };
            let id = IterationRunId {
                iteration: self.iteration,
                role,
            };
            #[cfg(any(test, feature = "storage-test-hooks"))]
            if let Some(observer) = &self.owner.observer {
                observer(AdmissionPoint::RunAdmit(id));
            }
            let state = self.owner.cancel.lock();
            if state.intent.is_some() {
                // `skipped{cancelled}`: never admitted, so no spec or directory.
                self.cancelled = Some(role);
                self.next = BATCH_RUNS;
                return Ok(None);
            }
            self.owner.counters.runs_admitted = self
                .owner
                .counters
                .runs_admitted
                .checked_add(1)
                .expect("iteration headroom");
            self.pending = Some(role);
            self.next += 1;
            drop(state);
            let template = template.value.clone();
            return Ok(Some(AdmittedRun {
                id,
                seed: self.seed,
                template,
            }));
        }
        Ok(None)
    }

    /// Call after record finalization; resources may still be held. There is
    /// deliberately no release of slots, credits or byte reservations here.
    pub fn record_finalized(&mut self, completion: RunCompletion) -> Result<(), AdmissionError> {
        let role = self.pending.ok_or(AdmissionError::NoRunPending)?;
        let bound = self.owner.templates.roles[role.index()]
            .as_ref()
            .expect("admitted template")
            .capture_bound;
        if completion.captured_bytes > bound
            || (!completion.started && completion.captured_bytes != 0)
            || (completion.retention_refused
                && (completion.started || completion.gate != RunGate::Decided))
        {
            return Err(AdmissionError::CaptureBound);
        }
        let counters = &mut self.owner.counters;
        counters.runs_started = counters
            .runs_started
            .checked_add(u64::from(completion.started))
            .expect("iteration headroom");
        counters.captured_bytes = counters
            .captured_bytes
            .checked_add(completion.captured_bytes)
            .expect("iteration headroom");
        self.retention_refused |= completion.retention_refused;
        self.pending = None;
        if completion.gate == RunGate::Decided {
            self.next = BATCH_RUNS;
        }
        Ok(())
    }

    /// The run whose admission came after intent (stress §6.5), if any; it
    /// is the deciding run of S2 and is reported as `skipped{cancelled}`.
    pub fn cancelled(&self) -> Option<IterationRunId> {
        self.cancelled.map(|role| IterationRunId {
            iteration: self.iteration,
            role,
        })
    }

    /// R2-R4 admission bookkeeping from a class derived from frozen evidence.
    /// Folding and arbitration of cancellation of later roles share the verdict
    /// lock. The returned lease must remain held throughout R5-R11.
    pub fn begin_retirement(
        self,
        class: IterationClass,
    ) -> Result<RetiringIteration<'a, T>, AdmissionError> {
        if self.pending.is_some() || self.next != BATCH_RUNS {
            return Err(AdmissionError::RunPending);
        }
        if self.retention_refused && class != IterationClass::Infrastructure {
            return Err(AdmissionError::State);
        }
        let state = self.owner.cancel.lock();
        let class = if self.cancelled.is_some() && class != IterationClass::Infrastructure {
            IterationClass::Cancelled
        } else {
            class
        };
        let counters = &mut self.owner.counters;
        counters.classes[class.index()] = counters.classes[class.index()]
            .checked_add(1)
            .expect("iteration headroom");
        let earlier_failures = counters.failing;
        if class.failing() {
            counters.failing = counters.failing.checked_add(1).expect("iteration headroom");
        }
        let disposition = if matches!(self.owner.schedule, Schedule::Replay { .. }) {
            Disposition::Keep
        } else if class.failing() {
            if earlier_failures < SAVE_ATTEMPTS {
                Disposition::Save
            } else {
                Disposition::Count
            }
        } else if matches!(
            class,
            IterationClass::Infrastructure | IterationClass::Cancelled
        ) {
            Disposition::Keep
        } else {
            Disposition::Discard
        };
        if disposition == Disposition::Count {
            counters.failures_dropped = counters
                .failures_dropped
                .checked_add(1)
                .expect("iteration headroom");
        }
        self.owner.previous_stop = match class {
            IterationClass::Infrastructure if self.retention_refused => {
                Some(StopReason::AdmissionClosed)
            }
            IterationClass::Infrastructure => Some(StopReason::Infrastructure),
            IterationClass::Cancelled => Some(StopReason::Cancelled),
            class
                if class.failing()
                    && matches!(
                        self.owner.schedule,
                        Schedule::Search {
                            keep_going: false,
                            ..
                        }
                    ) =>
            {
                Some(StopReason::Failure)
            }
            _ => None,
        };
        drop(state);
        Ok(RetiringIteration {
            owner: self.owner,
            iteration: self.iteration,
            seed: self.seed,
            disposition,
            class,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    Save,
    Count,
    Keep,
    Discard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveResult {
    NotSaved,
    Staged,
    Promoted,
}

#[derive(Debug)]
pub struct RetiringIteration<'a, T> {
    owner: &'a mut StressAdmission<T>,
    iteration: u64,
    seed: u64,
    disposition: Disposition,
    class: IterationClass,
}

impl<T> RetiringIteration<'_, T> {
    pub fn iteration(&self) -> u64 {
        self.iteration
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn disposition(&self) -> Disposition {
        self.disposition
    }

    pub fn class(&self) -> IterationClass {
        self.class
    }

    /// R11 acknowledgment. The retirement owner must first settle artefacts,
    /// evict records/specs and either confirm removal and release its bytes or
    /// record cleanup failure and retain the leftovers. A stalled writer must
    /// set `cleanup_failed`; no further iteration will then be admitted.
    pub fn complete(
        self,
        saved: Option<SaveResult>,
        cleanup_failed: bool,
    ) -> Result<(), AdmissionError> {
        if (self.disposition == Disposition::Save) != saved.is_some() {
            return Err(AdmissionError::State);
        }
        let counters = &mut self.owner.counters;
        match saved {
            Some(SaveResult::Staged | SaveResult::Promoted) => {
                counters.saved = counters.saved.checked_add(1).expect("iteration headroom");
                if saved == Some(SaveResult::Promoted) {
                    counters.promoted = counters
                        .promoted
                        .checked_add(1)
                        .expect("iteration headroom");
                }
            }
            Some(SaveResult::NotSaved) => {
                counters.not_saved = counters
                    .not_saved
                    .checked_add(1)
                    .expect("iteration headroom");
            }
            None if self.disposition == Disposition::Count => {
                counters.not_saved = counters
                    .not_saved
                    .checked_add(1)
                    .expect("iteration headroom");
            }
            _ => {}
        }
        counters.retired = counters.retired.checked_add(1).expect("iteration headroom");
        if cleanup_failed {
            // Stress §6.4 step 1 sets `cleanup_failed` whatever stop the
            // iteration's class set at R2: step 3 keeps a `save` (failing)
            // iteration's promotion after an R10 failure and step 5 then
            // requires `stress_cleanup_failed` unless a higher report §2.14
            // row applies; row 5 there reads this stop and outranks the
            // failing classes. Intent is §5 row 1, checked before row 3 at
            // the next admission, and §2.14 rows 2-3 still precede row 5.
            self.owner.previous_stop = Some(StopReason::CleanupFailed);
        }
        self.owner.live = false;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    IterationBudget,
    TimeBudget,
    SeedRange,
    CaptureBound,
    RunPending,
    NoRunPending,
    State,
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid stress admission: {self:?}")
    }
}

impl std::error::Error for AdmissionError {}

// Stress §6.6 permits eight runs in a future interactive iteration. The
// lending iteration/retirement boundary does not depend on the batch array's
// length; an interactive adapter must preserve atomic pair admission and the
// same single-live-iteration boundary.

#[cfg(test)]
#[path = "stress/tests.rs"]
mod tests;
