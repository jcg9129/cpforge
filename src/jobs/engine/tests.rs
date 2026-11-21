use super::*;
use crate::jobs::{prepare_build_job, prepare_job};
use crate::storage::inputs::CapturedSource;
use crate::storage::prune::PrunePolicy;
use crate::supervisor::{self, CancelToken, Stream};
use std::collections::VecDeque;
use std::sync::Barrier;
use std::time::Instant;
use tokio::sync::oneshot;

#[path = "../../../tests/ready_support/mod.rs"]
#[allow(dead_code)]
mod ready_support;
use ready_support::{Fixture, Tools};

fn runtime<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn ready<'a>(
    f: &Fixture,
    language: &str,
    selected: usize,
    probe: &'a dyn crate::exec::Probe,
) -> ReadyJob<'a> {
    f.write(
        if language == "cpp" {
            "problem/main.cpp"
        } else {
            "problem/main.py"
        },
        "source\n",
    );
    let target = f.target(language, "");
    let mut catalog = String::from("schema = 1\n");
    let mut ids = Vec::new();
    for i in 0..selected {
        let id = format!("t{i}");
        catalog.push_str(&format!("[[test]]\nid = '{id}'\ninput = 'tests/{id}.in'\nanswer = 'tests/{id}.ans'\norigin = 'custom'\nenabled = true\n"));
        f.write(&format!("problem/tests/{id}.in"), "input\n");
        f.write(&format!("problem/tests/{id}.ans"), "yes\n");
        ids.push(crate::names::Identifier::new(id).unwrap());
    }
    f.write("problem/tests.toml", &catalog);
    let registered = f.job(256 << 20, PrunePolicy::default());
    let frozen = registered
        .inputs()
        .freeze(
            &f.store(),
            &ids,
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    prepare_job(registered, frozen, &target, f.inputs(probe)).unwrap()
}

fn test_engine(f: &Fixture, language: &str, selected: usize) -> Engine<'static> {
    Engine::new(
        ready(f, language, selected, &Tools),
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap()
}

fn command_engine(
    f: &Fixture,
    kind: CommandKind,
    count: usize,
    keep_going: bool,
) -> Engine<'static> {
    let ready = ready(f, "python", 0, &Tools);
    let iterations = (0..count)
        .map(|index| Iteration {
            role: match kind {
                CommandKind::Cmd => Role::Command {
                    test: None,
                    iteration: index as u32 + 1,
                },
                CommandKind::Run => Role::Raw,
                CommandKind::Hook => Role::Hook {
                    name: format!("hook{index}"),
                    trigger: "on_switch".into(),
                },
            },
            recipe: Recipe::TargetRun,
            stdin: None,
            depends_on_build: false,
        })
        .collect();
    Engine::new(
        ready,
        JobRequest::Iterations {
            kind,
            iterations,
            needs_build: false,
            keep_going,
        },
    )
    .unwrap()
}

#[derive(Debug)]
enum Behavior {
    Exit(i64),
    WaitCancel,
    Panic,
    Wall,
    InvalidLimit,
}
#[derive(Debug, Default)]
struct Fake {
    behavior: VecDeque<Behavior>,
    runs: Vec<RunId>,
    entered: Option<oneshot::Sender<()>>,
    resume: Option<oneshot::Receiver<()>>,
}
impl Fake {
    fn with(behavior: impl IntoIterator<Item = Behavior>) -> Self {
        Self {
            behavior: behavior.into_iter().collect(),
            ..Self::default()
        }
    }
}
impl Executor for Fake {
    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        cancel: CancelToken,
        events: RunEvents,
        resources: RunResources,
    ) -> JobFuture<'_, ExecutionRecord> {
        Box::pin(async move {
            let _resources = resources;
            self.runs.push(spec.run.clone());
            assert_eq!(spec.resolved.cwd, spec.directory.join("cwd"));
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            if let Some(resume) = self.resume.take() {
                resume.await.unwrap();
            }
            let behavior = self.behavior.pop_front().unwrap_or(Behavior::Exit(0));
            assert!(!matches!(behavior, Behavior::Panic), "executor panic");
            if spec.stage == Stage::Build && matches!(behavior, Behavior::Exit(0)) {
                // Simulate the declared compiler output, without starting tools.
                let output = spec
                    .command
                    .args
                    .windows(2)
                    .find(|pair| pair[0] == "-o")
                    .unwrap();
                fs::write(&output[1], "generated program").unwrap();
            }
            if matches!(behavior, Behavior::InvalidLimit) {
                return ExecutionRecord::NotStarted(StartFailure {
                    reason: NotStartedReason::UnsupportedLimit,
                    api: None,
                    os_error: None,
                    invalid_limit: Some(supervisor::LimitValidation::OutOfRange),
                });
            }
            events.output(Stream::Stdout, b"yes\n");
            let mut outcome = root(match behavior {
                Behavior::Exit(code) => code,
                _ => 0,
            });
            match behavior {
                Behavior::WaitCancel => {
                    cancel.cancelled().await;
                    outcome.stop_trigger = Some(supervisor::StopTrigger::Cancelled);
                }
                Behavior::Wall => {
                    outcome.stop_trigger = Some(supervisor::StopTrigger::WallDeadline);
                    outcome
                        .violations
                        .push(supervisor::Violation::WallDeadline {
                            observed_by: Instant::now(),
                        });
                }
                _ => {}
            }
            ExecutionRecord::Started(Box::new(outcome))
        })
    }
}

