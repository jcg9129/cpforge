use super::*;
use crate::jobs::engine::CancelSource;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
struct Clock(Arc<Mutex<Duration>>);

impl Clock {
    fn advance(&self, duration: Duration) {
        *self.0.lock().unwrap() += duration;
    }
}

impl StressClock for Clock {
    fn now(&self) -> Duration {
        *self.0.lock().unwrap()
    }
}

fn templates() -> BatchTemplates<BatchRole> {
    let role = |value| RunTemplate::new(value, 4096, 1024).unwrap();
    BatchTemplates::new(
        role(BatchRole::Generator),
        Some(role(BatchRole::Validator)),
        Some(role(BatchRole::Brute)),
        role(BatchRole::Candidate),
        Some(role(BatchRole::Checker)),
    )
    .unwrap()
}

fn admission(iterations: u64, keep_going: bool) -> StressAdmission<BatchRole> {
    StressAdmission::new(
        templates(),
        Schedule::Search {
            start_seed: 23,
            budget: SearchBudget {
                iterations,
                time: Duration::from_secs(60),
            },
            keep_going,
        },
        CancelState::new(),
        true,
        Box::<Clock>::default(),
    )
    .unwrap()
}

/// Injected roles finalize their records immediately. The retirement
/// acknowledgment here is deliberately only an admission-layer harness: disk
/// settlement, removal, physical holders and events belong to its adapter.
fn finish(mut live: LiveIteration<'_, BatchRole>, class: IterationClass) -> Disposition {
    let mut runs = 0;
    while let Some(run) = live.admit_run().unwrap() {
        assert_eq!(run.template, run.id.role);
        assert_eq!(run.seed, live.seed);
        runs += 1;
        live.record_finalized(RunCompletion {
            started: true,
            captured_bytes: 5120,
            gate: RunGate::Continue,
            retention_refused: false,
        })
        .unwrap();
    }
    if live.cancelled().is_none() {
        assert_eq!(runs, 5);
    }
    let retiring = live.begin_retirement(class).unwrap();
    let disposition = retiring.disposition;
    retiring
        .complete(
            (disposition == Disposition::Save).then_some(SaveResult::Promoted),
            false,
        )
        .unwrap();
    disposition
}

fn stop<T: Clone + fmt::Debug>(admission: &mut StressAdmission<T>) -> Stop {
    admission.admit().unwrap_err()
}

#[test]
fn st_l02_intent_at_iteration_admission_seven() {
    let mut engine = admission(100, true);
    let cancel = engine.cancel.clone();
    let points = Arc::new(AtomicUsize::new(0));
    let seen = points.clone();
    engine.observe(move |point| {
        if let AdmissionPoint::IterationAdmit(k) = point {
            seen.fetch_add(1, Ordering::Relaxed);
            if k == 7 {
                cancel.request_cancel(CancelSource::Api);
            }
        }
    });
    for k in 0..7 {
        let live = engine.admit().unwrap();
        assert_eq!(live.iteration, k);
        finish(live, IterationClass::Passed);
    }
    assert_eq!(
        stop(&mut engine),
        Stop {
            reason: StopReason::Cancelled,
            iteration: Some(7),
        }
    );
    let counters = engine.counters();
    assert_eq!(counters.admitted, 7);
    assert_eq!(counters.retired, 7);
    assert_eq!(counters.runs_admitted, 35);
    assert_eq!(counters.runs_started, 35);
    assert_eq!(counters.captured_bytes, 35 * 5120);
    assert_eq!(counters.classes[IterationClass::Passed.index()], 7);
    assert_eq!(points.load(Ordering::Relaxed), 8);
    assert_eq!(stop(&mut engine).reason, StopReason::Cancelled);
    assert_eq!(points.load(Ordering::Relaxed), 8);
}

