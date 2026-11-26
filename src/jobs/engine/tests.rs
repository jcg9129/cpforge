use super::*;
use crate::jobs::{ReadyJob, prepare_build_job, prepare_job};
use crate::storage::inputs::CapturedSource;
use crate::storage::prune::PrunePolicy;
use crate::supervisor::{self, CancelToken, Stream};
use std::collections::VecDeque;
use std::path::PathBuf;
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
    ready_with_budget(f, language, selected, probe, 256 << 20)
}

fn ready_with_budget<'a>(
    f: &Fixture,
    language: &str,
    selected: usize,
    probe: &'a dyn crate::exec::Probe,
    job_bytes: u64,
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
    let registered = f.job(job_bytes, PrunePolicy::default());
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
    /// Cancel the job and report preparation as interrupted, as the creation
    /// thread does when cancellation stops it before the first mkdir.
    interrupt_prepare: Option<CancelState>,
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
    fn prepare_run(
        &mut self,
        preparation: RunPreparation,
        _cancel: CancelToken,
    ) -> JobFuture<'_, PreparedRun> {
        let interrupt = self.interrupt_prepare.take();
        Box::pin(async move {
            match interrupt {
                Some(cancel) => {
                    cancel.request_cancel(CancelSource::Api);
                    PreparedRun::interrupted()
                }
                None => preparation.prepare(None),
            }
        })
    }

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
        output_file: None,
        #[cfg(all(
            feature = "storage-test-hooks",
            any(target_os = "linux", target_os = "macos")
        ))]
        milestones: None,
        kill_attempts: 0,
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
        // Report §2.1: cancellation intent outranks the unsucceeded build.
        assert!(
            report.data().executions[1..]
                .iter()
                .all(|e| e.result.coverage
                    == Coverage::Skipped {
                        reason: SkipReason::Cancelled
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
fn late_cancellation_replaces_newest_ordinary_warning_at_capacity() {
    assert_warning_cap(true);
}

#[test]
fn warning_capacity_without_late_cancellation_keeps_all_24_ordinary_warnings() {
    assert_warning_cap(false);
}

fn assert_warning_cap(late_cancellation: bool) {
    struct Hooks(usize);
    impl PhaseHooks for Hooks {
        fn fact_evidence(&self) -> Option<(Fact, Vec<WireWarning>)> {
            Some((
                Fact {
                    outcome: FactOutcome::NotAttempted {
                        reason: "not_completed",
                    },
                    warnings: Vec::new(),
                },
                (0..self.0)
                    .map(|index| WireWarning {
                        kind: WarningKind::Durability {
                            artefact: "job_metadata",
                            detail: "uncertain",
                        },
                        message: format!("ordinary warning {index}"),
                    })
                    .collect(),
            ))
        }
    }

    // Exercise both warning buffers and their combined ordering.
    for detail_count in [0, 12, 24] {
        let f = Fixture::new();
        let mut ready = ready(&f, "python", 1, &Tools);
        assert!(warnings::admission(&ready.prune_report).is_empty());
        let admission_count = 24 - detail_count;
        ready.prune_report.selection.failed_probes = (0..admission_count)
            .map(|index| {
                (
                    f.path(&format!("unreadable-job-{index}")),
                    std::io::Error::other("probe unavailable"),
                )
            })
            .collect();
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: Comparator::Tokens,
            },
        )
        .unwrap();
        let cancel = engine.cancellation();
        let engine = engine.observe(move |point| {
            if late_cancellation && point == Point::BeforeSnapshot {
                cancel.request_cancel(CancelSource::Api);
            }
        });
        let report = runtime(engine.run(
            &mut Fake::default(),
            &mut MemoryPersistence::default(),
            &mut Hooks(detail_count),
        ));
        let data = report.data();
        assert_eq!(data.warnings.len() + data.details.warnings.len(), 24);
        assert_eq!(data.details.warnings_dropped, u32::from(late_cancellation));
        assert_eq!(data.result.cancellation.is_some(), late_cancellation);

        let json = crate::report::wire::read::read_json(
            &crate::report::wire::encode_report(&report).unwrap(),
        )
        .unwrap();
        let warnings = json["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 24);
        assert_eq!(json["warnings_dropped"], u32::from(late_cancellation));
        let ordinary_count = 24 - usize::from(late_cancellation);
        let kept_admission = admission_count.min(ordinary_count);
        for warning in &warnings[..kept_admission] {
            assert_eq!(warning["kind"], "prune");
            assert_eq!(warning["detail"], "lease_unreadable");
        }
        for (index, warning) in warnings[kept_admission..ordinary_count].iter().enumerate() {
            assert_eq!(warning["kind"], "durability");
            assert_eq!(warning["message"], format!("ordinary warning {index}"));
        }
        if late_cancellation {
            assert!(matches!(
                data.warnings.last(),
                Some(Warning::LateCancellation)
            ));
            assert_eq!(warnings[23]["kind"], "late_cancellation");
            assert_eq!(warnings[23]["phase"], "closure");
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
    assert_eq!(budget.usage().reserved, 0);
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
    // Only interrupted preparation may leave `program` and `spec` null.
    assert!(report.data().executions[0].program.is_some());
    assert!(report.data().executions[0].spec.is_some());
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
    // Cancelled after preparation completed: program and spec stay non-null.
    assert!(execution.program.is_some());
    assert!(execution.spec.as_ref().is_some_and(|s| s.digest.is_some()));
    assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
}

#[test]
fn cancellation_interrupting_run_preparation_nulls_program_and_spec() {
    use crate::report::wire;
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 1);
    let dir = engine.ready.dir.clone();
    let mut fake = Fake {
        interrupt_prepare: Some(engine.cancellation()),
        ..Fake::default()
    };
    let report = runtime(run(engine, &mut fake));
    assert!(fake.runs.is_empty());
    let execution = &report.data().executions[0];
    assert_eq!(
        execution.result.coverage,
        Coverage::NotStarted {
            reason: NotStartedReason::CancelledBeforeStart
        }
    );
    assert_eq!(execution.result.invariant, None);
    assert_eq!(execution.run.as_ref().unwrap().as_str(), "r00000");
    assert!(!dir.join("r00000").exists());
    assert!(execution.program.is_none() && execution.spec.is_none());
    assert!(execution.record.is_some());
    assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
    let value = wire::read::read_json(&wire::encode_report(&report).unwrap()).unwrap();
    let encoded = &value["executions"][0];
    assert_eq!(encoded["coverage"]["kind"], "not_started");
    assert_eq!(
        encoded["coverage"]["reason"]["kind"],
        "cancelled_before_start"
    );
    assert_eq!(encoded["run"], "r00000");
    assert!(encoded["program"].is_null() && encoded["spec"].is_null());
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
        fn enqueue(&mut self, artifact: PendingArtifacts) -> JobFuture<'_, ()> {
            self.memory.enqueue(artifact)
        }
        fn settle<'a>(
            &'a mut self,
            executions: &'a mut [ExecutionReport],
        ) -> JobFuture<'a, Vec<Warning>> {
            Box::pin(async move {
                assert_eq!(self.memory.pending.len(), 2);
                let total: u64 = self
                    .memory
                    .pending
                    .iter()
                    .filter_map(|p| p.metadata.as_ref())
                    .map(Reservation::bytes)
                    .sum();
                assert_eq!(total, METADATA);
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
            submit_not_succeeded: false,
            input_not_validated: false,
            not_eligible: false,
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
        roles: &[],
        scoring: crate::report::Scoring::PassFail,
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        file_io: None,
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
fn report_reservation_is_twice_the_plan_bound_plus_run_reservations() {
    runtime(async {
        for shape in [None, Some(crate::report::wire::ReportShape::default())] {
            let f = Fixture::new();
            let mut engine = test_engine(&f, "python", 1);
            if let Some(shape) = shape {
                engine = engine.report_shape(shape).unwrap();
            }
            let budget = engine.ready.budget.clone();
            let target = engine.ready.target.as_ref().unwrap();
            let streams = target.run.stdout.bytes.value + target.run.stderr.bytes.value;
            let expected = crate::report::wire::report_reservation(1, 1, 0, shape).unwrap();
            // Observer panics become internal evidence, so record and assert later.
            let at_admission = Arc::new(std::sync::Mutex::new(None));
            let (before_run_budget, seen) = (budget.clone(), at_admission.clone());
            let engine = engine.observe(move |point| {
                if point == Point::AfterAdmission(0) {
                    *seen.lock().unwrap() = Some(before_run_budget.usage().reserved);
                }
            });
            let (entered_tx, entered_rx) = oneshot::channel();
            let (resume_tx, resume_rx) = oneshot::channel();
            let mut fake = Fake {
                entered: Some(entered_tx),
                resume: Some(resume_rx),
                ..Fake::default()
            };
            let observe = async {
                entered_rx.await.unwrap();
                assert_eq!(budget.usage().reserved, expected + streams + METADATA);
                resume_tx.send(()).unwrap();
            };
            let (report, ()) = tokio::join!(run(engine, &mut fake), observe);
            assert_eq!(*at_admission.lock().unwrap(), Some(expected));
            assert_eq!(report.data().result.verdict, JobVerdict::Passed);
            assert_eq!(report.data().details.shape, shape);
            assert_eq!(budget.usage().reserved, 0);
        }
    });
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

#[test]
fn disk_persistence_publishes_frozen_bytes_and_terminal_only_after_report() {
    use crate::jobs::persist::JobPersistence;
    use crate::report::wire;
    struct RecordingFake {
        fake: Fake,
        specs: Vec<Arc<ExecutionSpec>>,
    }
    impl Executor for RecordingFake {
        fn execute(
            &mut self,
            spec: Arc<ExecutionSpec>,
            cancel: CancelToken,
            events: RunEvents,
            resources: RunResources,
        ) -> JobFuture<'_, ExecutionRecord> {
            self.specs.push(spec.clone());
            self.fake.execute(spec, cancel, events, resources)
        }
    }
    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let budget = ready.budget.clone();
    let dir = ready.dir.clone();
    let mut persistence = JobPersistence::new(&ready).unwrap();
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let mut receiver = engine.events().subscribe().unwrap();
    let mut fake = RecordingFake {
        fake: Fake::default(),
        specs: Vec::new(),
    };
    let report = runtime(engine.run(&mut fake, &mut persistence, &mut NoHooks));
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert!(matches!(
        report.persistence(),
        ReportPersistence::Published { .. }
    ));
    assert_eq!(fake.specs[0].digest, Some(fake.specs[0].spec_digest()));
    let spec_bytes = fs::read(dir.join("r00000/spec.json")).unwrap();
    assert_eq!(
        spec_bytes,
        wire::encode_spec(&fake.specs[0].wire_spec()).unwrap()
    );
    let record_bytes = fs::read(dir.join("r00000/record.json")).unwrap();
    assert_eq!(
        record_bytes,
        wire::encode_record(&wire::Record {
            platform: report.data().platform,
            spec: report.data().executions[0].spec.as_ref().unwrap(),
            execution: report.data().executions[0].evidence.as_ref().unwrap(),
        })
        .unwrap()
    );
    let report_bytes = fs::read(dir.join("report.json")).unwrap();
    assert_eq!(report_bytes, wire::encode_report(&report).unwrap());
    assert!(
        wire::read::read_json(&report_bytes)
            .unwrap()
            .get("persistence")
            .is_none()
    );
    assert_eq!(
        wire::read::read_json(&wire::encode_stdout_report(&report).unwrap()).unwrap()["persistence"]
            ["report"]["kind"],
        "published"
    );
    let capture =
        wire::read::read_json(&fs::read(dir.join("inputs/capture.json")).unwrap()).unwrap();
    assert_eq!(capture["document"], "capture_manifest");
    assert_eq!(
        report.data().capture.as_ref().unwrap().manifest.as_deref(),
        Some(Path::new("inputs/capture.json"))
    );
    assert!(dir.join("r00000/cleanup.log").is_file());
    let terminal = runtime(async {
        while let Some(event) = receiver.recv().await {
            wire::encode_event(&event).unwrap();
        }
        receiver.terminal().await
    });
    assert_eq!(terminal.report_id, Some(report.data().job.id.clone()));
    let encoded = wire::encode_job_finished(&terminal).unwrap();
    assert!(!wire::read::read_json(&encoded).unwrap()["report"].is_null());
    let reservation = budget
        .reserve(2 * wire::report_bound(1, 1, 0).unwrap() as u64)
        .unwrap();
    let repeated =
        runtime(persistence.publish_report(Arc::new(report.data().clone()), Some(reservation)));
    assert!(matches!(repeated, ReportPersistence::Failed { .. }));
    assert_eq!(fs::read(dir.join("report.json")).unwrap(), report_bytes);
}

#[test]
fn record_budget_refusal_warns_without_changing_verdict() {
    use crate::jobs::persist::JobPersistence;
    struct RefuseRecord {
        persistence: JobPersistence,
        budget: Arc<JobBudget>,
        blocker: Option<Reservation>,
    }
    impl Persistence for RefuseRecord {
        fn configure(&mut self, planned: u32, cancel: crate::supervisor::CancelToken) {
            self.persistence.configure(planned, cancel);
        }
        fn enqueue(&mut self, mut artifact: PendingArtifacts) -> JobFuture<'_, ()> {
            if artifact.finalized {
                artifact.metadata = None;
                let usage = self.budget.usage();
                self.blocker = Some(
                    self.budget
                        .reserve(self.budget.limit() - usage.retained - usage.reserved)
                        .unwrap(),
                );
                artifact.metadata = self.budget.reserve(METADATA).ok();
                assert!(artifact.metadata.is_none());
            }
            self.persistence.enqueue(artifact)
        }
        fn settle<'a>(
            &'a mut self,
            executions: &'a mut [ExecutionReport],
        ) -> JobFuture<'a, Vec<Warning>> {
            self.persistence.settle(executions)
        }
        fn publish_report(
            &mut self,
            report: Arc<ReportData>,
            reservation: Option<Reservation>,
        ) -> JobFuture<'_, ReportPersistence> {
            self.persistence.publish_report(report, reservation)
        }
    }
    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let mut persistence = RefuseRecord {
        persistence: JobPersistence::new(&ready).unwrap(),
        budget: ready.budget.clone(),
        blocker: None,
    };
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let report = runtime(engine.run(&mut Fake::default(), &mut persistence, &mut NoHooks));
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert!(matches!(
        report.data().executions[0].record.as_ref().unwrap().error,
        Some(PersistError::Io { .. })
    ));
    assert!(
        report.data().executions[0]
            .record
            .as_ref()
            .unwrap()
            .path
            .is_none()
    );
    assert!(report.data().warnings.iter().any(|w| matches!(
        w,
        Warning::Persistence {
            artefact: "record",
            error: PersistError::Io { .. },
            ..
        }
    )));
    assert!(matches!(
        report.persistence(),
        ReportPersistence::Published { .. }
    ));
}