fn root(code: i64) -> supervisor::RootOutcome {
    supervisor::RootOutcome {
        stdout: Some(supervisor::StreamCapture {
            observed: 4,
            retained: b"yes\n".to_vec(),
            stored: 4,
            forwarded: 0,
            eof: true,
            exceeded_at: None,
            losses: Vec::new(),
            sink: None,
        }),
        stderr: None,
        status: supervisor::Termination::Exited { code },
        stop_trigger: None,
        enforced_at: None,
        termination: supervisor::TerminationEvidence {
            requested_at: None,
            method: None,
            cleanup: supervisor::Cleanup::Confirmed,
        },
        violations: Vec::new(),
        measurements: Vec::new(),
        limits: Vec::new(),
        timing: supervisor::RootTiming {
            setup: Duration::ZERO,
            create_end: Instant::now(),
            exit_observed: Some(Instant::now()),
            observer: supervisor::ObserverKind::Polling,
            wall: Some(Duration::from_millis(1)),
            method: supervisor::WallMethod::Observed,
            grace: Duration::ZERO,
        },
        notes: Vec::new(),
        errors: Vec::new(),
    }
}
async fn run(engine: Engine<'_>, fake: &mut Fake) -> Arc<FinalizedReport> {
    engine
        .run(fake, &mut MemoryPersistence::default(), &mut NoHooks)
        .await
}

#[test]
fn plan_build_first_manifest_order_and_run_ids() {
    let f = Fixture::new();
    let engine = test_engine(&f, "cpp", 2);
    assert_eq!(
        engine.plan().iter().map(|p| p.index).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(engine.plan()[0].role, Role::Build);
    assert!(engine.plan()[1..].iter().all(|p| p.depends_on_build));
    let mut fake = Fake::default();
    let report = runtime(run(engine, &mut fake));
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert_eq!(
        fake.runs.iter().map(RunId::as_str).collect::<Vec<_>>(),
        ["r00000", "r00001", "r00002"]
    );
    let build = report.data().build.as_ref().unwrap();
    assert_eq!(build.build_id.as_ref().unwrap().as_str(), "r00000");
    assert!(
        build
            .output_root
            .as_ref()
            .unwrap()
            .ends_with(std::path::Path::new("builds").join("r00000"))
    );
    assert_eq!(
        report
            .data()
            .tests
            .iter()
            .map(|t| t.frozen.id.as_str())
            .collect::<Vec<_>>(),
        ["t0", "t1"]
    );
    assert!(!report.data().result.partial);
}

#[test]
fn cancellation_before_admission_skips_without_allocating() {
    for build in [false, true] {
        let f = Fixture::new();
        let engine = test_engine(&f, if build { "cpp" } else { "python" }, 2);
        let cancel = engine.cancellation();
        cancel.request_cancel(CancelSource::Api);
        let mut fake = Fake::default();
        let report = runtime(run(engine, &mut fake));
        assert!(fake.runs.is_empty());
        assert!(report.data().executions.iter().all(|e| e.run.is_none()));
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        assert_eq!(
            report.data().result.cancellation.unwrap().phase,
            CancelPhase::Primary
        );
        assert!(report.data().result.partial);
    }
}

#[test]
fn cancellation_during_build_uses_same_token_and_blocks_dependants() {
    runtime(async {
        let f = Fixture::new();
        let engine = test_engine(&f, "cpp", 2);
        let cancel = engine.cancellation();
        let (entered, ready) = oneshot::channel();
        let mut fake = Fake {
            entered: Some(entered),
            ..Fake::with([Behavior::WaitCancel])
        };
        let (report, ()) = tokio::join!(run(engine, &mut fake), async {
            ready.await.unwrap();
            cancel.request_cancel(CancelSource::Console);
        });
        assert_eq!(
            report.data().build.as_ref().unwrap().result,
            BuildResult::Cancelled
        );
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        assert_eq!(
            report.data().result.cancellation.unwrap().source,
            CancelSource::Console
        );
        assert!(
            report.data().executions[1..]
                .iter()
                .all(|e| e.result.coverage
                    == Coverage::Skipped {
                        reason: SkipReason::BuildNotSucceeded
                    })
        );
    });
}

#[test]
fn cancellation_between_tests_preserves_finalized_first_test() {
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 2);
    let cancel = engine.cancellation();
    let engine = engine.observe(move |point| {
        if point == Point::BeforeAdmission(1) {
            cancel.request_cancel(CancelSource::Api);
        }
    });
    let report = runtime(run(engine, &mut Fake::default()));
    assert_eq!(
        report.data().tests[0].verdict.as_ref().unwrap(),
        &report::TestVerdict::Passed
    );
    assert_eq!(
        report.data().tests[1].verdict.as_ref().unwrap(),
        &report::TestVerdict::Cancelled
    );
    assert_eq!(
        report.data().executions[1].result.coverage,
        Coverage::Skipped {
            reason: SkipReason::Cancelled
        }
    );
    assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
}