#[test]
fn st_l13_counter_headroom_is_reserved_before_any_batch_run() {
    let mut engine = admission(100, true);
    let counters = StressCounters {
        runs_started: u64::MAX - 3,
        ..StressCounters::default()
    };
    engine.set_counters(counters).unwrap();
    engine.observe(|point| assert!(matches!(point, AdmissionPoint::IterationAdmit(0))));
    assert_eq!(
        stop(&mut engine),
        Stop {
            reason: StopReason::CounterLimit,
            iteration: Some(0),
        }
    );
    assert_eq!(engine.counters(), counters);

    // Exactly enough headroom for one full iteration admits it; the next
    // check refuses before any run, and nothing wraps.
    let mut engine = admission(100, true);
    engine
        .set_counters(StressCounters {
            runs_started: u64::MAX - 5,
            ..StressCounters::default()
        })
        .unwrap();
    finish(engine.admit().unwrap(), IterationClass::Passed);
    assert_eq!(engine.counters().runs_started, u64::MAX);
    assert_eq!(
        stop(&mut engine),
        Stop {
            reason: StopReason::CounterLimit,
            iteration: Some(1),
        }
    );
    assert_eq!(engine.counters().runs_admitted, 5);
}

#[test]
fn st_l14_time_budget_checks_only_admission() {
    let clock = Clock::default();
    // Planning and builds do not consume the iteration time budget.
    clock.advance(Duration::from_secs(600));
    let mut engine = StressAdmission::new(
        templates(),
        Schedule::Search {
            start_seed: 0,
            budget: SearchBudget {
                iterations: 100,
                time: Duration::from_secs(10),
            },
            keep_going: true,
        },
        CancelState::new(),
        true,
        Box::new(clock.clone()),
    )
    .unwrap();
    let checks = Arc::new(Mutex::new(Vec::new()));
    let (observed, at) = (checks.clone(), clock.clone());
    engine.observe(move |point| {
        if let AdmissionPoint::IterationAdmit(_) = point {
            observed.lock().unwrap().push(at.now().as_secs() - 600);
        }
    });
    for k in 0..4 {
        let live = engine.admit().unwrap();
        assert_eq!(live.iteration, k);
        clock.advance(Duration::from_secs(3));
        finish(live, IterationClass::Passed);
    }
    // The fourth iteration, admitted at 9 s, completes past T.
    assert_eq!(clock.now(), Duration::from_secs(612));
    assert_eq!(
        stop(&mut engine),
        Stop {
            reason: StopReason::TimeBudget,
            iteration: Some(4),
        }
    );
    assert_eq!(*checks.lock().unwrap(), [0, 3, 6, 9, 12]);
    assert_eq!(engine.counters().admitted, 4);
    assert_eq!(engine.counters().retired, 4);
    assert_eq!(engine.counters().runs_started, 20);

    // Expiry is `now >= t0 + T`: a check exactly at T is refused.
    let clock = Clock::default();
    let mut engine = StressAdmission::new(
        templates(),
        Schedule::Search {
            start_seed: 0,
            budget: SearchBudget {
                iterations: 100,
                time: Duration::from_secs(9),
            },
            keep_going: true,
        },
        CancelState::new(),
        true,
        Box::new(clock.clone()),
    )
    .unwrap();
    for _ in 0..3 {
        let live = engine.admit().unwrap();
        clock.advance(Duration::from_secs(3));
        finish(live, IterationClass::Passed);
    }
    assert_eq!(stop(&mut engine).reason, StopReason::TimeBudget);
    assert_eq!(engine.counters().admitted, 3);
}

#[test]
fn st_l15_iteration_budget_three_and_one() {
    for n in [3, 1] {
        let mut engine = admission(n, true);
        for k in 0..n {
            let live = engine.admit().unwrap();
            assert_eq!(live.iteration, k);
            assert_eq!(live.seed, 23 + k);
            finish(live, IterationClass::Passed);
        }
        assert_eq!(stop(&mut engine).reason, StopReason::Iterations);
        assert_eq!(engine.stop().unwrap().iteration, Some(n));
        assert_eq!(engine.counters().admitted, n);
        assert_eq!(engine.counters().retired, n);
        assert_eq!(engine.counters().runs_started, 5 * n);
    }
}

