use super::*;
use crate::exec::ResolvedCommand;
use crate::report::{RunClass, RunNote};
use crate::storage::job::{ProbeOutcome, probe};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};

fn allowance() -> Duration {
    Duration::from_secs(if cfg!(target_os = "macos") {
        4
    } else if std::env::var_os("GITHUB_ACTIONS").is_some() {
        2
    } else {
        1
    })
}

// Counters and thread credits are process-wide. Isolate these fixtures from
// other unit tests, with a hard wall timeout even if an executor poll blocks.
fn isolated(name: &str, test: impl FnOnce()) {
    const CHILD: &str = "CPF_CWD_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        supervisor::init_process().unwrap();
        test();
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("jobs::engine::tests::cwd_tests::{name}"),
            "--nocapture",
        ])
        .env(CHILD, name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(45);
    loop {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("cwd fixture did not return within its wall timeout");
        }
        // Polling the child's exit status only; no ordering depends on it.
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn engine(f: &Fixture, count: usize, limit: u64) -> Engine<'static> {
    let mut engine = command_engine(f, CommandKind::Cmd, count, true);
    engine.ready.cwd_bytes = Some(limit);
    let Some(PlannedTarget::BuildFree(free)) = &engine.ready.planned_target else {
        panic!("expected a build-free target");
    };
    let mut stage = free.run(engine.ready.dir.join("cwd")).unwrap();
    let ResolvedCommand::Argv { program, args } = &mut stage.command else {
        panic!("expected argv");
    };
    program.path = "/bin/sh".into();
    program.record.chosen_path = program.path.clone();
    program.record.request = crate::exec::ProgramRequest::Path(program.path.clone());
    *args = vec!["-c".into(), "printf 123456789 > large".into()];
    for plan in &mut engine.plan {
        plan.recipe = Recipe::Resolved(Box::new(stage.clone()));
    }
    engine
}

struct Release(mpsc::Sender<()>);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