#[test]
fn cancellation_check_arbitration_on_both_sides_of_the_lock() {
    for after in [false, true] {
        let f = Fixture::new();
        let engine = test_engine(&f, "python", 1);
        let cancel = engine.cancellation();
        let rendezvous = Arc::new(Barrier::new(2));
        let released = Arc::new(Barrier::new(2));
        let thread = {
            let rendezvous = rendezvous.clone();
            let released = released.clone();
            std::thread::spawn(move || {
                rendezvous.wait();
                cancel.request_cancel(CancelSource::Api);
                released.wait();
            })
        };
        let engine = engine.observe(move |point| {
            if point
                == if after {
                    Point::AfterCheckAdmission(0)
                } else {
                    Point::BeforeCheck(0)
                }
            {
                rendezvous.wait();
                released.wait();
            }
        });
        let report = runtime(run(engine, &mut Fake::default()));
        thread.join().unwrap();
        assert_eq!(
            report.data().tests[0].verdict.as_ref().unwrap().clone(),
            if after {
                report::TestVerdict::Passed
            } else {
                report::TestVerdict::Cancelled
            }
        );
        assert_eq!(
            report.data().result.verdict,
            if after {
                JobVerdict::Passed
            } else {
                JobVerdict::Cancelled
            }
        );
        assert_eq!(
            report.data().result.cancellation.unwrap().phase,
            CancelPhase::Primary
        );
    }
}

#[test]
fn late_cancellation_phase_is_total_for_every_job_kind() {
    for kind in [
        JobKind::Build,
        JobKind::Test,
        JobKind::Cmd,
        JobKind::Run,
        JobKind::Hook,
    ] {
        for (point, expected) in [
            (
                Point::AfterPrimaryFreeze,
                if kind == JobKind::Test {
                    CancelPhase::Fact
                } else {
                    CancelPhase::Settlement
                },
            ),
            (Point::Settlement, CancelPhase::Settlement),
            (Point::Closure, CancelPhase::Closure),
            (Point::BeforeSnapshot, CancelPhase::Closure),
        ] {
            let f = Fixture::new();
            let engine = match kind {
                JobKind::Build => {
                    Engine::new(ready(&f, "cpp", 0, &Tools), JobRequest::Build).unwrap()
                }
                JobKind::Test => test_engine(&f, "python", 1),
                JobKind::Cmd => command_engine(&f, CommandKind::Cmd, 1, true),
                JobKind::Run => command_engine(&f, CommandKind::Run, 1, true),
                JobKind::Hook => command_engine(&f, CommandKind::Hook, 1, true),
            };
            let cancel = engine.cancellation();
            let engine = engine.observe(move |observed| {
                if observed == point {
                    cancel.request_cancel(CancelSource::Terminate);
                }
            });
            let report = runtime(run(engine, &mut Fake::default()));
            assert_eq!(
                report.data().result.cancellation.unwrap().phase,
                expected,
                "{kind:?} {point:?}"
            );
            assert_eq!(
                report.data().result.verdict,
                if kind == JobKind::Test {
                    JobVerdict::Passed
                } else {
                    JobVerdict::Succeeded
                }
            );
            assert!(matches!(
                report.data().warnings.as_slice(),
                [Warning::LateCancellation]
            ));
        }
    }
}

#[test]
fn cancellation_after_finalization_is_not_recorded_and_second_notification_requests_force_exit() {
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 1);
    let cancel = engine.cancellation();
    let notify = cancel.clone();
    let engine = engine.observe(move |point| {
        if point == Point::AfterSnapshot {
            notify.request_cancel(CancelSource::Console);
        }
    });
    let report = runtime(run(engine, &mut Fake::default()));
    assert!(report.data().result.cancellation.is_none());
    assert!(cancel.cancellation().is_none());
    cancel.request_at(
        CancelSource::Terminate,
        Instant::now() + Duration::from_millis(100),
    );
    assert!(cancel.force_exit_requested());
    let cancel = CancelState::new();
    let first = Instant::now();
    cancel.request_at(CancelSource::Console, first);
    cancel.request_at(CancelSource::Terminate, first + Duration::from_millis(99));
    assert!(!cancel.force_exit_requested());
    cancel.request_at(CancelSource::Terminate, first + Duration::from_millis(100));
    assert!(cancel.force_exit_requested());
    assert_eq!(cancel.cancellation().unwrap().source, CancelSource::Console);
}