#[test]
fn stalled_persistence_bounds_w_times_out_the_queue_and_omits_terminal_path() {
    use crate::jobs::persist::JobPersistence;
    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut persistence = JobPersistence::stalled(&ready, Duration::from_secs(1), gate.clone());
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let mut receiver = engine.events().subscribe().unwrap();
    let start = Instant::now();
    let report = runtime(async {
        tokio::time::timeout(
            Duration::from_secs(3),
            engine.run(&mut Fake::default(), &mut persistence, &mut NoHooks),
        )
        .await
        .expect("W must be bounded")
    });
    gate.notify_one();
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    for reference in [
        &report.data().executions[0].spec,
        &report.data().executions[0].record,
    ]
    .into_iter()
    .flatten()
    {
        assert_eq!(reference.error, Some(PersistError::Timeout));
        assert!(reference.path.is_none());
    }
    assert!(matches!(
        report.persistence(),
        ReportPersistence::Failed {
            error: PersistError::WorkerUnavailable,
            ..
        }
    ));
    let terminal = runtime(async {
        while receiver.recv().await.is_some() {}
        receiver.terminal().await
    });
    assert!(terminal.report_id.is_none());
    assert!(
        crate::report::wire::read::read_json(
            &crate::report::wire::encode_job_finished(&terminal).unwrap()
        )
        .unwrap()["report"]
            .is_null()
    );
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn late_persistence_rename_keeps_report_final_at_w_without_duplicate_warnings() {
    use crate::jobs::persist::{JobPersistence, RenameGate};
    use crate::report::wire;

    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let dir = ready.dir.clone();
    let gate = Arc::new(RenameGate::default());
    let mut persistence =
        JobPersistence::stalled_rename(&ready, Duration::from_secs(5), gate.clone());
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let mut receiver = engine.events().subscribe().unwrap();
    runtime(async {
        tokio::time::timeout(Duration::from_secs(8), async {
            let mut fake = Fake::default();
            let mut hooks = NoHooks;
            let (report, ()) =
                tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), async {
                    gate.reached.notified().await
                },);
            assert_eq!(report.data().result.verdict, JobVerdict::Passed);
            let execution = &report.data().executions[0];
            for reference in [&execution.spec, &execution.record] {
                let reference = reference.as_ref().expect("settled artefact");
                assert_eq!(reference.error, Some(PersistError::Timeout));
                assert!(reference.path.is_none());
            }
            let warnings: Vec<_> = report
                .data()
                .warnings
                .iter()
                .filter_map(|warning| match warning {
                    Warning::Persistence {
                        artefact,
                        run,
                        error,
                    } => {
                        assert_eq!(Some(run), execution.run.as_ref());
                        assert_eq!(*error, PersistError::Timeout);
                        Some(*artefact)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(warnings, ["spec", "record", "cleanup_log"]);
            assert!(matches!(
                report.persistence(),
                ReportPersistence::Failed {
                    error: PersistError::WorkerUnavailable,
                    ..
                }
            ));
            let frozen = wire::encode_stdout_report(&report).unwrap();
            let json = wire::read::read_json(&frozen).unwrap();
            assert_eq!(
                json["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|warning| warning["kind"] == "persist")
                    .count(),
                3
            );
            assert!(!dir.join("r00000/spec.json").exists());
            while receiver.recv().await.is_some() {}
            let terminal = receiver.terminal().await;
            assert!(terminal.report_id.is_none());
            let frozen_terminal = wire::encode_job_finished(&terminal).unwrap();

            // W and the terminal slot have settled before the syscall resumes.
            gate.release.notify_one();
            gate.completed.notified().await;
            let landed = fs::read(dir.join("r00000/spec.json")).unwrap();
            assert!(wire::read::read_json(&landed).is_ok());
            assert!(!dir.join("r00000/record.json").exists());
            assert!(!dir.join("r00000/cleanup.log").exists());
            assert!(!dir.join("report.json").exists());
            assert_eq!(wire::encode_stdout_report(&report).unwrap(), frozen);
            assert_eq!(
                wire::encode_job_finished(&terminal).unwrap(),
                frozen_terminal
            );
            assert!(receiver.recv().await.is_none());
            let mut executions = report.data().executions.clone();
            assert!(persistence.settle(&mut executions).await.is_empty());
        })
        .await
        .expect("late publication must not delay settlement or crash the job");
    });
}

#[test]
fn report_collision_fails_publication_without_rewriting_or_changing_verdict() {
    use crate::jobs::persist::JobPersistence;
    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let path = ready.dir.join("report.json");
    fs::write(&path, b"existing").unwrap();
    let mut persistence = JobPersistence::new(&ready).unwrap();
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let mut receiver = engine.events().subscribe().unwrap();
    let report = runtime(engine.run(&mut Fake::default(), &mut persistence, &mut NoHooks));
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert!(matches!(
        report.persistence(),
        ReportPersistence::Failed {
            error: PersistError::Io { .. },
            ..
        }
    ));
    let terminal = runtime(async {
        while receiver.recv().await.is_some() {}
        receiver.terminal().await
    });
    assert!(terminal.report_id.is_none());
    assert_eq!(fs::read(path).unwrap(), b"existing");
}

#[test]
fn pre_delivery_status_uses_report_precedence_and_ignores_persistence() {
    for (verdict, code) in [
        (JobVerdict::InternalError, 70),
        (JobVerdict::Infrastructure, 10),
        (JobVerdict::BuildFailed, 3),
        (JobVerdict::CheckerFailed, 5),
        (JobVerdict::WrongAnswer, 1),
        (JobVerdict::Unchecked, 4),
        (JobVerdict::NoTests, 4),
        (JobVerdict::Passed, 0),
        (JobVerdict::Cancelled, 6),
    ] {
        assert_eq!(
            pre_delivery_exit(JobKind::Test, verdict, None, None).code,
            code
        );
    }
    let intent = Cancellation {
        requested_at: Duration::ZERO,
        source: CancelSource::Api,
        phase: CancelPhase::Closure,
    };
    assert_eq!(
        pre_delivery_exit(
            JobKind::Test,
            JobVerdict::Infrastructure,
            Some(intent),
            None
        )
        .code,
        6
    );
    assert_eq!(
        pre_delivery_exit(JobKind::Test, JobVerdict::InternalError, Some(intent), None).code,
        70
    );
    assert_eq!(
        pre_delivery_exit(JobKind::Hook, JobVerdict::Infrastructure, None, None).code,
        0
    );
    assert_eq!(
        pre_delivery_exit(JobKind::Hook, JobVerdict::Cancelled, None, None).code,
        6
    );
    let console = Cancellation {
        source: CancelSource::Console,
        ..intent
    };
    let status = pre_delivery_exit(JobKind::Test, JobVerdict::Passed, Some(console), None);
    assert_eq!(status.code, if cfg!(unix) { 130 } else { 6 });
    assert_eq!(
        status.reraise,
        if cfg!(unix) { Some("sigint") } else { None }
    );
}

#[test]
fn stalled_report_publication_bounds_s_and_keeps_w_warnings_frozen() {
    use crate::jobs::persist::JobPersistence;
    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let dir = ready.dir.clone();
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut persistence =
        JobPersistence::stalled_report(&ready, Duration::from_secs(1), gate.clone());
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let mut receiver = engine.events().subscribe().unwrap();
    let report = runtime(async {
        tokio::time::timeout(
            Duration::from_secs(3),
            engine.run(&mut Fake::default(), &mut persistence, &mut NoHooks),
        )
        .await
        .expect("S must be bounded")
    });
    gate.notify_one();
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert!(
        report.data().executions[0]
            .spec
            .as_ref()
            .unwrap()
            .path
            .is_some()
    );
    assert!(
        report.data().executions[0]
            .record
            .as_ref()
            .unwrap()
            .path
            .is_some()
    );
    assert!(
        !report
            .data()
            .warnings
            .iter()
            .any(|warning| matches!(warning, Warning::Persistence { .. }))
    );
    assert!(matches!(
        report.persistence(),
        ReportPersistence::Failed {
            error: PersistError::Timeout,
            ..
        }
    ));
    let terminal = runtime(async {
        while receiver.recv().await.is_some() {}
        receiver.terminal().await
    });
    assert!(terminal.report_id.is_none());
    assert!(!dir.join("report.json").exists());
}

#[test]
fn ndjson_delivery_frames_fake_engine_and_preserves_the_terminal_barrier() {
    use crate::transport::Delivery;
    for output in [false, true] {
        let f = Fixture::new();
        let engine = test_engine(&f, "python", 2).output_events(output);
        let receiver = engine.events().subscribe().unwrap();
        let mut delivery = Delivery::new(Vec::new(), false);
        let report = runtime(async {
            let mut executor = Fake::default();
            let mut persistence = MemoryPersistence::default();
            let mut hooks = NoHooks;
            let (report, delivered) = tokio::join!(
                engine.run(&mut executor, &mut persistence, &mut hooks),
                delivery.events(receiver, JobKind::Test, output)
            );
            delivered.unwrap();
            report
        });
        let bytes = delivery.into_inner();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let lines: Vec<serde_json::Value> = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(lines[0]["document"], "event_stream");
        assert_eq!(lines[0]["output"], output);
        let events = &lines[1..lines.len() - 1];
        let sequence: Vec<u64> = events
            .iter()
            .map(|event| event["seq"].as_str().unwrap().parse().unwrap())
            .collect();
        assert_eq!(sequence[0], 0);
        assert!(sequence.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(events.last().unwrap()["event"], "job_finished");
        assert_eq!(lines.last().unwrap()["event"], "report");
        assert_eq!(
            lines.last().unwrap()["report"]["result"]["verdict"],
            "passed"
        );
        assert_eq!(
            events.iter().any(|event| event["event"] == "output_chunk"),
            output
        );
        assert_eq!(report.data().events.dropped, 0);
        assert_eq!(sequence.last().copied(), Some(events.len() as u64 - 1));
    }
}

#[test]
fn disconnected_delivery_does_not_stop_execution_or_persistence() {
    use crate::exit::ExitCode;
    use crate::transport::{Delivery, MachineMode, delivered_status};
    #[derive(Debug)]
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected EPIPE",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let path = ready.dir.join("report.json");
    let mut persistence = crate::jobs::persist::JobPersistence::new(&ready).unwrap();
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let receiver = engine.events().subscribe().unwrap();
    let mut delivery = Delivery::new(Broken, false);
    let mut fake = Fake::default();
    let (report, delivered) = runtime(async {
        let mut hooks = NoHooks;
        tokio::join!(
            engine.run(&mut fake, &mut persistence, &mut hooks),
            delivery.events(receiver, JobKind::Test, false)
        )
    });
    assert_eq!(fake.runs.len(), 1);
    assert!(path.is_file());
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert_eq!(report.data().events.dropped, 0);
    assert_eq!(
        delivered_status(&report, MachineMode::Events { output: false }, &delivered),
        ExitCode::Delivery
    );
}

#[test]
fn report_epipe_preserves_persistence_and_stops_output() {
    use crate::exit::ExitCode;
    use crate::transport::{Delivery, Failing, MachineMode, delivered_status};

    for offset in [0, 100] {
        for code in [0, 9] {
            let f = Fixture::new();
            let ready = ready(&f, "python", 1, &Tools);
            let path = ready.dir.join("report.json");
            let mut persistence = crate::jobs::persist::JobPersistence::new(&ready).unwrap();
            let engine = Engine::new(
                ready,
                JobRequest::Test {
                    comparator: Comparator::Tokens,
                },
            )
            .unwrap();
            let report = runtime(async {
                tokio::time::timeout(
                    Duration::from_secs(20),
                    engine.run(
                        &mut Fake::with([Behavior::Exit(code)]),
                        &mut persistence,
                        &mut NoHooks,
                    ),
                )
                .await
                .expect("report persistence deadline")
            });
            let persisted = fs::read(&path).unwrap();
            let document = crate::report::wire::encode_stdout_report(&report).unwrap();
            assert!(document.len() > 100);
            let mut delivery = Delivery::new(
                Failing {
                    bytes: Vec::new(),
                    limit: offset,
                    flush_fails: false,
                },
                false,
            );
            let result = delivery.report(&report);
            assert!(delivery.failed());
            assert_eq!(
                delivered_status(&report, MachineMode::Json, &result),
                ExitCode::Delivery
            );
            assert_eq!(
                report.data().details.pre_delivery_exit.code,
                if code == 0 { 0 } else { 1 }
            );
            // Neither retrying the report nor the panic fallback may append a
            // second document after the first write error.
            assert!(delivery.report(&report).is_err());
            delivery
                .escaped_job_panic(JobKind::Test, MachineMode::Json)
                .unwrap();
            assert_eq!(delivery.into_inner().bytes, document[..offset]);
            assert_eq!(fs::read(&path).unwrap(), persisted);
            let saved: serde_json::Value = serde_json::from_slice(&persisted).unwrap();
            assert_eq!(saved["document"], "job_report");
            assert_eq!(
                saved["result"]["verdict"],
                if code == 0 { "passed" } else { "nonzero_exit" }
            );
        }
    }
}

#[test]
fn event_overflow_delivers_final_report() {
    use crate::exit::ExitCode;
    use crate::transport::{Delivery, MachineMode, delivered_status};

    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let path = ready.dir.join("report.json");
    let mut persistence = crate::jobs::persist::JobPersistence::new(&ready).unwrap();
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let events = engine.events();
    let receiver = events.subscribe().unwrap();
    let engine = engine.observe(move |point| {
        if point == Point::Closure {
            // Keep the consumer connected but unread through phase E. Fill the
            // real 8 MiB queue with cheap events, without launching 10 000 jobs.
            for done in 0..10_000 {
                events.offer(EventPayload::Progress {
                    done,
                    total: 10_000,
                });
            }
        }
    });
    let mut delivery = Delivery::new(Vec::new(), false);
    let (report, delivered) = runtime(async {
        tokio::time::timeout(Duration::from_secs(20), async {
            let report = engine
                .run(&mut Fake::default(), &mut persistence, &mut NoHooks)
                .await;
            let delivered = delivery.events(receiver, JobKind::Test, false).await;
            (report, delivered)
        })
        .await
        .expect("event delivery deadline")
    });
    assert_eq!(
        delivered_status(&report, MachineMode::Events { output: false }, &delivered),
        ExitCode::Success
    );
    delivered.unwrap();
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert!(report.data().events.dropped > 0);
    let bytes = delivery.into_inner();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let lines: Vec<serde_json::Value> = bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(lines[0]["document"], "event_stream");
    assert_eq!(lines[1]["event"], "job_started");
    let terminal = &lines[lines.len() - 2];
    assert_eq!(terminal["event"], "job_finished");
    let sequence: Vec<u64> = lines[1..lines.len() - 1]
        .iter()
        .map(|event| event["seq"].as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(sequence[0], 0);
    assert!(sequence.windows(2).all(|pair| pair[0] < pair[1]));
    let retained = (lines.len() - 3) as u64;
    assert_eq!(retained, (EVENT_BUDGET / EVENT_BOUND) as u64);
    let dropped = report.data().events.dropped;
    assert_eq!(sequence.last().unwrap() - retained, dropped);
    assert_eq!(terminal["dropped"], dropped.to_string());
    let final_line = lines.last().unwrap();
    assert_eq!(final_line["event"], "report");
    assert_eq!(
        final_line["report"]["events"]["dropped"],
        dropped.to_string()
    );
    assert_eq!(
        final_line["report"]["persistence"]["report"]["kind"],
        "published"
    );
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(final_line["report"]["events"], saved["events"]);
    assert_eq!(final_line["report"]["result"], saved["result"]);
}

#[test]
fn progress_drop_warning_is_frozen_at_e() {
    let f = Fixture::new();
    let queue = crate::progress::ProgressQueue::new(1);
    for _ in 0..129 {
        queue.offer(&"x".repeat(511));
    }
    let engine = test_engine(&f, "python", 1).progress(queue.clone());
    let report = runtime(run(engine, &mut Fake::default()));
    // One overflow plus all 128 queued progress lines detached at E. The
    // engine's per-test result is lossless and never enters this count.
    assert_eq!(queue.close(), 129);
    queue.offer("after E");
    assert_eq!(queue.close(), 129);
    assert!(
        report
            .data()
            .details
            .warnings
            .iter()
            .any(|warning| matches!(warning.kind, WarningKind::ProgressDropped { lines: 129 }))
    );
}

// Run this helper in a subprocess only: changing process signal dispositions
// would interfere with unrelated tests in the test harness.
#[cfg(unix)]
#[test]
fn signal_job_helper() {
    use crate::cancel_signals::CancelSignals;
    use crate::transport::{Delivery, MachineMode, delivered_status};
    let Some(mode) = std::env::var_os("CPF_SIGNAL_JOB_HELPER") else {
        return;
    };
    crate::process::initialize().unwrap();
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 1);
    let signals = CancelSignals::install(engine.cancellation()).unwrap();
    let (entered, wait) = oneshot::channel();
    let mut fake = Fake {
        behavior: [Behavior::WaitCancel].into(),
        entered: Some(entered),
        ..Fake::default()
    };
    let report = runtime(async {
        let (report, ()) = tokio::join!(run(engine, &mut fake), async {
            wait.await.unwrap();
            eprintln!("SIGNAL_READY");
        });
        report
    });
    let mut delivery = Delivery::stdout();
    let delivered = if mode == "broken" {
        Err(crate::error::Error::Delivery(
            "injected delivery failure".into(),
        ))
    } else {
        delivery.report(&report)
    };
    let status = delivered_status(&report, MachineMode::Json, &delivered);
    // Complete fixture cleanup before a successful delivery re-raises the signal.
    drop(report);
    drop(f);
    if mode == "force" {
        // Allow the contract's debounce interval to pass, then expose a
        // deterministic barrier for the forcing notification.
        std::thread::sleep(Duration::from_millis(110));
        eprintln!("FORCE_READY");
        loop {
            std::thread::park();
        }
    }
    std::process::exit(signals.finish(status) as i32);
}

#[cfg(unix)]
#[test]
fn signal_origin_reraises_after_report_and_delivery_failure_suppresses_it() {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    for (signal, mode) in [
        (libc::SIGINT, "report"),
        (libc::SIGTERM, "report"),
        (libc::SIGINT, "broken"),
        (libc::SIGINT, "force"),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "jobs::engine::tests::signal_job_helper",
                "--nocapture",
            ])
            .env("CPF_SIGNAL_JOB_HELPER", mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (ready, wait) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                if line == "SIGNAL_READY" || line == "FORCE_READY" {
                    let _ = ready.send(());
                }
            }
        });
        if wait.recv_timeout(Duration::from_secs(30)).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("signal helper did not reach its fake run");
        }
        let mut stdout = child.stdout.take().unwrap();
        let output_reader = std::thread::spawn(move || {
            let mut output = String::new();
            stdout.read_to_string(&mut output).unwrap();
            output
        });
        // SAFETY: this is the still-owned child PID, waiting at its fake run.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        if mode == "force" {
            if wait.recv_timeout(Duration::from_secs(30)).is_err() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("signal helper did not reach its delivery barrier");
            }
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("signal helper did not exit");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let output = output_reader.join().unwrap();
        reader.join().unwrap();
        if mode == "broken" {
            assert_eq!(status.code(), Some(11));
        } else {
            assert_eq!(
                status.signal(),
                Some(if mode == "force" {
                    libc::SIGTERM
                } else {
                    signal
                })
            );
            let report: serde_json::Value = output
                .lines()
                .find_map(|line| {
                    line.find('{')
                        .and_then(|start| serde_json::from_str(&line[start..]).ok())
                })
                .expect("complete report line before death");
            assert_eq!(report["document"], "job_report");
            assert_eq!(report["result"]["verdict"], "cancelled");
            assert_eq!(
                report["result"]["cancellation"]["source"],
                if signal == libc::SIGINT {
                    "console"
                } else {
                    "terminate"
                }
            );
        }
    }
}

