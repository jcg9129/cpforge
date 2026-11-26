//! Process-free rows: the native Directory on Unix and a stub on Windows feed
//! the same capture, scratch, copy, cleanup poller and report derivation.
use super::*;
use crate::supervisor::file_io::FileCapture;
use crate::supervisor::file_io_hooks::Hooks;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};

#[derive(Default)]
struct Files {
    faults: &'static str,
    read_size: Option<u64>,
    unverified: bool,
    delete_input: bool,
    refuse: bool,
    captured: Vec<Arc<std::sync::atomic::AtomicU64>>,
}
impl Executor for Files {
    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        cancel: CancelToken,
        _events: RunEvents,
        mut resources: RunResources,
    ) -> JobFuture<'_, ExecutionRecord> {
        Box::pin(async move {
            let mut setup = *resources.file_io.take().unwrap();
            let state = setup.state.clone();
            let retained = resources.retained_counter();
            self.captured.push(retained.clone());
            let counter = retained.clone();
            let owner = resources._owner.clone();
            let mut admission = supervisor::admit().unwrap();
            if self.refuse {
                let bounds = supervisor::global_bounds().unwrap();
                let mut held = Vec::new();
                while let Some(c) = bounds.thread_credits.try_reserve(1) {
                    held.push(c);
                }
                let refusal = admission.worker_credits(1).unwrap_err();
                return ExecutionRecord::NotStarted(executor::start_failure(refusal));
            }
            let credit = admission.worker_credits(1).unwrap().pop();
            setup.hooks = Some(Hooks::testing(self.faults, self.read_size));
            let worker_cancel = cancel.clone();
            let (send, mut receive) = oneshot::channel();
            // Preparation owns H and the staged charge even if its async caller
            // returns first. FileCapture removes on its own cancelled boundary.
            let worker = std::thread::spawn(move || {
                let prepared = FileCapture::prepare(setup, credit, retained, || {
                    if worker_cancel.is_cancelled() {
                        Err(supervisor::NotStarted::CancelledBeforeStart)
                    } else {
                        Ok(())
                    }
                });
                if let Err(Ok(mut capture)) = send.send(prepared) {
                    capture.abort_setup();
                }
            });
            admission.hold_resources(resources, counter);
            let prepared = tokio::select! {
                biased;
                result = &mut receive => result.unwrap(),
                _ = cancel.cancelled() => {
                    // Avoid dropping a completed capture if its send won the
                    // cancellation race: the same creation owner aborts it.
                    drop(receive);
                    admission.transfer_file_creation(worker);
                    return ExecutionRecord::NotStarted(executor::start_failure(supervisor::NotStarted::CancelledBeforeStart));
                }
            };
            worker.join().unwrap();
            let mut capture = match prepared {
                Ok(capture) => Box::new(capture),
                Err(e) => return ExecutionRecord::NotStarted(executor::start_failure(e)),
            };
            capture.created();
            fs::write(spec.directory.join("cwd").join("out.txt"), b"yes\n").unwrap();
            if self.delete_input {
                fs::remove_file(spec.directory.join("cwd").join("in.txt")).unwrap();
            }
            let mut outcome = root(0);
            if self.unverified {
                outcome.termination.cleanup = supervisor::Cleanup::Unverified {
                    reason: supervisor::UnverifiedReason::ProcUnavailable,
                };
            }
            let start = tokio::time::Instant::now();
            let deadline = start + Duration::from_secs(5);
            while !capture.poll(&mut outcome, true, tokio::time::Instant::now() >= deadline) {
                tokio::task::yield_now().await;
            }
            if matches!(
                outcome.output_file,
                Some(report::OutputFile::NotCollected {
                    reason: report::CollectionFailure::Timeout,
                    ..
                })
            ) {
                assert!(start.elapsed() >= Duration::from_secs(5));
                assert!(start.elapsed() < Duration::from_secs(7));
                admission.transfer_file_io(capture);
            } else {
                while !capture.released() {
                    tokio::task::yield_now().await;
                }
                // The production cwd worker uses exactly this Scratch::clear.
                if let Some(mut scratch) = state.take() {
                    let (same, excluded) = scratch.clear(&spec.directory.join("cwd"), &state);
                    assert!(same);
                    if excluded > 0 {
                        assert_eq!(excluded, 6);
                        let guard = crate::storage::WorkspaceGuard::leased(&owner);
                        let measured =
                            crate::storage::measure_tree(&guard, spec.directory.join("cwd"))
                                .unwrap();
                        assert!(measured.errors.is_empty() && measured.skipped.is_empty());
                        assert_eq!(measured.bytes.saturating_sub(excluded), 4);
                    }
                }
                drop(admission);
            }
            ExecutionRecord::Started(Box::new(outcome))
        })
    }
}