#[test]
fn retention_refusal_closes_admission_and_returns_partial_reservations() {
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 3);
    let budget = engine.ready.budget.clone();
    let blocking = budget
        .reserve(budget.limit() - budget.usage().retained - 512 * 1024)
        .unwrap();
    let mut fake = Fake::default();
    let report = runtime(run(engine, &mut fake));
    assert!(fake.runs.is_empty());
    assert_eq!(
        report.data().executions[0].result.coverage,
        Coverage::NotStarted {
            reason: NotStartedReason::RetentionBudget
        }
    );
    assert!(
        report.data().executions[1..]
            .iter()
            .all(|e| e.result.coverage
                == Coverage::Skipped {
                    reason: SkipReason::AdmissionClosed
                })
    );
    assert_eq!(report.data().result.verdict, JobVerdict::Infrastructure);
    drop(blocking);
    assert_eq!(budget.usage().reserved, (64 * 1024 + 12 * 1024 * 3) * 2);
}

#[test]
fn build_failure_skips_dependants() {
    let f = Fixture::new();
    let mut fake = Fake::with([Behavior::Exit(1)]);
    let report = runtime(run(test_engine(&f, "cpp", 2), &mut fake));
    assert_eq!(fake.runs.len(), 1);
    assert_eq!(report.data().result.verdict, JobVerdict::BuildFailed);
    assert_eq!(report.data().result.counts.primary.skipped, 2);
    assert!(report.data().tests.iter().all(|t| t.verdict.as_ref()
        == Some(&report::TestVerdict::NotRun {
            reason: SkipReason::BuildNotSucceeded
        })));
}

#[test]
fn existing_run_tree_is_untouched_and_id_is_consumed() {
    let f = Fixture::new();
    let engine = command_engine(&f, CommandKind::Cmd, 2, true);
    let collision = engine.ready.dir.join("r00000");
    fs::create_dir(&collision).unwrap();
    fs::write(collision.join("sentinel"), "original").unwrap();
    let mut fake = Fake::default();
    let report = runtime(run(engine, &mut fake));
    assert_eq!(
        fs::read_to_string(collision.join("sentinel")).unwrap(),
        "original"
    );
    assert!(!collision.join("cwd").exists());
    assert_eq!(
        report.data().executions[0].result.coverage,
        Coverage::NotStarted {
            reason: NotStartedReason::Setup
        }
    );
    assert_eq!(
        report.data().executions[0].run.as_ref().unwrap().as_str(),
        "r00000"
    );
    assert_eq!(fake.runs[0].as_str(), "r00001");
    assert_eq!(report.data().result.verdict, JobVerdict::Infrastructure);
}

#[test]
fn executor_panic_preserves_admitted_coverage_and_missing_evidence() {
    let f = Fixture::new();
    let report = runtime(run(
        test_engine(&f, "python", 1),
        &mut Fake::with([Behavior::Panic]),
    ));
    let e = &report.data().executions[0];
    assert_eq!(e.result.coverage, Coverage::Admitted);
    assert_eq!(e.result.invariant, Some(Invariant::RecordMissing));
    assert!(e.run.is_some() && e.spec.is_some() && e.record.is_none() && e.summary.is_none());
    assert_eq!(report.data().result.counts.by_class.unknown, 1);
    assert_eq!(report.data().result.counts.primary.admitted, 1);
    assert_eq!(report.data().result.counts.primary.invalid, 1);
    assert!(report.data().result.partial);
    assert_eq!(report.data().result.verdict, JobVerdict::InternalError);
}

#[test]
fn class_summary_and_test_derivation_panics_preserve_observed_evidence() {
    for point in [
        Point::BeforeClass(0),
        Point::BeforeSummary(0),
        Point::BeforeTestVerdict(0),
    ] {
        let f = Fixture::new();
        let engine = test_engine(&f, "python", 1).observe(move |observed| {
            assert_ne!(observed, point, "derivation panic");
        });
        let report = runtime(run(engine, &mut Fake::with([Behavior::Exit(9)])));
        let e = &report.data().executions[0];
        assert_eq!(e.result.coverage, Coverage::Started);
        assert!(e.record.is_some());
        if point != Point::BeforeTestVerdict(0) {
            assert!(e.summary.is_none());
            assert_eq!(e.result.invariant, Some(Invariant::SummaryUnderivable));
            assert_eq!(
                e.result.class,
                if point == Point::BeforeClass(0) {
                    None
                } else {
                    Some(RunClass::NonzeroExit { code: 9 })
                }
            );
        }
        assert_eq!(
            report.data().result.primary_verdict,
            JobVerdict::InternalError
        );
        assert_eq!(
            report.data().tests[0]
                .verdict
                .as_ref()
                .unwrap()
                .wire_token(),
            "internal_error"
        );
    }
}