#[test]
fn blocked_stdout_worker_does_not_delay_execution_or_report_publication() {
    use crate::transport::Delivery;
    #[derive(Debug)]
    struct Blocked {
        entered: Option<std::sync::mpsc::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl std::io::Write for Blocked {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new();
    let ready = ready(&f, "python", 1, &Tools);
    let path = ready.dir.join("report.json");
    let mut persistence = crate::jobs::persist::JobPersistence::new(&ready).unwrap();
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let receiver = engine.events().subscribe().unwrap();
    let (entered, wait) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let worker = Delivery::new(
        Blocked {
            entered: Some(entered),
            release: blocked,
        },
        false,
    )
    .start_events(receiver, JobKind::Test, false)
    .unwrap();
    wait.recv_timeout(Duration::from_secs(5)).unwrap();
    let report = runtime(engine.run(&mut Fake::default(), &mut persistence, &mut NoHooks));
    assert!(path.is_file());
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    release.send(()).unwrap();
    worker.finish().unwrap();
}

#[test]
fn caught_post_freeze_panic_preserves_report_and_abandons_delivery() {
    use crate::exit::ExitCode;
    use crate::transport::{Delivery, MachineMode, delivered_status};
    let f = Fixture::new();
    let engine = test_engine(&f, "python", 1).observe(|point| {
        assert_ne!(point, Point::AfterSnapshot, "injected post-freeze panic");
    });
    let receiver = engine.events().subscribe().unwrap();
    let report = runtime(run(engine, &mut Fake::default()));
    assert_eq!(report.data().result.verdict, JobVerdict::Passed);
    assert_eq!(report.data().details.pre_delivery_exit.code, 0);
    assert!(report.post_freeze_internal());
    let mut delivery = Delivery::new(Vec::new(), false);
    let result = delivery.report(&report);
    assert_eq!(
        delivered_status(&report, MachineMode::Json, &result),
        ExitCode::Internal
    );
    assert!(delivery.into_inner().is_empty());
    let mut stream = Delivery::new(Vec::new(), false);
    let result = runtime(stream.events(receiver, JobKind::Test, false));
    assert_eq!(result.unwrap_err().exit_code(), ExitCode::Internal);
    let bytes = String::from_utf8(stream.into_inner()).unwrap();
    assert!(!bytes.contains("\"event\":\"report\""));
    assert!(!bytes.contains("\"event\":\"job_finished\""));
}

#[test]
fn cancelled_command_iteration_marks_remaining_iterations_cancelled() {
    runtime(async {
        for keep_going in [false, true] {
            let f = Fixture::new();
            let engine = command_engine(&f, CommandKind::Cmd, 3, keep_going);
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
            assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
            assert!(
                report.data().executions[1..]
                    .iter()
                    .all(|e| e.result.coverage
                        == Coverage::Skipped {
                            reason: SkipReason::Cancelled
                        })
            );
            assert_eq!(fake.runs.len(), 1);
        }
    });
}

fn with_after_test(engine: Engine<'static>, count: usize) -> Engine<'static> {
    let Some(PlannedTarget::BuildFree(free)) = &engine.ready.planned_target else {
        panic!("fixture requires a build-free target")
    };
    let stage = free.run(engine.ready.dir.join("hook/cwd")).unwrap();
    let iterations = (0..count)
        .map(|index| Iteration {
            role: Role::Hook {
                name: format!("h{index}"),
                trigger: "after_test".into(),
            },
            recipe: Recipe::Resolved(Box::new(stage.clone())),
            stdin: None,
            depends_on_build: false,
        })
        .collect();
    engine.after_test(iterations).unwrap()
}

#[test]
fn after_test_internal_error_preserves_primary_and_skips_later_hooks() {
    let f = Fixture::new();
    let engine = with_after_test(test_engine(&f, "python", 1), 2);
    let report = runtime(engine.run(
        &mut Fake::with([Behavior::Exit(0), Behavior::Panic]),
        &mut MemoryPersistence::default(),
        &mut NoHooks,
    ));
    let data = report.data();
    assert_eq!(data.result.primary_verdict, JobVerdict::Passed);
    assert_eq!(data.result.verdict, JobVerdict::InternalError);
    assert_eq!(data.details.pre_delivery_exit.code, 70);
    assert!(matches!(
        data.details.hooks[0].outcome,
        HookResult::Planned(report::HookOutcome::InternalError { .. })
    ));
    assert!(matches!(
        data.details.hooks[1].outcome,
        HookResult::Planned(report::HookOutcome::Skipped {
            reason: SkipReason::JobNotEligible
        })
    ));
    assert_eq!(data.result.counts.primary.planned, 1);
    assert_eq!(data.result.counts.hooks.planned, 2);
    assert_eq!(data.result.counts.hooks.skipped, 1);
}

#[test]
fn after_test_checks_intent_before_each_hook_and_keeps_primary_counts_separate() {
    let f = Fixture::new();
    let engine = with_after_test(test_engine(&f, "python", 1), 3);
    let cancel = engine.cancellation();
    let engine = engine.observe(move |point| {
        if point == Point::BeforeHookStart(2) {
            cancel.request_cancel(CancelSource::Api);
        }
    });
    let mut fake = Fake::with([Behavior::Exit(0), Behavior::Exit(19)]);
    let report = runtime(engine.run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks));
    let data = report.data();
    assert_eq!(data.result.verdict, JobVerdict::Passed);
    assert_eq!(data.result.primary_verdict, JobVerdict::Passed);
    assert_eq!(data.details.pre_delivery_exit.code, 6);
    assert_eq!(data.result.cancellation.unwrap().phase, CancelPhase::Hooks);
    assert_eq!(fake.runs.len(), 2);
    assert!(matches!(
        data.details.hooks[0].outcome,
        HookResult::Planned(report::HookOutcome::Failed { .. })
    ));
    for hook in &data.details.hooks[1..] {
        assert!(matches!(
            hook.outcome,
            HookResult::Planned(report::HookOutcome::Skipped {
                reason: SkipReason::Cancelled
            })
        ));
    }
    assert_eq!(data.result.counts.primary.planned, 1);
    assert_eq!(data.result.counts.hooks.started, 1);
    assert_eq!(data.result.counts.hooks.skipped, 2);
}

/// Stderr is held from before E through S and D. The gate is independent of
/// the queue mutex, so engine finalization and document delivery can finish.
struct HeldStderr {
    writer: crate::progress::ProgressWriter,
    release: std::sync::mpsc::Sender<()>,
    bytes: Arc<std::sync::Mutex<Vec<u8>>>,
}
impl HeldStderr {
    fn new(planned_tests: usize) -> Self {
        use std::io::{self, Write};
        use std::sync::{Mutex, mpsc};
        struct Sink {
            entered: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
            bytes: Arc<Mutex<Vec<u8>>>,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                    // A failing test drops the release sender while unwinding:
                    // end the gate quietly instead of panicking this thread.
                    let _ = self.release.recv_timeout(Duration::from_secs(300));
                }
                self.bytes.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (entered, waiting) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = crate::progress::ProgressWriter::new(
            Sink {
                entered: Some(entered),
                release: gate,
                bytes: bytes.clone(),
            },
            planned_tests,
        )
        .unwrap();
        writer.queue().result("startup diagnostic");
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        Self {
            writer,
            release,
            bytes,
        }
    }
    fn finish(self, lines: &[String]) -> String {
        self.release.send(()).unwrap();
        self.writer.finish_lines(lines).unwrap();
        String::from_utf8(self.bytes.lock().unwrap().clone()).unwrap()
    }
}

#[test]
fn progress_drop_finality_across_e_s_d_matches_saved_stdout_and_events() {
    use crate::transport::Delivery;
    let f = Fixture::new();
    let stderr = HeldStderr::new(2);
    let queue = stderr.writer.queue();
    // One overflow, plus 128 queued entries detached at E. The writer took
    // only the startup result, so all offered progress must be counted.
    for _ in 0..129 {
        queue.offer(&"x".repeat(511));
    }
    let ready = ready(&f, "python", 2, &Tools);
    let path = ready.dir.join("report.json");
    let mut persistence = crate::jobs::persist::JobPersistence::new(&ready).unwrap();
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap()
    .progress(queue.clone());
    let receiver = engine.events().subscribe().unwrap();
    let mut delivery = Delivery::new(Vec::new(), false);
    let report = runtime(async {
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let (report, delivered) = tokio::join!(
            engine.run(&mut fake, &mut persistence, &mut hooks),
            delivery.events(receiver, JobKind::Test, false)
        );
        delivered.unwrap();
        report
    });
    assert!(stderr.bytes.lock().unwrap().is_empty());
    assert_eq!(queue.close(), 129);
    assert!(!queue.offer("after D"));
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut json = Delivery::new(Vec::new(), false);
    json.report(&report).unwrap();
    let stdout: serde_json::Value = serde_json::from_slice(&json.into_inner()).unwrap();
    let events = delivery.into_inner();
    let terminal: serde_json::Value = serde_json::from_slice(
        events
            .split(|b| *b == b'\n')
            .rfind(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    let dropped = |doc: &serde_json::Value| {
        doc["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["kind"] == "progress_dropped")
            .unwrap()["lines"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(dropped(&saved), 129);
    assert_eq!(dropped(&stdout), 129);
    assert_eq!(dropped(&terminal["report"]), 129);
    let text = stderr.finish(&[
        "warning: 129 progress lines dropped".into(),
        "summary".into(),
    ]);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines[0], "startup diagnostic");
    assert!(lines[1].starts_with("t0  OK"));
    assert!(lines[2].starts_with("t1  OK"));
    assert_eq!(
        &lines[3..],
        &["warning: 129 progress lines dropped", "summary"]
    );
    assert_eq!(queue.close(), 129);
}

#[test]
fn ten_thousand_execution_flood_keeps_ten_ordered_test_aggregates() {
    let f = Fixture::new();
    // The report reservation is 2 x (128 KiB + 9 KiB x 10 000 + 3 KiB x 10
    // + 256 x 10) = 184 648 704 bytes (report contract §3.7). The 1 GiB
    // budget keeps this ordering test independent of run admission margins.
    let ready = ready_with_budget(&f, "python", 10, &Tools, 1 << 30);
    let mut iterations = Vec::new();
    for test in ready.manifest.entries() {
        for iteration in 1..=1000 {
            iterations.push(Iteration {
                role: Role::Command {
                    test: Some(test.id.to_string()),
                    iteration,
                },
                recipe: Recipe::TargetRun,
                stdin: None,
                depends_on_build: false,
            });
        }
    }
    let stderr = HeldStderr::new(10);
    let queue = stderr.writer.queue();
    let engine = Engine::new(
        ready,
        JobRequest::Iterations {
            kind: CommandKind::Cmd,
            iterations,
            needs_build: false,
            keep_going: true,
        },
    )
    .unwrap()
    .progress(queue.clone());
    // An immediate sink double releases each reservation at enqueue. This
    // fixture concerns stderr, so it must not retain 10 000 metadata bundles
    // until W as MemoryPersistence intentionally does.
    struct ImmediatePersistence;
    impl Persistence for ImmediatePersistence {
        fn enqueue(&mut self, _: PendingArtifacts) -> JobFuture<'_, ()> {
            Box::pin(async {})
        }
        fn settle<'a>(
            &'a mut self,
            executions: &'a mut [ExecutionReport],
        ) -> JobFuture<'a, Vec<Warning>> {
            Box::pin(async move {
                persistence::unavailable(executions);
                Vec::new()
            })
        }
    }
    let report = runtime(engine.run(
        &mut Fake::default(),
        &mut ImmediatePersistence,
        &mut NoHooks,
    ));
    assert_eq!(report.data().executions.len(), 10_000);
    assert_eq!(report.data().tests.len(), 10);
    assert_eq!(report.data().aggregates.len(), 10);
    assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
    assert_eq!(queue.close(), 10_000);
    assert!(
        report
            .data()
            .details
            .warnings
            .iter()
            .any(|warning| matches!(warning.kind, WarningKind::ProgressDropped { lines: 10_000 }))
    );
    let text = stderr.finish(&["warning: 10000 progress lines dropped".into(), "OK".into()]);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 13);
    for (n, line) in lines[1..11].iter().enumerate() {
        assert!(
            line.starts_with(&format!("t{n}  OK  1000/1000 succeeded")),
            "{line}"
        );
        assert!(line.contains("wall min/median/max"));
    }
    assert_eq!(
        &lines[11..],
        &["warning: 10000 progress lines dropped", "OK"]
    );
    assert_eq!(queue.close(), 10_000);
}

struct ReleasePersistenceGate(Arc<tokio::sync::Notify>);
impl Drop for ReleasePersistenceGate {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

// Match the CLI's stack for fixtures that drive the complete engine. The
// injected executor keeps these persistence fixtures portable to Windows.
fn run_test<F: Future<Output = ()>>(future: impl FnOnce() -> F + Send) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn_scoped(scope, move || {
                runtime(async {
                    tokio::time::timeout(Duration::from_secs(120), future())
                        .await
                        .expect("engine fixture timed out");
                });
            })
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}

#[test]
fn scripted_persistence_publishes_three_thousand_complete_executions_then_report() {
    run_test(move || async move {
        use crate::jobs::persist::JobPersistence;
        let f = Fixture::new();
        let ready = ready(&f, "python", 3000, &Tools);
        let (mut persistence, driver, writer) = JobPersistence::scripted(&ready);
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: Comparator::Tokens,
            },
        )
        .unwrap();
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let (report, ()) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), driver);
        assert_eq!(report.data().result.verdict, JobVerdict::Passed);
        assert!(matches!(
            report.persistence(),
            ReportPersistence::Published { .. }
        ));
        assert!(
            !report
                .data()
                .warnings
                .iter()
                .any(|w| matches!(w, Warning::Persistence { .. }))
        );
        let files = writer.files.lock().unwrap();
        assert_eq!(files.len(), 9001);
        for execution in &report.data().executions {
            let spec = execution.spec.as_ref().unwrap();
            let record = execution.record.as_ref().unwrap();
            assert!(spec.error.is_none() && record.error.is_none());
            let spec_path = spec.path.as_ref().unwrap();
            let record_path = record.path.as_ref().unwrap();
            let stored_spec = crate::report::wire::read::read_json(&files[spec_path]).unwrap();
            assert_eq!(stored_spec["document"], "execution_spec");
            let stored_record = crate::report::wire::read::read_json(&files[record_path]).unwrap();
            // Stored records name the spec with `/` on every platform.
            let wire_spec_path = spec_path
                .iter()
                .map(|part| part.to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            assert_eq!(
                stored_record["spec"]["path"].as_str(),
                Some(&*wire_spec_path)
            );
            assert!(
                files[&record_path.with_file_name("cleanup.log")].starts_with(b"record finalized")
            );
        }
        let mut delivered: serde_json::Value =
            serde_json::from_slice(&crate::report::wire::encode_stdout_report(&report).unwrap())
                .unwrap();
        delivered.as_object_mut().unwrap().remove("persistence");
        let saved =
            crate::report::wire::read::read_json(&files[std::path::Path::new("report.json")])
                .unwrap();
        assert_eq!(saved, delivered);
        let stats = persistence.stats();
        assert_eq!(stats.last_write, Some(PathBuf::from("report.json")));
        assert!(stats.peak_credits <= 2 * 1024 * 1024);
        assert!(stats.credit_wait <= Duration::from_secs(46));
    });
}