#[test]
fn batch_roles_require_final_records_and_skip_absent_templates() {
    let role = |value| RunTemplate::new(value, 0, 0).unwrap();
    let sparse = BatchTemplates::new(
        role(BatchRole::Generator),
        None,
        None,
        role(BatchRole::Candidate),
        Some(role(BatchRole::Checker)),
    )
    .unwrap();
    let mut engine = admission(1, true);
    engine.templates = Box::new(sparse);
    let mut live = engine.admit().unwrap();
    for expected in [
        BatchRole::Generator,
        BatchRole::Candidate,
        BatchRole::Checker,
    ] {
        let run = live.admit_run().unwrap().unwrap();
        assert_eq!(run.id.role, expected);
        assert_eq!(live.admit_run().unwrap_err(), AdmissionError::RunPending);
        live.record_finalized(RunCompletion {
            started: true,
            captured_bytes: 0,
            gate: RunGate::Continue,
            retention_refused: false,
        })
        .unwrap();
        assert_eq!(
            live.record_finalized(RunCompletion {
                started: true,
                captured_bytes: 0,
                gate: RunGate::Continue,
                retention_refused: false,
            }),
            Err(AdmissionError::NoRunPending)
        );
    }
    assert!(live.admit_run().unwrap().is_none());
    live.begin_retirement(IterationClass::Passed)
        .unwrap()
        .complete(None, false)
        .unwrap();
    assert_eq!(engine.counters().runs_started, 3);
    assert_eq!(stop(&mut engine).reason, StopReason::Iterations);
}

#[test]
fn a_deciding_role_prevents_all_later_admissions() {
    for deciding_role in BatchRole::ALL {
        let mut engine = admission(2, true);
        let mut live = engine.admit().unwrap();
        while let Some(run) = live.admit_run().unwrap() {
            live.record_finalized(RunCompletion {
                started: true,
                captured_bytes: 4,
                gate: if run.id.role == deciding_role {
                    RunGate::Decided
                } else {
                    RunGate::Continue
                },
                retention_refused: false,
            })
            .unwrap();
        }
        let retiring = live
            .begin_retirement(IterationClass::GeneratorFailure)
            .unwrap();
        assert_eq!(retiring.disposition, Disposition::Save);
        retiring
            .complete(Some(SaveResult::NotSaved), false)
            .unwrap();
        assert_eq!(
            engine.counters().runs_admitted,
            deciding_role.index() as u64 + 1
        );
    }
}

#[test]
fn batch_run_admission_cancellation_keeps_the_iteration() {
    let mut engine = admission(100, true);
    let cancel = engine.cancel.clone();
    engine.observe(move |point| {
        if point
            == AdmissionPoint::RunAdmit(IterationRunId {
                iteration: 7,
                role: BatchRole::Candidate,
            })
        {
            cancel.request_cancel(CancelSource::Api);
        }
    });
    for _ in 0..7 {
        finish(engine.admit().unwrap(), IterationClass::Passed);
    }
    let mut live = engine.admit().unwrap();
    let mut admitted = Vec::new();
    while let Some(run) = live.admit_run().unwrap() {
        admitted.push(run.id.role);
        live.record_finalized(RunCompletion {
            started: true,
            captured_bytes: 0,
            gate: RunGate::Continue,
            retention_refused: false,
        })
        .unwrap();
    }
    assert_eq!(
        admitted,
        [BatchRole::Generator, BatchRole::Validator, BatchRole::Brute]
    );
    assert_eq!(
        live.cancelled(),
        Some(IterationRunId {
            iteration: 7,
            role: BatchRole::Candidate,
        })
    );
    // Intent overrides a class derived without it; S1 still precedes S2.
    let retiring = live.begin_retirement(IterationClass::Passed).unwrap();
    assert_eq!(retiring.class(), IterationClass::Cancelled);
    assert_eq!(retiring.disposition(), Disposition::Keep);
    retiring.complete(None, false).unwrap();
    assert_eq!(engine.counters().retired, 8);
    assert_eq!(engine.counters().runs_admitted, 38);
    assert_eq!(engine.counters().runs_started, 38);
    assert_eq!(
        engine.counters().classes[IterationClass::Cancelled.index()],
        1
    );
    assert_eq!(stop(&mut engine).reason, StopReason::Cancelled);
}

#[test]
fn stop_precedence_and_late_intent_are_stable() {
    let mut engine = admission(1, false);
    engine.builds_succeeded = false;
    engine.cancel.request_cancel(CancelSource::Api);
    assert_eq!(stop(&mut engine).reason, StopReason::Cancelled);

    let mut engine = admission(1, false);
    engine.builds_succeeded = false;
    assert_eq!(
        stop(&mut engine),
        Stop {
            reason: StopReason::BuildFailed,
            iteration: None,
        }
    );

    let mut engine = admission(1, false);
    finish(engine.admit().unwrap(), IterationClass::CandidateFailure);
    // An earlier failure wins over exhausted iteration/time budgets.
    assert_eq!(stop(&mut engine).reason, StopReason::Failure);

    let mut engine = admission(1, true);
    finish(engine.admit().unwrap(), IterationClass::Passed);
    let completed = stop(&mut engine);
    engine.cancel.request_cancel(CancelSource::Api);
    assert_eq!(stop(&mut engine), completed);
    assert_eq!(completed.reason, StopReason::Iterations);
}