#[test]
fn ordinary_and_diagnostic_comparison_panics_are_internal() {
    for diagnostic in [false, true] {
        let f = Fixture::new();
        let engine = test_engine(&f, "python", 1).observe(|point| {
            assert_ne!(point, Point::AfterCheckAdmission(0), "comparison panic");
        });
        let behavior = if diagnostic {
            Behavior::Wall
        } else {
            Behavior::Exit(0)
        };
        let report = runtime(run(engine, &mut Fake::with([behavior])));
        assert_eq!(
            report.data().tests[0].check.as_ref().unwrap().diagnostic,
            diagnostic
        );
        assert_eq!(
            report.data().tests[0]
                .verdict
                .as_ref()
                .unwrap()
                .wire_token(),
            "internal_error"
        );
        assert_eq!(report.data().result.verdict, JobVerdict::InternalError);
    }
}

#[test]
fn command_summary_and_retention_summary_panics_are_internal() {
    for retention in [false, true] {
        let f = Fixture::new();
        let engine = command_engine(&f, CommandKind::Cmd, 1, true);
        let budget = engine.ready.budget.clone();
        let _blocking = retention.then(|| {
            budget
                .reserve(budget.limit() - budget.usage().retained - 512 * 1024)
                .unwrap()
        });
        let point = if retention {
            Point::BeforeClass(0)
        } else {
            Point::BeforeSummary(0)
        };
        let engine = engine.observe(move |observed| {
            assert_ne!(observed, point, "summary panic");
        });
        let report = runtime(run(engine, &mut Fake::default()));
        assert!(report.data().executions[0].summary.is_none());
        assert_eq!(report.data().result.verdict, JobVerdict::InternalError);
        assert_eq!(report.data().result.counts.primary.invalid, 1);
        assert_eq!(
            report.data().result.counts.by_class.exited_zero,
            u32::from(!retention)
        );
        assert_eq!(
            report.data().result.counts.by_class.unknown,
            u32::from(retention)
        );
    }
}

#[test]
fn stop_policy_and_lower_middle_aggregate() {
    let f = Fixture::new();
    let report = runtime(run(
        command_engine(&f, CommandKind::Cmd, 3, false),
        &mut Fake::with([Behavior::Exit(1)]),
    ));
    assert_eq!(
        report.data().executions[1].result.coverage,
        Coverage::Skipped {
            reason: SkipReason::StopPolicy
        }
    );
    let f = Fixture::new();
    let report = runtime(run(
        command_engine(&f, CommandKind::Cmd, 4, true),
        &mut Fake::default(),
    ));
    assert_eq!(report.data().aggregates[0].succeeded, 4);
    assert_eq!(
        report.data().aggregates[0].wall,
        Some([Duration::from_millis(1); 3])
    );
}

#[test]
fn bounded_events_sequence_gap_equation_terminal_barrier_and_output_opt_in() {
    for output in [false, true] {
        runtime(async {
            let f = Fixture::new();
            let mut engine = test_engine(&f, "python", 3).output_events(output);
            engine.events = Events::with_budget(EVENT_BOUND * 4);
            let mut receiver = engine.events().subscribe().unwrap();
            let events = engine.events();
            let report = run(engine, &mut Fake::default()).await;
            let mut observed = Vec::new();
            while let Some(event) = receiver.recv().await {
                observed.push(event);
            }
            let terminal = receiver.terminal().await;
            assert!(Arc::ptr_eq(&terminal.report, &report));
            assert!(terminal.report_id.is_none());
            assert!(observed.windows(2).all(|pair| pair[0].seq < pair[1].seq));
            assert_eq!(observed[0].seq, 0);
            assert_eq!(terminal.seq - observed.len() as u64, terminal.dropped);
            assert_eq!(report.data().events.dropped, terminal.dropped);
            assert!(terminal.dropped > 0);
            assert_eq!(
                observed
                    .iter()
                    .any(|e| matches!(e.payload, EventPayload::OutputChunk { .. })),
                output
            );
            let previous = terminal.dropped;
            events.offer(EventPayload::Progress {
                done: 100,
                total: 100,
            });
            assert_eq!(events.close(), previous);
            assert!(receiver.recv().await.is_none());
        });
    }
}