#[test]
fn empty_accepted_set_skips_w_and_publishes_its_report() {
    run_test(move || async move {
        use crate::jobs::persist::JobPersistence;
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let (mut persistence, driver, writer) = JobPersistence::scripted(&ready);
        let engine = Engine::new(ready, JobRequest::Build).unwrap();
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let (report, ()) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), driver);
        assert!(report.data().executions.is_empty());
        assert!(matches!(
            report.persistence(),
            ReportPersistence::Published { .. }
        ));
        assert_eq!(writer.files.lock().unwrap().len(), 1);
        assert_eq!(persistence.stats().credit_wait, Duration::ZERO);
        assert_eq!(persistence.stats().peak_credits, 0);
    });
}

#[test]
fn saturated_persistence_waits_500ms_then_publishes_everything_and_report() {
    run_test(move || async move {
        use crate::jobs::persist::JobPersistence;
        let f = Fixture::new();
        let ready = ready(&f, "python", 100, &Tools);
        let gate = Arc::new(tokio::sync::Notify::new());
        let _release = ReleasePersistenceGate(gate.clone());
        let mut persistence = JobPersistence::controlled(&ready, gate.clone());
        let (writer, mut writes) = crate::jobs::persist::scripted::ScriptedWriter::new(false);
        *persistence.script.lock().unwrap() = Some(writer);
        let pool = persistence.pool.clone();
        let clock = persistence.clock.clone();
        let directory = ready.dir.clone();
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: Comparator::Tokens,
            },
        )
        .unwrap();
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let (report, ()) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), async {
                pool.waiting.notified().await;
                clock.advance(Duration::from_millis(500));
                gate.notify_one();
                while let Some(write) = writes.recv().await {
                    clock.advance(Duration::from_millis(4));
                    let report = write.path == std::path::Path::new("report.json");
                    let _ = write.release.send(());
                    if report {
                        break;
                    }
                }
            });
        assert_eq!(report.data().result.verdict, JobVerdict::Passed);
        assert!(
            !report
                .data()
                .warnings
                .iter()
                .any(|w| matches!(w, Warning::Persistence { .. }))
        );
        assert!(matches!(
            report.persistence(),
            ReportPersistence::Published { .. }
        ));
        for execution in &report.data().executions {
            for reference in [&execution.spec, &execution.record] {
                assert!(
                    directory
                        .join(reference.as_ref().unwrap().path.as_ref().unwrap())
                        .is_file()
                );
            }
        }
        assert!(
            crate::report::wire::read::read_json(&fs::read(directory.join("report.json")).unwrap())
                .is_ok()
        );
        assert!(persistence.stats().credit_wait >= Duration::from_millis(500));
        assert!(persistence.stats().credit_wait <= Duration::from_millis(11_200));
        assert_eq!(
            persistence.stats().last_write,
            Some(PathBuf::from("report.json"))
        );
    });
}