async fn idle() {
    tokio::time::timeout(Duration::from_secs(5), async {
        while supervisor::debug_counters().service_pending != 0
            || supervisor::debug_counters().credits_in_use != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(crate::supervisor::file_io_hooks::directories(), 0);
}
fn engine(f: &Fixture, count: usize, input: bool, limit: u64) -> Engine<'static> {
    let mut engine = Engine::new(
        ready_with_budget(f, "python", count, &Tools, limit),
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
    // The adapter runs Scratch::clear on the physical owner, so no second
    // cwd worker is needed (Windows production has no such worker).
    engine.ready.cwd_bytes = None;
    for plan in &mut engine.plan {
        let Some(PlannedTarget::BuildFree(free)) = &engine.ready.planned_target else {
            panic!("build-free")
        };
        let mut stage = free.run(engine.ready.dir.join("cwd")).unwrap();
        stage.stdout.bytes.value = 4096;
        stage.stderr.bytes.value = 4096;
        plan.recipe = Recipe::Resolved(Box::new(stage));
    }
    engine
}
fn header(data: &ReportData, count: u32, input: bool, code: i64) {
    assert_eq!(
        data.details.target.as_ref().unwrap().io,
        Some(report::FileIo {
            input: input.then(|| "in.txt".parse().unwrap()),
            output: Some("out.txt".parse().unwrap())
        })
    );
    assert_eq!(data.executions.len(), count as usize);
    assert_eq!(data.tests.len(), count as usize);
    assert_eq!(data.result.counts.tests, count);
    let mut coverage = report::Cov {
        planned: count,
        ..report::Cov::default()
    };
    for execution in &data.executions {
        match execution.result.coverage {
            Coverage::Started => coverage.started += 1,
            Coverage::NotStarted { .. } => coverage.not_started += 1,
            Coverage::Skipped { .. } => coverage.skipped += 1,
            ref other => panic!("unexpected coverage: {other:?}"),
        }
    }
    assert_eq!(data.result.counts.primary, coverage);
    assert_eq!(data.details.pre_delivery_exit.code, code);
}
async fn run_files(engine: Engine<'_>, files: &mut Files) -> Arc<FinalizedReport> {
    engine
        .run(files, &mut MemoryPersistence::default(), &mut NoHooks)
        .await
}

// One subprocess isolates process-wide credits and counters on every OS.
#[test]
fn portable_file_rows() {
    const CHILD: &str = "CPF_PORTABLE_FILE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "jobs::engine::tests::file_io_tests::portable_file_rows",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    drop(supervisor::persistence_credit().unwrap());
    run_test(move || async move {
        collection_rows().await;
        removal_rows().await;
        for (point, cancel) in [
            ("file_io.listing.entry", true),
            ("file_io.listing.entry", false),
            ("file_io.write", false),
        ] {
            stall_row(point, cancel).await;
        }
        for after in [false, true] {
            staging_row(after).await;
        }
        let f = Fixture::new();
        let result = run_files(
            engine(&f, 1, false, 256 << 20),
            &mut Files {
                refuse: true,
                ..Files::default()
            },
        )
        .await;
        let data = result.data();
        header(data, 1, false, 10);
        assert!(matches!(
            data.tests[0].class,
            Some(RunClass::Infrastructure { .. })
        ));
        assert_eq!(
            data.tests[0].verdict,
            Some(report::TestVerdict::Infrastructure)
        );
        assert!(matches!(
            data.tests[0].check.as_ref().unwrap().kind,
            report::CheckKind::NotAdmitted { .. }
        ));
        assert_eq!(
            data.tests[0].output_file,
            Some(report::OutputFile::NotAttempted)
        );
        assert_eq!(
            data.executions[0].result.coverage,
            Coverage::NotStarted {
                reason: NotStartedReason::Worker
            }
        );
        assert_eq!(data.result.counts.by_class.infrastructure, 1);
        assert_eq!(data.result.counts.by_verdict.infrastructure, 1);
    });
}

// F-09, F-13 (read and write), F-23. F-32's kept-copy charge uses the
// same bounded copier here; the real descendant row supplies actual growth.
async fn collection_rows() {
    for (faults, size, unverified, status) in [
        ("", Some(8192), false, 1),
        ("file_io.read=EIO@2", None, false, 10),
        ("file_io.write=ENOSPC", None, false, 10),
        ("", None, true, 0),
        ("file_io.partial.unlink=EIO", Some(8192), false, 1),
    ] {
        let f = Fixture::new();
        let engine = engine(&f, 1, false, 256 << 20);
        let directory = engine.ready.dir.clone();
        let budget = engine.ready.budget.clone();
        let baseline = budget.usage().retained;
        let mut files = Files {
            faults,
            read_size: size,
            unverified,
            ..Files::default()
        };
        let result = run_files(engine, &mut files).await;
        let data = result.data();
        header(data, 1, false, status);
        let test = &data.tests[0];
        assert_eq!(data.executions[0].result.coverage, Coverage::Started);
        assert_eq!(data.result.counts.by_class.exited_zero, 1);
        let output = directory.join("r00000").join("out").join("output");
        if size.is_some() {
            assert_eq!(
                test.output_file,
                Some(report::OutputFile::TooLarge {
                    bytes: 4097,
                    budget: 4096
                })
            );
            assert!(matches!(test.class, Some(RunClass::OutputBudget { .. })));
            assert_eq!(test.verdict, Some(report::TestVerdict::OutputBudget));
            assert!(matches!(
                test.check.as_ref().unwrap().kind,
                report::CheckKind::OutputRejected {
                    reason: report::OutputRejectReason::TooLarge
                }
            ));
            assert_eq!(data.result.counts.by_verdict.output_budget, 1);
        } else if unverified {
            assert_eq!(test.class, Some(RunClass::ExitedZero));
            assert_eq!(test.verdict, Some(report::TestVerdict::Passed));
            assert!(matches!(
                test.check.as_ref().unwrap().kind,
                report::CheckKind::Passed
            ));
            assert!(matches!(
                test.output_file,
                Some(report::OutputFile::Collected { bytes: 4, .. })
            ));
            assert!(
                data.executions[0]
                    .summary
                    .as_ref()
                    .unwrap()
                    .notes
                    .contains(&report::RunNote::CleanupUnverified)
            );
            assert_eq!(data.result.counts.by_verdict.passed, 1);
        } else {
            assert_eq!(test.class, Some(RunClass::ExitedZero));
            assert_eq!(test.verdict, Some(report::TestVerdict::Infrastructure));
            assert!(matches!(
                test.output_file,
                Some(report::OutputFile::NotCollected {
                    reason: report::CollectionFailure::Io,
                    os_error: Some(_)
                })
            ));
            assert!(matches!(
                test.check.as_ref().unwrap().kind,
                report::CheckKind::CheckError { os_error: Some(_) }
            ));
            if let (
                Some(report::OutputFile::NotCollected {
                    os_error: collected,
                    ..
                }),
                report::CheckKind::CheckError { os_error: checked },
            ) = (&test.output_file, &test.check.as_ref().unwrap().kind)
            {
                assert_eq!(collected, checked);
            }
            assert_eq!(data.result.counts.by_verdict.infrastructure, 1);
        }
        let kept = faults == "file_io.partial.unlink=EIO";
        assert_eq!(output.exists(), kept || unverified);
        assert_eq!(
            data.executions[0]
                .io_warnings
                .contains(&report::IoScratchDetail::PartialCopyKept),
            kept
        );
        if kept {
            assert_eq!(files.captured[0].load(Ordering::Acquire), 4096);
            assert_eq!(fs::metadata(output).unwrap().len(), 4096);
            assert_eq!(budget.usage().retained, baseline + 4096);
        }
        assert_eq!(
            budget.usage().retained,
            baseline
                + if kept {
                    4096
                } else if unverified {
                    4
                } else {
                    0
                }
        );
        assert!(budget.usage().retained + budget.usage().reserved <= budget.limit());
        drop(result);
        released_budget(&budget).await;
        assert_eq!(
            budget.usage(),
            crate::storage::budget::BudgetUsage::default()
        );
        idle().await;
    }
}

async fn removal_rows() {
    let f = Fixture::new();
    let probe = engine(&f, 4, true, 256 << 20);
    let baseline = probe.ready.budget.usage();
    let (tests, aggregates) = report_populations(probe.kind, &probe.plan, 4);
    let report_bytes =
        report::wire::report_reservation(4, tests, aggregates, probe.report_shape).unwrap();
    let limit = baseline.retained + report_bytes + 3 * METADATA + 3 * 4096 + 2 * (6 + 4);
    drop(probe);
    let f = Fixture::new();
    let job = engine(&f, 4, true, limit);
    let budget = job.ready.budget.clone();
    let observed_budget = budget.clone();
    let job = job.observe(move |_| {
        let usage = observed_budget.usage();
        assert!(usage.retained + usage.reserved <= limit);
    });
    let mut files = Files {
        faults: "file_io.unlink=EIO",
        ..Files::default()
    };
    let result = run_files(job, &mut files).await;
    let data = result.data();
    header(data, 4, true, 10);
    for i in 0..2 {
        assert_eq!(data.executions[i].result.coverage, Coverage::Started);
        assert_eq!(data.tests[i].class, Some(RunClass::ExitedZero));
        assert_eq!(data.tests[i].verdict, Some(report::TestVerdict::Passed));
        assert!(matches!(
            data.tests[i].check.as_ref().unwrap().kind,
            report::CheckKind::Passed
        ));
        assert!(matches!(
            data.tests[i].output_file,
            Some(report::OutputFile::Collected { bytes: 4, .. })
        ));
        assert_eq!(
            data.executions[i].io_warnings,
            [
                report::IoScratchDetail::StagedInputKept,
                report::IoScratchDetail::OutputKept
            ]
        );
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
        assert_eq!(test.output_file, Some(report::OutputFile::NotAttempted));
        assert!(matches!(
            test.check.as_ref().unwrap().kind,
            report::CheckKind::NotAdmitted {
                reason: report::CheckNotAdmitted::RunNotEligible
            }
        ));
    }
    assert_eq!(data.result.counts.by_class.exited_zero, 2);
    assert_eq!(data.result.counts.by_class.infrastructure, 1);
    assert_eq!(data.result.counts.by_class.skipped, 1);
    assert_eq!(data.result.counts.by_verdict.passed, 2);
    assert_eq!(data.result.counts.by_verdict.infrastructure, 1);
    assert_eq!(data.result.counts.by_verdict.not_run, 1);
    assert_eq!(budget.usage().retained, baseline.retained + 2 * (6 + 4));
    assert!(budget.usage().retained + budget.usage().reserved <= limit);
    drop(result);
    released_budget(&budget).await;
    assert_eq!(
        budget.usage(),
        crate::storage::budget::BudgetUsage::default()
    );
    // Program-deleted staged copy: no staged-input warning or retained charge.
    let f = Fixture::new();
    let job = engine(&f, 1, true, 256 << 20);
    let budget = job.ready.budget.clone();
    let baseline = budget.usage().retained;
    let result = run_files(
        job,
        &mut Files {
            delete_input: true,
            faults: "file_io.unlink=EIO",
            ..Files::default()
        },
    )
    .await;
    header(result.data(), 1, true, 0);
    assert_eq!(budget.usage().retained, baseline + 4);
    assert_eq!(
        result.data().executions[0].io_warnings,
        [report::IoScratchDetail::OutputKept]
    );
}

async fn stall_row(point: &'static str, cancelling: bool) {
    let f = Fixture::new();
    let engine = engine(&f, 1, false, 256 << 20);
    let directory = engine.ready.dir.clone();
    let budget = engine.ready.budget.clone();
    let entered = CancelToken::new();
    let signal = entered.clone();
    let (send, receive) = mpsc::channel();
    let receive = Mutex::new(receive);
    let log = Arc::new(Mutex::new(Vec::new()));
    let operations = log.clone();
    let once = AtomicBool::new(false);
    let engine = engine.observe(move |event| {
        if let Point::FileIo {
            point: operation, ..
        } = event
        {
            operations
                .lock()
                .unwrap()
                .push((operation, std::thread::current().id()));
            if operation == point && !once.swap(true, Ordering::AcqRel) {
                signal.cancel();
                receive
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(20))
                    .unwrap();
            }
        }
    });
    let cancel = engine.cancellation();
    let mut files = Files::default();
    let mut persistence = MemoryPersistence::default();
    let mut hooks = NoHooks;
    let mut future = Box::pin(engine.run(&mut files, &mut persistence, &mut hooks));
    tokio::select! { _ = entered.cancelled() => {}, _ = future.as_mut() => panic!("ended before stall") }
    if cancelling {
        cancel.request_cancel(CancelSource::Api);
    }
    let result = tokio::time::timeout(Duration::from_secs(7), future.as_mut())
        .await
        .unwrap();
    drop(future);
    header(result.data(), 1, false, if cancelling { 6 } else { 10 });
    let test = &result.data().tests[0];
    assert_eq!(
        test.output_file,
        Some(report::OutputFile::NotCollected {
            reason: report::CollectionFailure::Timeout,
            os_error: None
        })
    );
    assert_eq!(test.class, Some(RunClass::ExitedZero));
    assert_eq!(result.data().result.counts.by_class.exited_zero, 1);
    assert_eq!(
        result.data().result.counts.by_verdict.cancelled,
        u32::from(cancelling)
    );
    assert_eq!(
        result.data().result.counts.by_verdict.infrastructure,
        u32::from(!cancelling)
    );
    if cancelling {
        assert_eq!(test.verdict, Some(report::TestVerdict::Cancelled));
        assert!(matches!(
            test.check.as_ref().unwrap().kind,
            report::CheckKind::NotAdmitted {
                reason: report::CheckNotAdmitted::Cancelled
            }
        ));
    } else {
        assert_eq!(test.verdict, Some(report::TestVerdict::Infrastructure));
        assert!(matches!(
            test.check.as_ref().unwrap().kind,
            report::CheckKind::CheckError { os_error: None }
        ));
    }
    assert!(
        result.data().executions[0]
            .io_warnings
            .contains(&report::IoScratchDetail::NotMeasured)
    );
    assert!(budget.usage().reserved >= 4096);
    assert_eq!(supervisor::debug_counters().service_pending, 1);
    assert_eq!(supervisor::debug_counters().credits_in_use, 2);
    assert_eq!(crate::supervisor::file_io_hooks::directories(), 1);
    let before = log.lock().unwrap().clone();
    drop(result);
    drop(persistence);
    drop(files);
    assert!(budget.usage().reserved >= 4096);
    send.send(()).unwrap();
    idle().await;
    assert!(!directory.join("r00000").join("out").join("output").exists());
    released_budget(&budget).await;
    assert_eq!(
        budget.usage(),
        crate::storage::budget::BudgetUsage::default()
    );
    let log = log.lock().unwrap();
    let worker = before.last().unwrap().1;
    assert!(
        log[before.len()..]
            .iter()
            .all(|(_, thread)| *thread == worker)
    );
    assert!(log.iter().any(|(p, _)| *p == "file_io.partial.unlink"));
}

async fn staging_row(after_job: bool) {
    let f = Fixture::new();
    let engine = engine(&f, 2, true, 256 << 20);
    let directory = engine.ready.dir.clone();
    let budget = engine.ready.budget.clone();
    let entered = CancelToken::new();
    let signal = entered.clone();
    let (send, receive) = mpsc::channel();
    let before_return = send.clone();
    let receive = Mutex::new(receive);
    let log = Arc::new(Mutex::new(Vec::new()));
    let operations = log.clone();
    let engine = engine.observe(move |event| {
        if !after_job && event == Point::BeforeSnapshot {
            before_return.send(()).unwrap();
        }
        if let Point::FileIo { point, .. } = event {
            operations
                .lock()
                .unwrap()
                .push((point, std::thread::current().id()));
            if point == "file_io.staging.write" {
                signal.cancel();
                receive
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(20))
                    .unwrap();
            }
        }
    });
    let cancel = engine.cancellation();
    let mut files = Files::default();
    let mut persistence = MemoryPersistence::default();
    let mut hooks = NoHooks;
    let mut future = Box::pin(engine.run(&mut files, &mut persistence, &mut hooks));
    tokio::select! { _ = entered.cancelled() => {}, _ = future.as_mut() => panic!("ended before staging") }
    cancel.request_cancel(CancelSource::Api);
    let result = tokio::time::timeout(Duration::from_secs(2), future.as_mut())
        .await
        .unwrap();
    drop(future);
    let data = result.data();
    header(data, 2, true, 6);
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
    for test in &data.tests {
        assert_eq!(test.verdict, Some(report::TestVerdict::Cancelled));
        assert_eq!(test.output_file, Some(report::OutputFile::NotAttempted));
    }
    assert_eq!(data.tests[0].class, Some(RunClass::Cancelled));
    assert_eq!(data.tests[1].class, None);
    assert!(matches!(
        data.tests[0].check.as_ref().unwrap().kind,
        report::CheckKind::NotAdmitted {
            reason: report::CheckNotAdmitted::RunNotEligible
        }
    ));
    assert!(matches!(
        data.tests[1].check.as_ref().unwrap().kind,
        report::CheckKind::NotAdmitted {
            reason: report::CheckNotAdmitted::Cancelled
        }
    ));
    assert_eq!(data.result.counts.by_verdict.cancelled, 2);
    assert_eq!(data.result.counts.by_verdict.not_run, 0);
    assert_eq!(data.result.counts.by_class.cancelled, 1);
    assert_eq!(data.result.counts.by_class.skipped, 1);
    let staged = directory.join("r00000").join("cwd").join("in.txt");
    if after_job {
        assert!(staged.exists());
        assert!(budget.usage().reserved >= 6);
        assert_eq!(supervisor::debug_counters().service_pending, 1);
        assert_eq!(supervisor::debug_counters().credits_in_use, 2);
    }
    let before = log.lock().unwrap().clone();
    drop(result);
    drop(persistence);
    drop(files);
    if after_job {
        send.send(()).unwrap();
    }
    idle().await;
    assert!(!staged.exists());
    released_budget(&budget).await;
    assert_eq!(
        budget.usage(),
        crate::storage::budget::BudgetUsage::default()
    );
    let log = log.lock().unwrap();
    let worker = before.last().unwrap().1;
    assert!(
        log[before.len()..]
            .iter()
            .all(|(_, thread)| *thread == worker)
    );
}