#[test]
fn build_free_build_and_empty_test_job_finish() {
    let f = Fixture::new();
    f.write("problem/main.py", "pass\n");
    let target = f.target("python", "");
    let ready = prepare_build_job(
        f.job(256 << 20, PrunePolicy::default()),
        &target,
        f.inputs(&Tools),
    )
    .unwrap();
    let report = runtime(run(
        Engine::new(ready, JobRequest::Build).unwrap(),
        &mut Fake::default(),
    ));
    assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
    assert!(report.data().build.is_none());
    let f = Fixture::new();
    let report = runtime(run(test_engine(&f, "cpp", 0), &mut Fake::default()));
    assert_eq!(report.data().result.verdict, JobVerdict::NoTests);
    assert_eq!(report.data().executions.len(), 1);
}

#[test]
fn cancellation_after_admission_produces_a_not_started_record() {
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 1);
    let cancel = engine.cancellation();
    let engine = engine.observe(move |point| {
        if point == Point::AfterAdmission(0) {
            cancel.request_cancel(CancelSource::Api);
        }
    });
    let mut fake = Fake::default();
    let report = runtime(run(engine, &mut fake));
    assert!(fake.runs.is_empty());
    let execution = &report.data().executions[0];
    assert_eq!(execution.run.as_ref().unwrap().as_str(), "r00000");
    assert_eq!(
        execution.result.coverage,
        Coverage::NotStarted {
            reason: NotStartedReason::CancelledBeforeStart
        }
    );
    assert!(execution.record.is_some() && execution.summary.is_some());
    assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
}

#[test]
fn fact_cutoff_is_once_and_hook_start_checks_each_time() {
    struct Hooks {
        cancel: CancelState,
        during_hooks: bool,
        called: bool,
    }
    impl PhaseHooks for Hooks {
        fn fact<'a>(
            &'a mut self,
            primary: JobVerdict,
            cutoff: &'a mut dyn FnMut() -> bool,
        ) -> JobFuture<'a, ()> {
            Box::pin(async move {
                assert_eq!(primary, JobVerdict::Passed);
                assert!(!cutoff());
                if !self.during_hooks {
                    self.cancel.request_cancel(CancelSource::Api);
                }
                assert!(!cutoff(), "the cutoff is not revoked by later intent");
                self.called = true;
            })
        }
        fn hooks<'a>(
            &'a mut self,
            _primary: JobVerdict,
            start: &'a dyn Fn() -> Option<SkipReason>,
        ) -> JobFuture<'a, ()> {
            Box::pin(async move {
                if self.during_hooks {
                    assert_eq!(start(), None);
                    self.cancel.request_cancel(CancelSource::Api);
                }
                assert_eq!(start(), Some(SkipReason::Cancelled));
            })
        }
    }
    for during_hooks in [false, true] {
        let f = Fixture::new();
        let engine = test_engine(&f, "python", 1);
        let mut hooks = Hooks {
            cancel: engine.cancellation(),
            during_hooks,
            called: false,
        };
        let report = runtime(engine.run(
            &mut Fake::default(),
            &mut MemoryPersistence::default(),
            &mut hooks,
        ));
        assert!(hooks.called);
        assert_eq!(report.data().result.primary_verdict, JobVerdict::Passed);
        assert_eq!(
            report.data().result.cancellation.unwrap().phase,
            if during_hooks {
                CancelPhase::Hooks
            } else {
                CancelPhase::Fact
            }
        );
    }
}

#[test]
fn fact_cutoff_sees_intent_before_its_first_read() {
    struct Hooks(CancelState);
    impl PhaseHooks for Hooks {
        fn fact<'a>(
            &'a mut self,
            _primary: JobVerdict,
            cutoff: &'a mut dyn FnMut() -> bool,
        ) -> JobFuture<'a, ()> {
            Box::pin(async move {
                self.0.request_cancel(CancelSource::Api);
                assert!(cutoff());
            })
        }
    }
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 1);
    let mut hooks = Hooks(engine.cancellation());
    let report = runtime(engine.run(
        &mut Fake::default(),
        &mut MemoryPersistence::default(),
        &mut hooks,
    ));
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert_eq!(
        report.data().result.cancellation.unwrap().phase,
        CancelPhase::Fact
    );
}

#[test]
fn late_internal_failure_preserves_primary_and_disables_hook_starts() {
    struct Hooks;
    impl PhaseHooks for Hooks {
        fn fact<'a>(
            &'a mut self,
            _: JobVerdict,
            _: &'a mut dyn FnMut() -> bool,
        ) -> JobFuture<'a, ()> {
            panic!("fact must be ineligible");
        }
        fn hooks<'a>(
            &'a mut self,
            primary: JobVerdict,
            start: &'a dyn Fn() -> Option<SkipReason>,
        ) -> JobFuture<'a, ()> {
            Box::pin(async move {
                assert_eq!(primary, JobVerdict::Passed);
                assert_eq!(start(), Some(SkipReason::JobNotEligible));
            })
        }
    }
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 1).observe(|point| {
        assert_ne!(point, Point::AfterPrimaryFreeze, "late panic");
    });
    let report = runtime(engine.run(
        &mut Fake::default(),
        &mut MemoryPersistence::default(),
        &mut Hooks,
    ));
    assert_eq!(report.data().result.primary_verdict, JobVerdict::Passed);
    assert_eq!(report.data().result.verdict, JobVerdict::InternalError);
}