#[test]
fn infrastructure_and_cleanup_stop_even_with_keep_going() {
    let mut engine = admission(100, true);
    finish(engine.admit().unwrap(), IterationClass::Infrastructure);
    assert_eq!(stop(&mut engine).reason, StopReason::Infrastructure);

    let mut engine = admission(100, true);
    let mut live = engine.admit().unwrap();
    while live.admit_run().unwrap().is_some() {
        live.record_finalized(RunCompletion {
            started: true,
            captured_bytes: 0,
            gate: RunGate::Continue,
            retention_refused: false,
        })
        .unwrap();
    }
    live.begin_retirement(IterationClass::Passed)
        .unwrap()
        .complete(None, true)
        .unwrap();
    assert_eq!(stop(&mut engine).reason, StopReason::CleanupFailed);
    assert_eq!(engine.counters().retired, 1);
}

#[test]
fn retention_refusal_closes_admission_and_counts_no_start() {
    let mut engine = admission(100, true);
    let mut live = engine.admit().unwrap();
    assert_eq!(
        live.admit_run().unwrap().unwrap().id.role,
        BatchRole::Generator
    );
    live.record_finalized(RunCompletion {
        started: false,
        captured_bytes: 0,
        gate: RunGate::Decided,
        retention_refused: true,
    })
    .unwrap();
    let retiring = live
        .begin_retirement(IterationClass::Infrastructure)
        .unwrap();
    assert_eq!(retiring.disposition, Disposition::Keep);
    retiring.complete(None, false).unwrap();
    assert_eq!(engine.counters().admitted, 1);
    assert_eq!(engine.counters().retired, 1);
    assert_eq!(engine.counters().runs_admitted, 1);
    assert_eq!(engine.counters().runs_started, 0);
    assert_eq!(stop(&mut engine).reason, StopReason::AdmissionClosed);
}

#[test]
fn failed_save_attempts_still_consume_the_twenty_attempt_cap() {
    let mut engine = admission(25, true);
    for k in 0..25 {
        let mut live = engine.admit().unwrap();
        while live.admit_run().unwrap().is_some() {
            live.record_finalized(RunCompletion {
                started: true,
                captured_bytes: 1,
                gate: RunGate::Continue,
                retention_refused: false,
            })
            .unwrap();
        }
        let retiring = live
            .begin_retirement(IterationClass::CandidateFailure)
            .unwrap();
        assert_eq!(
            retiring.disposition,
            if k < 20 {
                Disposition::Save
            } else {
                Disposition::Count
            }
        );
        retiring
            .complete((k < 20).then_some(SaveResult::NotSaved), false)
            .unwrap();
    }
    assert_eq!(stop(&mut engine).reason, StopReason::Iterations);
    let counters = engine.counters();
    assert_eq!(counters.failing, 25);
    assert_eq!(counters.saved, 0);
    assert_eq!(counters.promoted, 0);
    assert_eq!(counters.not_saved, 25);
    assert_eq!(counters.failures_dropped, 5);
}

#[test]
fn abandoned_execution_or_retirement_never_opens_admission() {
    let mut engine = admission(100, true);
    {
        let _live = engine.admit().unwrap();
    }
    assert_eq!(engine.counters().retired, 0);
    assert!(engine.cancel.lock().internal.is_none());
    assert_eq!(stop(&mut engine).reason, StopReason::Infrastructure);
    assert_eq!(engine.counters().admitted, 1);
    assert_eq!(
        engine.cancel.lock().internal,
        Some(InternalFailure {
            invariant: Invariant::JobEvidence,
            locus: InternalLocus::Job,
        })
    );

    let mut engine = admission(100, true);
    let mut live = engine.admit().unwrap();
    while live.admit_run().unwrap().is_some() {
        live.record_finalized(RunCompletion {
            started: true,
            captured_bytes: 0,
            gate: RunGate::Continue,
            retention_refused: false,
        })
        .unwrap();
    }
    {
        let _retiring = live.begin_retirement(IterationClass::Passed).unwrap();
    }
    assert_eq!(engine.counters().retired, 0);
    assert_eq!(stop(&mut engine).reason, StopReason::Infrastructure);
    assert!(engine.cancel.lock().internal.is_some());
}