#[test]
fn cancellation_in_credit_wait_preserves_accepted_evidence_and_report() {
    run_test(move || async move {
        use crate::jobs::persist::JobPersistence;
        let f = Fixture::new();
        let ready = ready(&f, "python", 100, &Tools);
        let gate = Arc::new(tokio::sync::Notify::new());
        let _release = ReleasePersistenceGate(gate.clone());
        let mut persistence = JobPersistence::controlled(&ready, gate.clone());
        let (writer, mut writes) = crate::jobs::persist::scripted::ScriptedWriter::new(false);
        *persistence.script.lock().unwrap() = Some(writer);
        let pool = persistence.pool.clone();
        let clock = persistence.clock.clone();
        let directory = ready.dir.clone();
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: Comparator::Tokens,
            },
        )
        .unwrap();
        let cancel = engine.cancellation();
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let (report, ()) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), async {
                pool.waiting.notified().await;
                clock.advance(Duration::from_millis(500));
                cancel.request_cancel(CancelSource::Api);
                gate.notify_one();
                while let Some(write) = writes.recv().await {
                    clock.advance(Duration::from_millis(4));
                    let report = write.path == std::path::Path::new("report.json");
                    let _ = write.release.send(());
                    if report {
                        break;
                    }
                }
            });
        assert_eq!(report.data().details.pre_delivery_exit.code, 6);
        assert!(report.data().result.cancellation.is_some());
        let references: Vec<_> = report
            .data()
            .executions
            .iter()
            .flat_map(|e| [&e.spec, &e.record])
            .flatten()
            .collect();
        // The waiting artefact may be a spec, a record or a `cleanup.log`;
        // every refusal of a run's artefact is a `persist` warning, and only
        // specs and records also carry a `Ref`.
        assert!(report.data().warnings.iter().any(|w| matches!(
            w,
            Warning::Persistence {
                error: PersistError::Cancelled,
                ..
            }
        )));
        for reference in &references {
            assert!(
                reference.path.is_some() || reference.error == Some(PersistError::Cancelled),
                "{reference:?}"
            );
        }
        assert!(references.iter().any(|r| r.path.is_some()));
        for reference in references.into_iter().filter(|r| r.path.is_some()) {
            assert!(directory.join(reference.path.as_ref().unwrap()).is_file());
        }
        assert!(matches!(
            report.persistence(),
            ReportPersistence::Published { .. }
        ));
        let document: serde_json::Value =
            serde_json::from_slice(&crate::report::wire::encode_stdout_report(&report).unwrap())
                .unwrap();
        assert!(
            document["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["kind"] == "persist" && w["error"]["kind"] == "cancelled")
        );
    });
}

