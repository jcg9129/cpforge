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
    isolated_settings(name, &[], test);
}

fn isolated_settings(name: &str, settings: &[(&str, &str)], test: impl FnOnce()) {
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
        .envs(settings.iter().copied())
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
    released_budget(&budget).await;
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
            output_file: None,
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
                None,
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
                    None,
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
                    None,
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
                released_budget(&budget).await;
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

#[test]
fn judge_evidence_and_removal_do_not_need_a_post_run_credit() {
    isolated(
        "judge_evidence_and_removal_do_not_need_a_post_run_credit",
        || {
            run_test(move || async move {
                let f = Fixture::new();
                let engine = engine(&f, 1, 4);
                let directory = engine.ready.dir.join("judge-credit");
                fs::create_dir_all(directory.join("cwd")).unwrap();
                fs::write(directory.join("source"), b"abc").unwrap();
                let mut scratch =
                    super::super::judge_files::Scratch::new(engine.ready.owner.clone());
                scratch
                    .stage(
                        &[crate::jobs::roles::StagedFile {
                            source: Some(directory.join("source")),
                            destination: directory.join("cwd").join("input"),
                            bytes: None,
                        }],
                        &engine.ready.budget,
                        None,
                    )
                    .unwrap();
                let mut root = root(1);
                let mut stderr = root.stdout.take().unwrap();
                stderr.stored = 3;
                stderr.sink = Some(directory.join("stderr"));
                fs::write(stderr.sink.as_ref().unwrap(), b"bad trailing bytes").unwrap();
                root.stderr = Some(stderr);
                let mut executor = SupervisorExecutor::start().unwrap();
                let bounds = supervisor::global_bounds().unwrap();
                let mut held = Vec::new();
                while let Some(credit) = bounds.thread_credits.try_reserve(1) {
                    held.push(credit);
                }
                let walking = Arc::new(AtomicBool::new(false));
                let finished = super::super::judge_files::finish(
                    engine.ready.owner.clone(),
                    Some(Arc::new(ExecutionRecord::Started(Box::new(root)))),
                    Some(scratch),
                    &walking,
                    &mut executor,
                    None,
                )
                .await;
                assert!(!walking.load(Ordering::Acquire));
                assert!(
                    !finished.kept,
                    "no failed removal, so no judge_scratch warning"
                );
                assert_eq!(finished.kept_bytes, 0);
                assert_eq!(finished.cwd, Ok(()));
                assert_eq!(finished.stderr, Some(report::JudgeText::new(b"bad", 1024)));
                assert!(!directory.join("cwd").join("input").exists());
                assert_eq!(
                    super::super::cwd::finish(
                        engine.ready.owner.clone(),
                        directory.join("cwd"),
                        4,
                        0,
                        CancelToken::new(),
                        &walking,
                        None,
                        None,
                        0,
                    )
                    .await,
                    Err(RunNote::CwdUnmeasuredIo)
                );
                assert!(!walking.load(Ordering::Acquire));
                drop(held);
            });
        },
    );
}

// A stalled filesystem during judge evidence/removal is bounded by the
// post-run step's 5 s deadline on the credited worker and, when no credit is
// free, on the job's creation thread alike; the stalled thread keeps the work.
#[test]
fn stalled_judge_scratch_is_bounded_with_and_without_a_post_run_credit() {
    let barriers = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("judge-finish-barriers");
    isolated_settings(
        "stalled_judge_scratch_is_bounded_with_and_without_a_post_run_credit",
        &[
            ("CPF_STORAGE_PAUSE_AT", "judge.finish"),
            ("CPF_STORAGE_BARRIER_DIR", barriers.to_str().unwrap()),
        ],
        || {
            let _ = fs::remove_dir_all(&barriers);
            fs::create_dir_all(&barriers).unwrap();
            let release = barriers.join("judge.finish.release");
            run_test(move || async move {
                for exhausted in [false, true] {
                    let f = Fixture::new();
                    let engine = engine(&f, 1, 4);
                    let directory = engine.ready.dir.join("judge-stall");
                    fs::create_dir_all(directory.join("cwd")).unwrap();
                    fs::write(directory.join("source"), b"abc").unwrap();
                    let staged = directory.join("cwd").join("input");
                    let mut scratch =
                        super::super::judge_files::Scratch::new(engine.ready.owner.clone());
                    scratch
                        .stage(
                            &[crate::jobs::roles::StagedFile {
                                source: Some(directory.join("source")),
                                destination: staged.clone(),
                                bytes: None,
                            }],
                            &engine.ready.budget,
                            None,
                        )
                        .unwrap();
                    let mut executor = SupervisorExecutor::start().unwrap();
                    let bounds = supervisor::global_bounds().unwrap();
                    let mut held = Vec::new();
                    if exhausted {
                        while let Some(credit) = bounds.thread_credits.try_reserve(1) {
                            held.push(credit);
                        }
                    }
                    let walking = Arc::new(AtomicBool::new(false));
                    let start = tokio::time::Instant::now();
                    let finished = tokio::time::timeout(
                        Duration::from_secs(5) + allowance(),
                        super::super::judge_files::finish(
                            engine.ready.owner.clone(),
                            None,
                            Some(scratch),
                            &walking,
                            &mut executor,
                            None,
                        ),
                    )
                    .await
                    .expect("judge evidence/removal wait is bounded");
                    assert!(start.elapsed() >= Duration::from_secs(5));
                    // Same outcome as a worker past its deadline: unmeasured,
                    // staged bytes kept and excluded, no competing cwd step.
                    assert_eq!(finished.cwd, Err(RunNote::CwdUnmeasuredTimeout));
                    assert!(finished.kept);
                    assert_eq!(finished.kept_bytes, 3);
                    assert!(staged.exists());
                    assert!(
                        walking.load(Ordering::Acquire),
                        "the stalled work keeps the job's single walker mark"
                    );
                    fs::write(&release, b"release").unwrap();
                    tokio::time::timeout(Duration::from_secs(10), async {
                        while staged.exists() || walking.load(Ordering::Acquire) {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    })
                    .await
                    .expect("released work removes its staged copy");
                    fs::remove_file(&release).unwrap();
                    drop(held);
                    drop(executor);
                    idle().await;
                }
            });
        },
    );
}

fn file_engine(f: &Fixture, count: usize, script: &str, input: bool) -> Engine<'static> {
    file_engine_with_budget(f, count, script, input, 256 << 20)
}

fn file_engine_with_budget(
    f: &Fixture,
    count: usize,
    script: &str,
    input: bool,
    limit: u64,
) -> Engine<'static> {
    let ready = ready_with_budget(f, "python", count, &Tools, limit);
    let mut engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap()
    .file_io(Some(report::FileIo {
        input: input.then(|| "in.txt".parse().unwrap()),
        output: Some("out.txt".parse().unwrap()),
    }))
    .unwrap();
    let Some(PlannedTarget::BuildFree(free)) = &engine.ready.planned_target else {
        panic!("build-free target")
    };
    let mut stage = free.run(engine.ready.dir.join("cwd")).unwrap();
    let ResolvedCommand::Argv { program, args } = &mut stage.command else {
        panic!("argv")
    };
    program.path = "/bin/sh".into();
    program.record.chosen_path = program.path.clone();
    program.record.request = crate::exec::ProgramRequest::Path(program.path.clone());
    *args = vec!["-c".into(), script.into(), "cpf".into()];
    stage.stdout.bytes.value = 4096;
    stage.stderr.bytes.value = 4096;
    for plan in &mut engine.plan {
        plan.recipe = Recipe::Resolved(Box::new(stage.clone()));
    }
    engine
}