#[test]
fn settlement_waits_outside_verdict_lock_and_keeps_reservations() {
    struct PersistenceGate {
        memory: MemoryPersistence,
        entered: Option<oneshot::Sender<()>>,
        resume: Option<oneshot::Receiver<()>>,
    }
    impl Persistence for PersistenceGate {
        fn enqueue(&mut self, artifact: PendingArtifacts) {
            self.memory.enqueue(artifact);
        }
        fn settle<'a>(
            &'a mut self,
            executions: &'a mut [ExecutionReport],
        ) -> JobFuture<'a, Vec<Warning>> {
            Box::pin(async move {
                assert_eq!(self.memory.pending.len(), 1);
                let meta = self.memory.pending[0].metadata.as_ref().unwrap();
                assert_eq!(meta.bytes(), METADATA);
                self.entered.take().unwrap().send(()).unwrap();
                self.resume.take().unwrap().await.unwrap();
                self.memory.settle(executions).await
            })
        }
    }
    runtime(async {
        let f = Fixture::new();
        let engine = test_engine(&f, "python", 1);
        let cancel = engine.cancellation();
        let (entered, wait) = oneshot::channel();
        let (resume, resumed) = oneshot::channel();
        let mut persistence = PersistenceGate {
            memory: MemoryPersistence::default(),
            entered: Some(entered),
            resume: Some(resumed),
        };
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let (report, ()) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), async {
                wait.await.unwrap();
                cancel.request_cancel(CancelSource::Api);
                resume.send(()).unwrap();
            });
        assert_eq!(report.data().result.verdict, JobVerdict::Passed);
        assert_eq!(
            report.data().result.cancellation.unwrap().phase,
            CancelPhase::Settlement
        );
        assert!(persistence.memory.pending.is_empty());
        assert!(
            report
                .data()
                .executions
                .iter()
                .all(|e| e.spec.as_ref().unwrap().path.is_none()
                    && e.record.as_ref().unwrap().error.is_some())
        );
    });
}

#[test]
fn command_stop_policy_and_successful_iteration_aggregates() {
    let f = Fixture::new();
    let mut fake = Fake::with([Behavior::Exit(0), Behavior::Exit(2)]);
    let report = runtime(run(
        command_engine(&f, CommandKind::Cmd, 3, false),
        &mut fake,
    ));
    assert_eq!(fake.runs.len(), 2);
    assert_eq!(
        report.data().executions[2].result.coverage,
        Coverage::Skipped {
            reason: SkipReason::StopPolicy
        }
    );
    assert_eq!(report.data().result.verdict, JobVerdict::NonzeroExit);
    let aggregate = &report.data().aggregates[0];
    assert_eq!((aggregate.iterations, aggregate.succeeded), (3, 1));
    assert_eq!(aggregate.wall, Some([Duration::from_millis(1); 3]));
}

#[test]
fn no_consumer_has_no_drops_even_with_output_requested() {
    let f = Fixture::new();
    let report = runtime(run(
        test_engine(&f, "python", 1).output_events(true),
        &mut Fake::default(),
    ));
    assert_eq!(report.data().events.dropped, 0);
}

#[test]
fn command_test_ranges_are_frozen_manifest_order_and_checks_are_null() {
    let f = Fixture::new();
    let ready = ready(&f, "python", 2, &Tools);
    let iterations = ready
        .manifest
        .entries()
        .iter()
        .flat_map(|test| {
            (1..=2).map(move |iteration| Iteration {
                role: Role::Command {
                    test: Some(test.id.to_string()),
                    iteration,
                },
                recipe: Recipe::TargetRun,
                stdin: Some(test.input.path.clone()),
                depends_on_build: false,
            })
        })
        .collect();
    let engine = Engine::new(
        ready,
        JobRequest::Iterations {
            kind: CommandKind::Cmd,
            iterations,
            needs_build: false,
            keep_going: true,
        },
    )
    .unwrap();
    let report = runtime(run(engine, &mut Fake::default()));
    assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
    assert_eq!(report.data().tests.len(), 2);
    for (index, test) in report.data().tests.iter().enumerate() {
        assert_eq!(test.first_execution, index as u32 * 2);
        assert_eq!(test.execution_count, 2);
        assert!(test.check.is_none() && test.verdict.is_none() && test.class.is_none());
    }
    assert!(report.data().target.is_none() && report.data().capture.is_none());
    assert_eq!(report.data().aggregates.len(), 2);
    assert_eq!(report.data().result.counts.tests, 0);
}