#[test]
fn controlled_stalled_worker_has_honest_errors_and_bounded_w_without_cancelling_deadlines() {
    run_test(move || async move {
        use crate::jobs::persist::JobPersistence;
        let f = Fixture::new();
        let ready = ready(&f, "python", 100, &Tools);
        let gate = Arc::new(tokio::sync::Notify::new());
        let _release = ReleasePersistenceGate(gate.clone());
        let mut persistence = JobPersistence::controlled(&ready, gate.clone());
        let pool = persistence.pool.clone();
        let clock = persistence.clock.clone();
        let call = clock.now();
        let budget = ready.budget.clone();
        let settlement = Arc::new(tokio::sync::Notify::new());
        let reached = settlement.clone();
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: Comparator::Tokens,
            },
        )
        .unwrap()
        .observe(move |point| {
            if point == Point::Settlement {
                reached.notify_one();
            }
        });
        let mut receiver = engine.events().subscribe().unwrap();
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let (report, ()) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), async {
                pool.waiting.notified().await;
                clock.advance(Duration::from_secs(1));
                settlement.notified().await;
                assert_eq!(clock.now(), call + Duration::from_secs(1));
                clock.advance(Duration::from_secs(4));
            });
        assert_eq!(clock.now(), call + Duration::from_secs(5));
        assert_eq!(persistence.stats().credit_wait, Duration::from_secs(1));
        assert_eq!(
            persistence.stats().settlement_elapsed,
            Some(Duration::from_secs(4))
        );
        assert_eq!(report.data().result.verdict, JobVerdict::Passed);
        let references: Vec<_> = report
            .data()
            .executions
            .iter()
            .flat_map(|e| [&e.spec, &e.record])
            .flatten()
            .collect();
        assert!(
            references
                .iter()
                .any(|r| r.error == Some(PersistError::Timeout))
        );
        for reference in references {
            assert!(reference.path.is_none());
            assert!(matches!(
                reference.error,
                Some(
                    PersistError::Timeout
                        | PersistError::QueueFull
                        | PersistError::WorkerUnavailable
                )
            ));
        }
        assert!(matches!(
            report.persistence(),
            ReportPersistence::Failed {
                error: PersistError::WorkerUnavailable,
                ..
            }
        ));
        while receiver.recv().await.is_some() {}
        assert!(receiver.terminal().await.report_id.is_none());
        assert!(budget.usage().reserved > 0);
        drop(persistence);
        gate.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while budget.usage().reserved > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
}