#[test]
fn bounds_and_seed_span_use_checked_arithmetic() {
    assert_eq!(
        RunTemplate::new((), u64::MAX, 1).unwrap_err(),
        AdmissionError::CaptureBound
    );
    let huge = || RunTemplate::new((), u64::MAX, 0).unwrap();
    assert_eq!(
        BatchTemplates::new(huge(), None, None, huge(), None).unwrap_err(),
        AdmissionError::CaptureBound
    );
    let make = |start_seed, iterations, time| {
        StressAdmission::new(
            templates(),
            Schedule::Search {
                start_seed,
                budget: SearchBudget { iterations, time },
                keep_going: true,
            },
            CancelState::new(),
            true,
            Box::<Clock>::default(),
        )
    };
    let time = Duration::from_secs(1);
    assert_eq!(
        make(0, 0, time).unwrap_err(),
        AdmissionError::IterationBudget
    );
    assert_eq!(
        make(0, 1, Duration::ZERO).unwrap_err(),
        AdmissionError::TimeBudget
    );
    assert_eq!(
        make(u64::MAX, 2, time).unwrap_err(),
        AdmissionError::SeedRange
    );
    let mut engine = make(u64::MAX - 1, 2, time).unwrap();
    for seed in [u64::MAX - 1, u64::MAX] {
        let live = engine.admit().unwrap();
        assert_eq!(live.seed, seed);
        finish(live, IterationClass::Passed);
    }
    assert_eq!(stop(&mut engine).reason, StopReason::Iterations);
}

#[test]
fn replay_checks_builds_and_retires_exactly_one_saved_index() {
    for iteration in [9999, 10_000, u64::MAX] {
        let mut engine = StressAdmission::new(
            templates(),
            Schedule::Replay {
                iteration,
                seed: 17,
            },
            CancelState::new(),
            true,
            Box::<Clock>::default(),
        )
        .unwrap();
        let live = engine.admit().unwrap();
        assert_eq!(live.iteration, iteration);
        assert_eq!(live.seed, 17);
        assert_eq!(
            finish(live, IterationClass::CandidateFailure),
            Disposition::Keep
        );
        assert_eq!(
            stop(&mut engine),
            Stop {
                reason: StopReason::Replay,
                iteration: Some(iteration),
            }
        );
        assert_eq!(engine.counters().retired, 1);
        assert_eq!(engine.counters().failing, 1);
        assert_eq!(engine.counters().not_saved, 0);
    }

    // Row 2 applies to a replay; counter injection is for search jobs only.
    let mut engine = StressAdmission::new(
        templates(),
        Schedule::Replay {
            iteration: 3,
            seed: 0,
        },
        CancelState::new(),
        false,
        Box::<Clock>::default(),
    )
    .unwrap();
    assert_eq!(
        engine.set_counters(StressCounters::default()),
        Err(AdmissionError::State)
    );
    assert_eq!(
        stop(&mut engine),
        Stop {
            reason: StopReason::BuildFailed,
            iteration: None,
        }
    );
    assert_eq!(engine.counters().admitted, 0);
}

#[test]
fn iteration_id_has_unpadded_u64_identity_and_portable_components() {
    for k in [0, 9999, 10_000, 10_004, u64::MAX] {
        for role in BatchRole::ALL {
            let id = IterationRunId { iteration: k, role };
            assert_eq!(id.to_string(), format!("{k}.{}", role.token()));
            assert_eq!(
                id.directory(Path::new("job")),
                Path::new("job").join("iterations").join(id.to_string())
            );
        }
    }
}