async fn file_stall(point: &'static str, cancelling: bool) {
    let f = Fixture::new();
    let engine = file_engine(&f, 1, "printf 'yes\\n' > out.txt", false);
    let directory = engine.ready.dir.clone();
    let budget = engine.ready.budget.clone();
    let entered = CancelToken::new();
    let worker_entered = entered.clone();
    let (send, receiver) = mpsc::channel();
    let release = Release(send);
    let receiver = Mutex::new(receiver);
    let once = AtomicBool::new(false);
    let log = Arc::new(Mutex::new(Vec::new()));
    let operations = log.clone();
    let engine = engine.observe(move |event| {
        if let Point::FileIo {
            point: operation,
            size,
            ..
        } = event
        {
            operations
                .lock()
                .unwrap()
                .push((operation, size, std::thread::current().id()));
            if operation == point && !once.swap(true, Ordering::AcqRel) {
                worker_entered.cancel();
                receiver
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(30))
                    .unwrap();
            }
        }
    });
    let cancel = engine.cancellation();
    let stamps = Arc::new(supervisor::Milestones::default());
    let mut executor = SupervisorExecutor::start().unwrap();
    executor.options.milestones = Some(stamps.clone());
    let mut persistence = MemoryPersistence::default();
    let mut hooks = NoHooks;
    let mut future = Box::pin(engine.run(&mut executor, &mut persistence, &mut hooks));
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = entered.cancelled() => {},
            _ = future.as_mut() => panic!("job ended before file-I/O stall"),
        }
    })
    .await
    .unwrap();
    if cancelling {
        cancel.request_cancel(CancelSource::Api);
    }
    let final_report = tokio::time::timeout(Duration::from_secs(10), future.as_mut())
        .await
        .unwrap();
    drop(future);
    let data = final_report.data();
    let test = &data.tests[0];
    assert_eq!(
        test.output_file,
        Some(report::OutputFile::NotCollected {
            reason: report::CollectionFailure::Timeout,
            os_error: None
        })
    );
    assert_eq!(test.class, Some(RunClass::ExitedZero));
    assert_eq!(data.executions.len(), 1);
    assert_eq!(data.executions[0].result.coverage, Coverage::Started);
    assert_eq!(
        data.result.counts.primary,
        report::Cov {
            planned: 1,
            started: 1,
            ..report::Cov::default()
        }
    );
    assert_eq!(
        data.result.counts.by_class,
        report::ByClass {
            exited_zero: 1,
            ..report::ByClass::default()
        }
    );
    assert_eq!(data.result.counts.tests, 1);
    assert_eq!(
        data.details.target.as_ref().unwrap().io,
        Some(report::FileIo {
            input: None,
            output: Some("out.txt".parse().unwrap())
        })
    );
    if cancelling {
        assert!(matches!(
            test.check.as_ref().unwrap().kind,
            report::CheckKind::NotAdmitted {
                reason: report::CheckNotAdmitted::Cancelled
            }
        ));
        assert_eq!(test.verdict, Some(report::TestVerdict::Cancelled));
        assert_eq!(data.result.counts.by_verdict.cancelled, 1);
        assert_eq!(data.details.pre_delivery_exit.code, 6);
    } else {
        assert!(matches!(
            test.check.as_ref().unwrap().kind,
            report::CheckKind::CheckError { os_error: None }
        ));
        assert_eq!(test.verdict, Some(report::TestVerdict::Infrastructure));
        assert_eq!(data.result.counts.by_verdict.infrastructure, 1);
        assert_eq!(data.details.pre_delivery_exit.code, 10);
    }
    assert!(
        data.executions[0]
            .io_warnings
            .contains(&report::IoScratchDetail::NotMeasured)
    );
    let drain = stamps.get(supervisor::Milestone::DrainStart).unwrap();
    let frozen = stamps.get(supervisor::Milestone::CaptureFrozen).unwrap();
    assert!(frozen.duration_since(drain) >= Duration::from_secs(5));
    assert!(frozen.duration_since(drain) <= Duration::from_secs(5) + allowance());
    assert!(
        budget.usage().reserved >= 4096,
        "worker keeps its reservation"
    );
    assert!(budget.usage().retained + budget.usage().reserved <= budget.limit());
    let output = directory.join("r00000").join("out").join("output");
    if point == "file_io.write" {
        assert_eq!(fs::metadata(&output).unwrap().len(), 0);
    }
    drop(final_report);
    drop(persistence);
    drop(executor);
    tokio::time::timeout(Duration::from_secs(5), async {
        while supervisor::debug_counters().service_pending != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(supervisor::debug_counters().file_io_directories, 1);
    assert!(matches!(probe(&directory), ProbeOutcome::Held));
    let before = log.lock().unwrap().clone();
    drop(release);
    idle().await;
    assert_eq!(supervisor::debug_counters().file_io_directories, 0);
    released_budget(&budget).await;
    assert_eq!(
        budget.usage(),
        crate::storage::budget::BudgetUsage::default()
    );
    assert!(!output.exists());
    assert!(matches!(probe(&directory), ProbeOutcome::Acquired(_)));
    let operations = log.lock().unwrap();
    let worker = before
        .iter()
        .find(|(operation, _, _)| *operation == point)
        .unwrap()
        .2;
    // Even after logical transfer, every subsequent operation (including the
    // partial-copy unlink) runs on the original worker, never cleanup's poller.
    assert!(
        operations[before.len()..]
            .iter()
            .all(|(_, _, thread)| *thread == worker)
    );
    assert!(
        operations
            .iter()
            .any(|(operation, _, _)| *operation == "file_io.partial.unlink")
    );
}

// F-31(a)-(c): the run's capture deadline includes a blocked directory read or write.
#[test]
fn file_collection_stalls_freeze_and_transfer_at_the_sink_deadline() {
    isolated(
        "file_collection_stalls_freeze_and_transfer_at_the_sink_deadline",
        || {
            run_test(move || async move {
                file_stall("file_io.listing.entry", true).await;
                file_stall("file_io.listing.entry", false).await;
                file_stall("file_io.write", false).await;
            });
        },
    );
}

// F-31(d): worker credit refusal is admission infrastructure, before any child.
#[test]
fn file_worker_credit_refusal_never_starts_the_solution() {
    isolated(
        "file_worker_credit_refusal_never_starts_the_solution",
        || {
            run_test(move || async move {
                let f = Fixture::new();
                let engine = file_engine(&f, 1, "printf 'yes\\n' > out.txt", false);
                let mut executor = SupervisorExecutor::start().unwrap();
                let bounds = supervisor::global_bounds().unwrap();
                let mut held = Vec::new();
                while let Some(credit) = bounds.thread_credits.try_reserve(1) {
                    held.push(credit);
                }
                // Leave the two stdout/stderr sink credits free, so the refused
                // credit is the output-file worker's (capture pipeline admission).
                held.truncate(held.len().saturating_sub(2));
                let final_report = engine
                    .run(
                        &mut executor,
                        &mut MemoryPersistence::default(),
                        &mut NoHooks,
                    )
                    .await;
                let data = final_report.data();
                assert!(matches!(
                    data.tests[0].class,
                    Some(RunClass::Infrastructure { .. })
                ));
                assert_eq!(
                    data.tests[0].verdict,
                    Some(report::TestVerdict::Infrastructure)
                );
                assert_eq!(
                    data.tests[0].output_file,
                    Some(report::OutputFile::NotAttempted)
                );
                assert!(matches!(
                    data.tests[0].check.as_ref().unwrap().kind,
                    report::CheckKind::NotAdmitted { .. }
                ));
                assert_eq!(data.executions.len(), 1);
                assert!(matches!(
                    data.executions[0].result.coverage,
                    Coverage::NotStarted {
                        reason: NotStartedReason::CleanupCapacity | NotStartedReason::Worker
                    }
                ));
                assert_eq!(
                    data.result.counts.primary,
                    report::Cov {
                        planned: 1,
                        not_started: 1,
                        ..report::Cov::default()
                    }
                );
                assert_eq!(
                    data.details.target.as_ref().unwrap().io,
                    Some(report::FileIo {
                        input: None,
                        output: Some("out.txt".parse().unwrap())
                    })
                );
                assert_eq!(data.result.counts.by_class.infrastructure, 1);
                assert_eq!(data.result.counts.tests, 1);
                assert_eq!(data.result.counts.by_verdict.infrastructure, 1);
                assert_eq!(data.details.pre_delivery_exit.code, 10);
                assert_eq!(supervisor::debug_counters().file_io_directories, 0);
                drop(held);
            });
        },
    );
}

// F-25 supplements the command fixture with charge and descriptor quiescence.
#[test]
fn renamed_file_io_cwd_releases_the_staged_charge_and_directory() {
    isolated(
        "renamed_file_io_cwd_releases_the_staged_charge_and_directory",
        || {
            run_test(move || async move {
                let f = Fixture::new();
                let engine = file_engine(
                    &f,
                    1,
                    "printf 'yes\\n' > out.txt; mv ../cwd ../old; mkdir ../cwd; printf wrong > ../cwd/out.txt",
                    true,
                );
                let directory = engine.ready.dir.clone();
                let budget = engine.ready.budget.clone();
                let before = budget.usage().retained;
                let mut executor = SupervisorExecutor::start().unwrap();
                let final_report = engine
                    .run(
                        &mut executor,
                        &mut MemoryPersistence::default(),
                        &mut NoHooks,
                    )
                    .await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while budget.usage().reserved != 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let data = final_report.data();
                assert_eq!(data.tests[0].verdict, Some(report::TestVerdict::Passed));
                assert_eq!(
                    data.executions[0].io_warnings,
                    [report::IoScratchDetail::NotMeasured]
                );
                assert!(!directory.join("r00000").join("old").join("in.txt").exists());
                assert_eq!(budget.usage().reserved, 0);
                assert_eq!(
                    budget.usage().retained,
                    before + 4,
                    "only extracted output remains charged"
                );
                assert_eq!(supervisor::debug_counters().file_io_directories, 0);
                drop(final_report);
                drop(executor);
                idle().await;
                released_budget(&budget).await;
                assert_eq!(budget.usage().retained, 0);
            });
        },
    );
}

// F-33: cancellation publishes without waiting for the staged-input writer.
#[test]
fn blocked_file_staging_keeps_its_charge_until_the_creation_thread_returns() {
    isolated(
        "blocked_file_staging_keeps_its_charge_until_the_creation_thread_returns",
        || {
            run_test(move || async move {
                for release_after_owner in [false, true] {
                    let f = Fixture::new();
                    let engine = file_engine(&f, 2, "printf 'yes\\n' > out.txt", true);
                    let directory = engine.ready.dir.clone();
                    let budget = engine.ready.budget.clone();
                    let entered = CancelToken::new();
                    let worker_entered = entered.clone();
                    let (send, receiver) = mpsc::channel();
                    let release = Release(send);
                    let release_before_return = release.0.clone();
                    let receiver = Mutex::new(receiver);
                    let log = Arc::new(Mutex::new(Vec::new()));
                    let operations = log.clone();
                    let engine = engine.observe(move |event| {
                        if !release_after_owner && event == Point::BeforeSnapshot {
                            // The cancelled record and checks already exist,
                            // but the job has not returned its report yet.
                            let _ = release_before_return.send(());
                        }
                        if let Point::FileIo { point, .. } = event {
                            operations
                                .lock()
                                .unwrap()
                                .push((point, std::thread::current().id()));
                            if point == "file_io.staging.write" {
                                worker_entered.cancel();
                                receiver
                                    .lock()
                                    .unwrap()
                                    .recv_timeout(Duration::from_secs(30))
                                    .unwrap();
                            }
                        }
                    });
                    let cancel = engine.cancellation();
                    let mut executor = SupervisorExecutor::start().unwrap();
                    let mut persistence = MemoryPersistence::default();
                    let mut hooks = NoHooks;
                    let mut future =
                        Box::pin(engine.run(&mut executor, &mut persistence, &mut hooks));
                    tokio::time::timeout(Duration::from_secs(10), async {
                        tokio::select! {
                            _ = entered.cancelled() => {},
                            _ = future.as_mut() => panic!("job finished before staging blocked"),
                        }
                    })
                    .await
                    .unwrap();
                    let staged = directory.join("r00000").join("cwd").join("in.txt");
                    assert!(staged.is_file());
                    assert!(budget.usage().reserved >= 6);
                    cancel.request_cancel(CancelSource::Api);
                    let final_report =
                        tokio::time::timeout(Duration::from_secs(5), future.as_mut())
                            .await
                            .unwrap();
                    drop(future);
                    let data = final_report.data();
                    assert_eq!(data.executions.len(), 2);
                    assert_eq!(
                        data.executions[0].result.coverage,
                        Coverage::NotStarted {
                            reason: NotStartedReason::CancelledBeforeStart
                        }
                    );
                    assert_eq!(
                        data.executions[1].result.coverage,
                        Coverage::Skipped {
                            reason: SkipReason::Cancelled
                        }
                    );
                    assert_eq!(
                        data.result.counts.primary,
                        report::Cov {
                            planned: 2,
                            not_started: 1,
                            skipped: 1,
                            ..report::Cov::default()
                        }
                    );
                    assert_eq!(data.result.counts.tests, 2);
                    assert_eq!(data.result.counts.by_class.cancelled, 1);
                    assert_eq!(data.result.counts.by_class.skipped, 1);
                    // A test skipped by cancellation is `cancelled` too (report §2.5).
                    assert_eq!(data.result.counts.by_verdict.cancelled, 2);
                    assert_eq!(data.result.counts.by_verdict.not_run, 0);
                    assert_eq!(data.tests[0].class, Some(RunClass::Cancelled));
                    assert_eq!(data.tests[0].verdict, Some(report::TestVerdict::Cancelled));
                    assert!(matches!(
                        data.tests[0].check.as_ref().unwrap().kind,
                        report::CheckKind::NotAdmitted {
                            reason: report::CheckNotAdmitted::RunNotEligible
                        }
                    ));
                    for test in &data.tests {
                        assert_eq!(test.output_file, Some(report::OutputFile::NotAttempted));
                    }
                    assert_eq!(
                        data.details.target.as_ref().unwrap().io,
                        Some(report::FileIo {
                            input: Some("in.txt".parse().unwrap()),
                            output: Some("out.txt".parse().unwrap())
                        })
                    );
                    assert_eq!(data.tests[1].class, None);
                    assert_eq!(data.tests[1].verdict, Some(report::TestVerdict::Cancelled));
                    assert!(matches!(
                        data.tests[1].check.as_ref().unwrap().kind,
                        report::CheckKind::NotAdmitted {
                            reason: report::CheckNotAdmitted::Cancelled
                        }
                    ));
                    assert_eq!(data.details.pre_delivery_exit.code, 6);
                    if release_after_owner {
                        assert!(
                            staged.is_file(),
                            "no other worker may touch the staged copy"
                        );
                        assert!(budget.usage().reserved >= 6);
                    }
                    let before = log.lock().unwrap().clone();
                    let worker = before.last().unwrap().1;
                    let mut report_owner = Some(final_report);
                    if release_after_owner {
                        drop(report_owner.take());
                    }
                    drop(persistence);
                    drop(executor);
                    drop(release);
                    idle().await;
                    assert!(!staged.exists());
                    assert_eq!(budget.usage().reserved, 0);
                    assert_eq!(supervisor::debug_counters().file_io_directories, 0);
                    assert!(
                        log.lock().unwrap()[before.len()..]
                            .iter()
                            .all(|(_, thread)| *thread == worker)
                    );
                    drop(report_owner);
                    released_budget(&budget).await;
                    assert_eq!(budget.usage().retained, 0);
                }
            });
        },
    );
}

// F-21: kept staged copies remain charged, eventually closing admission. The
// measurement excludes those copies, and a program-deleted input is released.
#[test]
fn failed_io_scratch_unlinks_retain_only_the_copies_still_present() {
    isolated_settings(
        "failed_io_scratch_unlinks_retain_only_the_copies_still_present",
        &[("CPF_FILE_IO_FAULTS", "file_io.unlink=EIO")],
        || {
            run_test(move || async move {
                let probe_fixture = Fixture::new();
                let probe = file_engine(&probe_fixture, 4, "printf 'yes\\n' > out.txt", true);
                let baseline = probe.ready.budget.usage();
                let (tests, aggregates) = report_populations(probe.kind, &probe.plan, 4);
                let report_bytes =
                    report::wire::report_reservation(4, tests, aggregates, probe.report_shape)
                        .unwrap();
                let input = probe.ready.manifest.entries()[0].input.len;
                assert_eq!(input, 6);
                // MemoryPersistence owns each completed run's metadata allowance
                // until W. Leave room for the third allowance, all its streams,
                // and exactly two kept staged inputs plus their extracted outputs.
                let limit =
                    baseline.retained + report_bytes + 3 * METADATA + 3 * 4096 + 2 * (input + 4);
                drop(probe);
                let f = Fixture::new();
                let mut engine =
                    file_engine_with_budget(&f, 4, "printf 'yes\\n' > out.txt", true, limit);
                engine.ready.cwd_bytes = Some(4);
                assert_eq!(engine.ready.budget.usage(), baseline);
                let directory = engine.ready.dir.clone();
                let budget = engine.ready.budget.clone();
                let peak = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let max_bytes = peak.clone();
                let observed_budget = budget.clone();
                let engine = engine.observe(move |_| {
                    let usage = observed_budget.usage();
                    let charged = usage.retained + usage.reserved;
                    assert!(charged <= limit);
                    max_bytes.fetch_max(charged, Ordering::AcqRel);
                });
                let mut executor = SupervisorExecutor::start().unwrap();
                let final_report = engine
                    .run(
                        &mut executor,
                        &mut MemoryPersistence::default(),
                        &mut NoHooks,
                    )
                    .await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while budget.usage().reserved != 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let data = final_report.data();
                assert_eq!(
                    data.details.target.as_ref().unwrap().io,
                    Some(report::FileIo {
                        input: Some("in.txt".parse().unwrap()),
                        output: Some("out.txt".parse().unwrap())
                    })
                );
                assert_eq!(data.executions.len(), 4);
                for index in 0..2 {
                    let test = &data.tests[index];
                    assert_eq!(test.verdict, Some(report::TestVerdict::Passed));
                    assert_eq!(test.class, Some(RunClass::ExitedZero));
                    assert!(matches!(
                        test.check.as_ref().unwrap().kind,
                        report::CheckKind::Passed
                    ));
                    assert!(matches!(
                        test.output_file,
                        Some(report::OutputFile::Collected { bytes: 4, .. })
                    ));
                    let execution = &data.executions[index];
                    assert_eq!(execution.result.coverage, Coverage::Started);
                    assert_eq!(
                        execution.io_warnings,
                        [
                            report::IoScratchDetail::StagedInputKept,
                            report::IoScratchDetail::OutputKept
                        ]
                    );
                    // A complete measurement within cwd_bytes = 4: only out.txt
                    // counts; the kept 6-byte staged input would overflow it.
                    let summary = execution.summary.as_ref().unwrap();
                    assert_eq!(summary.cwd_overflow_bytes, None);
                    assert_eq!(summary.notes, Vec::<RunNote>::new());
                    let cwd = directory
                        .join(execution.run.as_ref().unwrap().as_str())
                        .join("cwd");
                    assert_eq!(fs::metadata(cwd.join("in.txt")).unwrap().len(), input);
                    assert_eq!(fs::read(cwd.join("out.txt")).unwrap(), b"yes\n");
                }
                assert_eq!(
                    data.executions[2].result.coverage,
                    Coverage::NotStarted {
                        reason: NotStartedReason::RetentionBudget
                    }
                );
                assert_eq!(
                    data.executions[3].result.coverage,
                    Coverage::Skipped {
                        reason: SkipReason::AdmissionClosed
                    }
                );
                assert!(matches!(
                    data.tests[2].class,
                    Some(RunClass::Infrastructure { .. })
                ));
                assert_eq!(
                    data.tests[2].verdict,
                    Some(report::TestVerdict::Infrastructure)
                );
                assert_eq!(data.tests[3].class, None);
                assert_eq!(
                    data.tests[3].verdict,
                    Some(report::TestVerdict::NotRun {
                        reason: SkipReason::AdmissionClosed
                    })
                );
                for test in &data.tests[2..] {
                    assert!(matches!(
                        test.check.as_ref().unwrap().kind,
                        report::CheckKind::NotAdmitted {
                            reason: report::CheckNotAdmitted::RunNotEligible
                        }
                    ));
                    assert_eq!(test.output_file, Some(report::OutputFile::NotAttempted));
                }
                assert_eq!(
                    data.result.counts.primary,
                    report::Cov {
                        planned: 4,
                        started: 2,
                        not_started: 1,
                        skipped: 1,
                        ..report::Cov::default()
                    }
                );
                assert_eq!(data.result.counts.tests, 4);
                assert_eq!(
                    data.result.counts.by_class,
                    report::ByClass {
                        exited_zero: 2,
                        infrastructure: 1,
                        skipped: 1,
                        ..report::ByClass::default()
                    }
                );
                assert_eq!(data.result.counts.by_verdict.passed, 2);
                assert_eq!(data.result.counts.by_verdict.infrastructure, 1);
                assert_eq!(data.result.counts.by_verdict.not_run, 1);
                assert_eq!(data.details.pre_delivery_exit.code, 10);
                assert_eq!(budget.usage().retained, baseline.retained + 2 * (input + 4));
                assert!(peak.load(Ordering::Acquire) <= limit);
                assert_eq!(supervisor::debug_counters().file_io_directories, 0);
                drop(final_report);
                drop(executor);
                idle().await;
                released_budget(&budget).await;
                assert_eq!(budget.usage().retained, 0);

                let f = Fixture::new();
                let engine = file_engine(&f, 1, "rm in.txt; printf 'yes\\n' > out.txt", true);
                let budget = engine.ready.budget.clone();
                let before = budget.usage().retained;
                let mut executor = SupervisorExecutor::start().unwrap();
                let final_report = engine
                    .run(
                        &mut executor,
                        &mut MemoryPersistence::default(),
                        &mut NoHooks,
                    )
                    .await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while budget.usage().reserved != 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(
                    final_report.data().tests[0].verdict,
                    Some(report::TestVerdict::Passed)
                );
                assert_eq!(
                    final_report.data().executions[0].io_warnings,
                    [report::IoScratchDetail::OutputKept]
                );
                assert_eq!(
                    budget.usage().retained,
                    before + 4,
                    "program-deleted input is not charged"
                );
            });
        },
    );
}