#[test]
fn reserved_report_lane_ignores_saturated_record_credits_and_intent() {
    run_test(move || async move {
        use crate::jobs::persist::JobPersistence;
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let (mut persistence, driver, writer) = JobPersistence::scripted(&ready);
        let pool = persistence.pool.clone();
        let clock = persistence.clock.clone();
        let mut credits = Vec::new();
        let token = CancelToken::new();
        for _ in 0..28 {
            credits.push(pool.acquire(72 * 1024, clock.now(), &token).await.unwrap());
        }
        credits.push(pool.acquire(32 * 1024, clock.now(), &token).await.unwrap());
        let engine = Engine::new(ready, JobRequest::Build).unwrap();
        engine.cancellation().request_cancel(CancelSource::Api);
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let (report, ()) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), driver);
        assert!(matches!(
            report.persistence(),
            ReportPersistence::Published { .. }
        ));
        assert!(
            writer
                .files
                .lock()
                .unwrap()
                .contains_key(std::path::Path::new("report.json"))
        );
        assert_eq!(persistence.stats().peak_credits, 2 * 1024 * 1024);
        assert_eq!(persistence.stats().credit_wait, Duration::ZERO);
        drop(credits);
    });
}

#[test]
fn report_deadline_has_its_own_five_second_origin_after_empty_w() {
    run_test(move || async move {
        use crate::jobs::persist::{JobPersistence, scripted::ScriptedWriter};
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let gate = Arc::new(tokio::sync::Notify::new());
        gate.notify_one();
        let mut persistence = JobPersistence::controlled(&ready, gate);
        let clock = persistence.clock.clone();
        let (writer, mut events) = ScriptedWriter::new(true);
        *persistence.script.lock().unwrap() = Some(writer);
        let engine = Engine::new(ready, JobRequest::Build).unwrap();
        let mut receiver = engine.events().subscribe().unwrap();
        let mut fake = Fake::default();
        let mut hooks = NoHooks;
        let origin = clock.now();
        let (report, ()) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), async {
                let write = events.recv().await.unwrap();
                assert_eq!(write.path, PathBuf::from("report.json"));
                clock.advance(Duration::from_secs(5));
                let _ = write.release.send(());
            });
        assert_eq!(clock.now(), origin + Duration::from_secs(5));
        assert!(report.data().executions.is_empty());
        assert!(matches!(
            report.persistence(),
            ReportPersistence::Failed {
                error: PersistError::Timeout,
                ..
            }
        ));
        assert_eq!(
            persistence.stats().settlement_elapsed,
            Some(Duration::from_secs(5))
        );
        while receiver.recv().await.is_some() {}
        assert!(receiver.terminal().await.report_id.is_none());
    });
}