#[test]
fn every_cumulative_counter_refuses_overflow_without_mutation() {
    let mut cases = Vec::new();
    for field in 0..10 {
        let mut counters = StressCounters::default();
        match field {
            0 => counters.admitted = u64::MAX,
            1 => counters.retired = u64::MAX,
            2 => counters.runs_admitted = u64::MAX - 4,
            3 => counters.runs_started = u64::MAX - 4,
            4 => counters.captured_bytes = u64::MAX - 25_599,
            5 => counters.failing = u64::MAX,
            6 => counters.saved = u64::MAX,
            7 => counters.promoted = u64::MAX,
            8 => counters.not_saved = u64::MAX,
            9 => counters.failures_dropped = u64::MAX,
            _ => unreachable!(),
        }
        cases.push(counters);
    }
    cases.extend((0..CLASSES).map(|class| {
        let mut counters = StressCounters::default();
        counters.classes[class] = u64::MAX;
        counters
    }));
    for counters in cases {
        // Start zero makes even an injected large admission count a valid seed
        // identity; counter refusal must happen before identity construction.
        let mut engine = StressAdmission::new(
            templates(),
            Schedule::Search {
                start_seed: 0,
                budget: SearchBudget {
                    iterations: u64::MAX,
                    time: Duration::from_secs(60),
                },
                keep_going: true,
            },
            CancelState::new(),
            true,
            Box::<Clock>::default(),
        )
        .unwrap();
        engine.set_counters(counters).unwrap();
        let reason = stop(&mut engine).reason;
        assert_eq!(
            reason,
            if counters.admitted == u64::MAX {
                StopReason::Iterations
            } else {
                StopReason::CounterLimit
            }
        );
        assert_eq!(engine.counters(), counters);
    }
}

#[test]
fn intent_during_retirement_finishes_bookkeeping_before_stopping() {
    let mut engine = admission(100, true);
    let cancel = engine.cancel.clone();
    let mut live = engine.admit().unwrap();
    while live.admit_run().unwrap().is_some() {
        live.record_finalized(RunCompletion {
            started: true,
            captured_bytes: 0,
            gate: RunGate::Continue,
            retention_refused: false,
        })
        .unwrap();
    }
    let retiring = live
        .begin_retirement(IterationClass::CandidateFailure)
        .unwrap();
    cancel.request_cancel(CancelSource::Api);
    assert_eq!(retiring.class(), IterationClass::CandidateFailure);
    retiring
        .complete(Some(SaveResult::Promoted), false)
        .unwrap();
    let counters = engine.counters();
    assert_eq!(counters.retired, 1);
    assert_eq!(counters.saved, 1);
    assert_eq!(counters.promoted, 1);
    assert_eq!(stop(&mut engine).reason, StopReason::Cancelled);
}

#[derive(Debug)]
struct TrackedTemplate {
    copies: Rc<Cell<usize>>,
    peak: Rc<Cell<usize>>,
}

impl Clone for TrackedTemplate {
    fn clone(&self) -> Self {
        let copies = self.copies.get() + 1;
        self.copies.set(copies);
        self.peak.set(self.peak.get().max(copies));
        Self {
            copies: self.copies.clone(),
            peak: self.peak.clone(),
        }
    }
}

impl Drop for TrackedTemplate {
    fn drop(&mut self) {
        self.copies.set(self.copies.get() - 1);
    }
}

#[test]
fn streaming_admission_exceeds_plan_cap_with_constant_template_storage() {
    let copies = Rc::new(Cell::new(5));
    let peak = Rc::new(Cell::new(5));
    let template = || {
        RunTemplate::new(
            TrackedTemplate {
                copies: copies.clone(),
                peak: peak.clone(),
            },
            0,
            0,
        )
        .unwrap()
    };
    let templates = BatchTemplates::new(
        template(),
        Some(template()),
        Some(template()),
        template(),
        Some(template()),
    )
    .unwrap();
    let mut engine = StressAdmission::new(
        templates,
        Schedule::Search {
            start_seed: 0,
            budget: SearchBudget {
                iterations: 10_005,
                time: Duration::from_secs(60),
            },
            keep_going: true,
        },
        CancelState::new(),
        true,
        Box::<Clock>::default(),
    )
    .unwrap();
    for k in 0..10_005 {
        let mut live = engine.admit().unwrap();
        while let Some(run) = live.admit_run().unwrap() {
            assert_eq!(run.id.iteration, k);
            assert_eq!(copies.get(), 6);
            live.record_finalized(RunCompletion {
                started: true,
                captured_bytes: 0,
                gate: RunGate::Continue,
                retention_refused: false,
            })
            .unwrap();
        }
        live.begin_retirement(IterationClass::Passed)
            .unwrap()
            .complete(None, false)
            .unwrap();
        assert_eq!(copies.get(), 5);
    }
    assert_eq!(stop(&mut engine).reason, StopReason::Iterations);
    assert_eq!(engine.counters().retired, 10_005);
    assert_eq!(engine.counters().runs_started, 50_025);
    assert_eq!(peak.get(), 6);
    drop(engine);
    assert_eq!(copies.get(), 0);
}