async fn idle() {
    tokio::time::timeout(allowance(), async {
        loop {
            let counters = supervisor::debug_counters();
            if counters.slots_in_use == 0
                && counters.service_pending == 0
                && counters.credits_in_use == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cleanup did not return worker credits");
}

async fn stalled(point: Point, cancelling: bool) {
    let f = Fixture::new();
    let engine = engine(&f, 2, 4);
    let directory = engine.ready.dir.clone();
    let budget = engine.ready.budget.clone();
    assert!(budget.usage().retained > 0);
    let runtime_thread = std::thread::current().id();
    let entered = CancelToken::new();
    let worker_entered = entered.clone();
    let (release, receiver) = mpsc::channel();
    let release = Release(release);
    let receiver = Mutex::new(receiver);
    let engine = engine.observe(move |observed| {
        if observed == point {
            assert_ne!(std::thread::current().id(), runtime_thread);
            worker_entered.cancel();
            receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .expect("cwd pause was not released");
        }
    });
    let cancel = engine.cancellation();
    let mut executor = SupervisorExecutor::start().unwrap();
    let mut persistence = MemoryPersistence::default();
    let mut hooks = NoHooks;
    let started = Instant::now();
    let mut future = Box::pin(engine.run(&mut executor, &mut persistence, &mut hooks));
    tokio::time::timeout(allowance(), async {
        tokio::select! {
            _ = entered.cancelled() => {},
            _ = future.as_mut() => panic!("job finished before the cwd pause"),
        }
    })
    .await
    .expect("cwd walk blocked the executor");
    // A separately scheduled task and timers must keep making progress.
    tokio::time::timeout(allowance(), tokio::spawn(async { 42 }))
        .await
        .expect("another task could not progress")
        .unwrap();
    let waiting = Instant::now();
    let bound = if cancelling {
        cancel.request_cancel(CancelSource::Api);
        allowance()
    } else {
        Duration::from_secs(5) + allowance()
    };
    let report = tokio::time::timeout(bound, future.as_mut())
        .await
        .expect("cwd wait exceeded its bound");
    drop(future);
    assert!(waiting.elapsed() <= bound);
    let execution = &report.data().executions[0];
    let summary = execution.summary.as_ref().unwrap();
    let note = if cancelling {
        RunNote::CwdUnmeasuredCancelled
    } else {
        assert!(started.elapsed() >= Duration::from_secs(5));
        RunNote::CwdUnmeasuredTimeout
    };
    assert!(summary.notes.contains(&note));
    assert!(!summary.notes.contains(&RunNote::CwdOverflow));
    assert_eq!(summary.cwd_overflow_bytes, None);
    assert_eq!(summary.class, RunClass::ExitedZero);
    assert_eq!(execution.result.invariant, None);
    let encoded = crate::report::wire::encode_run_summary(summary).unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert!(
        wire["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == note.wire_token())
    );
    if !cancelling {
        assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
        // The next run is not blocked and does not stack a second walker on
        // the stalled filesystem: it is reported unmeasured at once.
        let next = report.data().executions[1].summary.as_ref().unwrap();
        assert_eq!(next.notes, vec![RunNote::CwdUnmeasuredTimeout]);
        assert_eq!(next.cwd_overflow_bytes, None);
        assert_eq!(next.class, RunClass::ExitedZero);
        assert!(directory.join("r00001/cwd/large").is_file());
    }
    drop(report);
    drop(persistence);
    drop(executor);
    tokio::time::timeout(allowance(), async {
        loop {
            let counters = supervisor::debug_counters();
            if counters.service_pending == 1 && counters.credits_in_use == 2 {
                assert_eq!(counters.slots_in_use, 0);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stalled walker was not transferred with its credit");
    assert!(matches!(probe(&directory), ProbeOutcome::Held));
    assert!(
        budget.usage().retained > 0,
        "lease accounting released early"
    );
    assert!(directory.join("r00000/cwd/large").is_file());
    drop(release);
    idle().await;
    assert!(matches!(probe(&directory), ProbeOutcome::Acquired(_)));
    assert_eq!(budget.usage().retained, 0);
}

#[test]
fn stalled_measurement_times_out_and_transfers_ownership() {
    isolated(
        "stalled_measurement_times_out_and_transfers_ownership",
        || {
            run_test(move || async move { stalled(Point::BeforeCwdMeasure(0), false).await });
        },
    );
}

#[test]
fn stalled_clearing_shares_the_deadline_and_transfers_ownership() {
    isolated(
        "stalled_clearing_shares_the_deadline_and_transfers_ownership",
        || {
            run_test(move || async move { stalled(Point::BeforeCwdClear(0), false).await });
        },
    );
}

#[test]
fn cancellation_interrupts_stalled_measurement() {
    isolated("cancellation_interrupts_stalled_measurement", || {
        run_test(move || async move { stalled(Point::BeforeCwdMeasure(0), true).await });
    });
}

#[test]
fn cancellation_interrupts_stalled_clearing() {
    isolated("cancellation_interrupts_stalled_clearing", || {
        run_test(move || async move { stalled(Point::BeforeCwdClear(0), true).await });
    });
}

/// The summary with every observation fixed to the golden run_finished
/// values, keeping what the cwd step decides: class, notes and overflow.
fn fixed(summary: &RunSummary) -> RunSummary {
    use crate::supervisor as p;
    let now = Instant::now();
    RunSummary {
        observation: SummaryEvidence::default(),
        class: summary.class.clone(),
        record: Arc::new(ExecutionRecord::Started(Box::new(p::RootOutcome {
            milestones: None,
            kill_attempts: 0,
            stdout: None,
            stderr: None,
            status: p::Termination::Exited { code: 0 },
            stop_trigger: None,
            enforced_at: None,
            termination: p::TerminationEvidence {
                requested_at: None,
                method: None,
                cleanup: p::Cleanup::Confirmed,
            },
            violations: vec![],
            measurements: vec![],
            limits: vec![],
            timing: p::RootTiming {
                setup: Duration::from_micros(1),
                create_end: now,
                exit_observed: Some(now),
                observer: p::ObserverKind::Polling,
                wall: Some(Duration::from_micros(2)),
                method: p::WallMethod::Observed,
                grace: Duration::from_millis(50),
            },
            notes: vec![],
            errors: vec![],
        }))),
        deadline: Duration::from_micros(3),
        notes: summary.notes.clone(),
        cwd_overflow_bytes: summary.cwd_overflow_bytes,
    }
}

/// Legacy expected bytes: the golden summary, which predates this step, and
/// its overflow form as the synchronous step produced it.
fn legacy(overflow: bool) -> Vec<u8> {
    let event: &[u8] = include_bytes!("../../report/wire/fixtures/run_finished.json");
    let key = b"\"summary\":";
    let start = event.windows(key.len()).position(|w| w == key).unwrap() + key.len();
    let summary = &event[start..event.len() - 1];
    let healthy: &[u8] = br#""notes":[],"notes_dropped":0,"cwd_overflow_bytes":null}"#;
    assert!(summary.ends_with(healthy));
    if !overflow {
        return summary.to_vec();
    }
    let mut bytes = summary[..summary.len() - healthy.len()].to_vec();
    bytes.extend_from_slice(
        br#""notes":["cwd_overflow"],"notes_dropped":0,"cwd_overflow_bytes":"9"}"#,
    );
    bytes
}

/// A job-owned tree of 9 bytes outside any engine run.
fn tree(engine: &Engine<'static>) -> PathBuf {
    let tree = engine.ready.dir.join("r00000/cwd");
    fs::create_dir_all(&tree).unwrap();
    fs::write(tree.join("large"), b"123456789").unwrap();
    tree
}

fn never_walks() -> Option<Observation> {
    Some(Observation(Arc::new(|point: Point| {
        assert!(
            !matches!(point, Point::BeforeCwdMeasure(_) | Point::BeforeCwdClear(_)),
            "walker started"
        );
    })))
}

#[test]
fn credit_refusal_is_io_and_releases_the_walker_flag() {
    isolated("credit_refusal_is_io_and_releases_the_walker_flag", || {
        run_test(move || async move {
            let f = Fixture::new();
            let engine = engine(&f, 1, 4);
            let tree = tree(&engine);
            let walking = Arc::new(AtomicBool::new(false));
            let bounds = supervisor::global_bounds().unwrap();
            let mut held = Vec::new();
            while let Some(credit) = bounds.thread_credits.try_reserve(1) {
                held.push(credit);
            }
            let result = super::super::cwd::finish(
                engine.ready.owner.clone(),
                tree.clone(),
                4,
                0,
                CancelToken::new(),
                &walking,
                never_walks(),
                0,
            )
            .await;
            assert_eq!(result, Err(RunNote::CwdUnmeasuredIo));
            assert!(!walking.load(Ordering::Acquire), "walker flag leaked");
            assert!(tree.join("large").is_file());
            drop(held);
            // The next attempt is admitted and walks normally.
            let result = super::super::cwd::finish(
                engine.ready.owner.clone(),
                tree.clone(),
                4,
                0,
                CancelToken::new(),
                &walking,
                None,
                0,
            )
            .await;
            assert_eq!(result, Ok(Some(9)));
            assert!(!tree.exists());
            assert!(!walking.load(Ordering::Acquire));
            idle().await;
        });
    });
}

#[test]
fn cancellation_before_the_step_skips_it_and_keeps_the_tree() {
    isolated(
        "cancellation_before_the_step_skips_it_and_keeps_the_tree",
        || {
            run_test(move || async move {
                let f = Fixture::new();
                let engine = engine(&f, 1, 4);
                let tree = tree(&engine);
                let walking = Arc::new(AtomicBool::new(false));
                let cancel = CancelToken::new();
                cancel.cancel();
                let credits = supervisor::debug_counters().credits_in_use;
                let result = super::super::cwd::finish(
                    engine.ready.owner.clone(),
                    tree.clone(),
                    4,
                    0,
                    cancel,
                    &walking,
                    never_walks(),
                    0,
                )
                .await;
                assert_eq!(result, Err(RunNote::CwdUnmeasuredCancelled));
                // No worker was credited; an over-cap tree is kept.
                assert_eq!(supervisor::debug_counters().credits_in_use, credits);
                assert!(!walking.load(Ordering::Acquire));
                assert_eq!(fs::read(tree.join("large")).unwrap(), b"123456789");
                idle().await;
            });
        },
    );
}

#[test]
fn dropped_wait_transfers_a_clearing_walker_that_finishes_its_clear() {
    isolated(
        "dropped_wait_transfers_a_clearing_walker_that_finishes_its_clear",
        || {
            run_test(move || async move {
                let f = Fixture::new();
                let engine = engine(&f, 1, 4);
                let tree = tree(&engine);
                let directory = engine.ready.dir.clone();
                let budget = engine.ready.budget.clone();
                let owner = engine.ready.owner.clone();
                drop(engine);
                let entered = CancelToken::new();
                let worker_entered = entered.clone();
                let (release, receiver) = mpsc::channel();
                let release = Release(release);
                let receiver = Mutex::new(receiver);
                // Pause after the pre-clear check passed: the clear itself
                // runs later, independent of the deadline.
                let observation = Observation(Arc::new(move |point: Point| {
                    if point == Point::BeforeCwdClear(0) {
                        worker_entered.cancel();
                        receiver
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(30))
                            .expect("cwd pause was not released");
                    }
                }));
                let walking = Arc::new(AtomicBool::new(false));
                let cancel = CancelToken::new();
                let mut future = Box::pin(super::super::cwd::finish(
                    owner,
                    tree.clone(),
                    4,
                    0,
                    cancel.clone(),
                    &walking,
                    Some(observation),
                    0,
                ));
                tokio::time::timeout(allowance(), async {
                    tokio::select! {
                        _ = entered.cancelled() => {},
                        _ = future.as_mut() => panic!("walk finished before the pause"),
                    }
                })
                .await
                .expect("walker did not reach the clear");
                // Dropping the waiter neither cancels nor joins the walker.
                drop(future);
                assert!(!cancel.is_cancelled());
                tokio::time::timeout(allowance(), async {
                    loop {
                        let counters = supervisor::debug_counters();
                        if counters.service_pending == 1 && counters.credits_in_use == 2 {
                            assert_eq!(counters.slots_in_use, 0);
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("dropped walker was not transferred with its credit");
                assert!(walking.load(Ordering::Acquire));
                assert!(matches!(probe(&directory), ProbeOutcome::Held));
                assert!(
                    budget.usage().retained > 0,
                    "lease accounting released early"
                );
                assert!(tree.join("large").is_file());
                drop(release);
                idle().await;
                // The transferred walker completed its clear after the waiter
                // returned, then released the flag, lease and accounting.
                assert!(!tree.exists());
                assert!(!walking.load(Ordering::Acquire));
                assert!(matches!(probe(&directory), ProbeOutcome::Acquired(_)));
                assert_eq!(budget.usage().retained, 0);
            });
        },
    );
}

#[test]
fn healthy_measurement_and_overflow_preserve_report_fields() {
    isolated(
        "healthy_measurement_and_overflow_preserve_report_fields",
        || {
            run_test(move || async move {
                for limit in [9, 4] {
                    let f = Fixture::new();
                    let engine = engine(&f, 1, limit);
                    let directory = engine.ready.dir.join("r00000/cwd");
                    let report = engine
                        .run(
                            &mut SupervisorExecutor::start().unwrap(),
                            &mut MemoryPersistence::default(),
                            &mut NoHooks,
                        )
                        .await;
                    assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
                    let summary = report.data().executions[0].summary.as_ref().unwrap();
                    assert_eq!(summary.class, RunClass::ExitedZero);
                    // With observations fixed, the step's contribution encodes
                    // exactly as the schema 1 document did before this step.
                    assert_eq!(
                        crate::report::wire::encode_run_summary(&fixed(summary)).unwrap(),
                        legacy(limit == 4)
                    );
                    if limit == 9 {
                        assert_eq!(summary.notes, Vec::<RunNote>::new());
                        assert_eq!(summary.cwd_overflow_bytes, None);
                        assert_eq!(fs::read(directory.join("large")).unwrap(), b"123456789");
                    } else {
                        assert_eq!(summary.notes, vec![RunNote::CwdOverflow]);
                        assert_eq!(summary.cwd_overflow_bytes, Some(9));
                        assert!(!directory.exists());
                        assert!(report.data().details.warnings.iter().any(|warning| {
                            matches!(
                                warning.kind,
                                WarningKind::Retention {
                                    detail: "cwd_overflow",
                                    ..
                                }
                            )
                        }));
                    }
                }
                idle().await;
            });
        },
    );
}

#[test]
fn walk_io_failures_are_diagnostic_and_later_runs_continue() {
    isolated(
        "walk_io_failures_are_diagnostic_and_later_runs_continue",
        || {
            run_test(move || async move {
                for failure in [Point::BeforeCwdMeasure(0), Point::BeforeCwdClear(0)] {
                    let f = Fixture::new();
                    let engine = engine(&f, 2, 4);
                    let cwd = engine.ready.dir.join("r00000/cwd");
                    let engine = engine.observe(move |point| {
                        if point == failure {
                            // The walker has inspected a directory; replace it
                            // before its open so ENOTDIR is deterministic, even
                            // when tests run with elevated filesystem permissions.
                            fs::rename(&cwd, cwd.with_file_name("kept")).unwrap();
                            fs::write(&cwd, b"not a directory").unwrap();
                        }
                    });
                    let report = engine
                        .run(
                            &mut SupervisorExecutor::start().unwrap(),
                            &mut MemoryPersistence::default(),
                            &mut NoHooks,
                        )
                        .await;
                    assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
                    let first = report.data().executions[0].summary.as_ref().unwrap();
                    assert_eq!(first.notes, vec![RunNote::CwdUnmeasuredIo]);
                    assert_eq!(first.cwd_overflow_bytes, None);
                    let next = report.data().executions[1].summary.as_ref().unwrap();
                    assert_eq!(next.notes, vec![RunNote::CwdOverflow]);
                    idle().await;
                }
            });
        },
    );
}