#[test]
fn every_job_verdict_keeps_the_fact_publication_exclusions() {
    for &verdict in JobVerdict::ALL {
        assert_eq!(
            eligible(verdict),
            !matches!(
                verdict,
                JobVerdict::Cancelled | JobVerdict::Infrastructure | JobVerdict::InternalError
            )
        );
    }
}

// F-24 (Discard): caps are scalar config values; inject the resolved policy,
// then execute a real solution through the ordinary test engine.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn file_output_does_not_consume_discarded_stdout() {
    run_test(move || async move {
        supervisor::init_process().unwrap();
        let f = Fixture::new();
        let mut engine = test_engine(&f, "python", 1)
            .file_io(Some(report::FileIo {
                input: None,
                output: Some("out.txt".parse().unwrap()),
            }))
            .unwrap();
        let Some(PlannedTarget::BuildFree(free)) = &engine.ready.planned_target else {
            panic!("build-free target")
        };
        let mut stage = free.run(engine.ready.dir.join("cwd")).unwrap();
        let crate::exec::ResolvedCommand::Argv { program, args } = &mut stage.command else {
            panic!("argv")
        };
        program.path = "/bin/sh".into();
        program.record.chosen_path = program.path.clone();
        program.record.request = crate::exec::ProgramRequest::Path(program.path.clone());
        *args = vec![
            "-c".into(),
            "printf 'yes\\n' > out.txt; head -c 8192 /dev/zero".into(),
            "cpf".into(),
        ];
        stage.stdout.bytes.value = 4096;
        stage.stdout.policy = CapPolicy::Discard;
        engine.plan[0].recipe = Recipe::Resolved(Box::new(stage));
        let report = engine
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        let data = report.data();
        assert_eq!(data.tests[0].class, Some(RunClass::ExitedZero));
        assert_eq!(data.tests[0].verdict, Some(report::TestVerdict::Passed));
        assert!(matches!(
            data.tests[0].check.as_ref().unwrap().kind,
            report::CheckKind::Passed
        ));
        assert!(matches!(
            data.tests[0].output_file,
            Some(report::OutputFile::Collected { bytes: 4, .. })
        ));
        assert_eq!(data.executions[0].result.coverage, Coverage::Started);
        assert!(
            data.executions[0]
                .summary
                .as_ref()
                .unwrap()
                .notes
                .contains(&report::RunNote::TruncatedStdout)
        );
        assert!(data.details.target.as_ref().unwrap().io.is_some());
        assert_eq!(data.result.counts.primary.started, 1);
        assert_eq!(data.result.counts.by_verdict.passed, 1);
        assert_eq!(data.details.pre_delivery_exit.code, 0);
    });
}

#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
#[path = "cwd_tests.rs"]
mod cwd_tests;