#[test]
fn orphan_skip_conditions_preserve_coverage_and_name_execution_locus() {
    let execution = report::validate_execution(
        ExecutionResult {
            coverage: Coverage::Skipped {
                reason: SkipReason::AdmissionClosed,
            },
            class: None,
            invariant: None,
        },
        &report::SkipConditions {
            job: JobKind::Cmd,
            depends_on_unsucceeded_build: false,
            earlier_retention_refusal: false,
            keep_going: true,
            earlier_primary_failed: false,
            intent_before_admission: false,
            after_test_hook: false,
            hooks_eligible: false,
        },
    );
    let counts = report::coverage_counts(std::slice::from_ref(&execution), &[], &[]);
    assert_eq!(counts.primary.skipped, 1);
    assert_eq!(counts.primary.invalid, 1);
    let result = report::job_verdict(&report::JobInput {
        kind: JobKind::Cmd,
        build: None,
        primary: &[execution],
        tests: &[],
        selected_tests: 0,
        internal_before_freeze: None,
        internal_after_freeze: None,
    });
    assert_eq!(result.verdict, JobVerdict::InternalError);
    assert_eq!(
        result.internal.unwrap().locus,
        InternalLocus::Execution { index: 0 }
    );
}

#[test]
fn stream_reservations_rollback_atomically_and_count_actual_writes() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let budget = Arc::new(JobBudget::new(METADATA + 10));
    assert!(reserve(&budget, 11).is_none());
    assert_eq!(budget.usage().reserved, 0);
    assert!(reserve(&budget, u64::MAX).is_none());
    let f = Fixture::new();
    let ready = ready(&f, "python", 0, &Tools);
    let (reservation, metadata) = reserve(&budget, 10).unwrap();
    let retained_bytes = Arc::new(AtomicU64::new(0));
    let resources = RunResources {
        reservation,
        _owner: ready.owner.clone(),
        retained_bytes: retained_bytes.clone(),
    };
    retained_bytes.fetch_add(4, Ordering::Release);
    drop(resources);
    assert_eq!(budget.usage().retained, 4);
    assert_eq!(budget.usage().reserved, METADATA);
    drop(metadata);
    assert_eq!(budget.usage().reserved, 0);
}

#[test]
fn unmapped_supervisor_failure_is_internal_with_record_retained() {
    let f = Fixture::new();
    let report = runtime(run(
        test_engine(&f, "python", 1),
        &mut Fake::with([Behavior::InvalidLimit]),
    ));
    let execution = &report.data().executions[0];
    assert!(matches!(
        execution.result.coverage,
        Coverage::NotStarted { .. }
    ));
    assert_eq!(
        execution.result.invariant,
        Some(Invariant::SummaryUnderivable)
    );
    assert!(
        execution.record.is_some()
            && execution.summary.is_none()
            && execution.result.class.is_none()
    );
    assert!(
        matches!(execution.evidence.as_deref(), Some(ExecutionRecord::NotStarted(failure)) if failure.invalid_limit == Some(supervisor::LimitValidation::OutOfRange))
    );
    assert_eq!(report.data().result.verdict, JobVerdict::InternalError);
}

#[test]
fn phase_callback_panics_leave_the_primary_verdict_frozen() {
    struct Panics(bool);
    impl PhaseHooks for Panics {
        fn fact<'a>(
            &'a mut self,
            _: JobVerdict,
            _: &'a mut dyn FnMut() -> bool,
        ) -> JobFuture<'a, ()> {
            assert!(self.0, "fact callback panic");
            Box::pin(async {})
        }
        fn hooks<'a>(
            &'a mut self,
            _: JobVerdict,
            start: &'a dyn Fn() -> Option<SkipReason>,
        ) -> JobFuture<'a, ()> {
            Box::pin(async move {
                if self.0 {
                    assert_eq!(start(), None);
                    panic!("hook callback panic");
                }
                assert_eq!(start(), Some(SkipReason::JobNotEligible));
            })
        }
    }
    for during_hooks in [false, true] {
        let f = Fixture::new();
        let report = runtime(test_engine(&f, "python", 1).run(
            &mut Fake::default(),
            &mut MemoryPersistence::default(),
            &mut Panics(during_hooks),
        ));
        assert_eq!(report.data().result.primary_verdict, JobVerdict::Passed);
        assert_eq!(report.data().result.verdict, JobVerdict::InternalError);
        assert_eq!(
            report.data().result.internal.as_ref().unwrap().locus,
            InternalLocus::Phase {
                phase: if during_hooks { "hooks" } else { "fact" }
            }
        );
    }
}
