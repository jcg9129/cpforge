//! Portable execution adapter for retirement: frozen injected roles, actual
//! reservations, file sinks and the existing Executor and persistence worker.
use super::*;
use crate::jobs::engine::stress::*;
use crate::jobs::persist::JobPersistence;
use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
struct Injected {
    held: Vec<RunResources>,
    hold: bool,
    unverified: bool,
    code: i64,
    /// Extra counted bytes beyond what was written, past the reservation.
    extra: u64,
}

impl Executor for Injected {
    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        cancel: CancelToken,
        _events: RunEvents,
        resources: RunResources,
    ) -> JobFuture<'_, ExecutionRecord> {
        Box::pin(async move {
            let bytes = b"yes\n";
            fs::write(spec.directory.join("out").join("stdout"), bytes).unwrap();
            resources
                .retained_counter()
                .fetch_add(bytes.len() as u64 + self.extra, Ordering::Release);
            let mut record = root(self.code);
            if cancel.is_cancelled() {
                record.stop_trigger = Some(supervisor::StopTrigger::Cancelled);
            }
            if self.unverified {
                record.termination.cleanup = supervisor::Cleanup::Unverified {
                    reason: supervisor::UnverifiedReason::Unsupported,
                };
            }
            if self.hold && spec.run.as_str().ends_with(".candidate") {
                self.held.push(resources);
            } else {
                drop(resources);
            }
            ExecutionRecord::Started(Box::new(record))
        })
    }
}

fn controller(n: u64, cancel: CancelState) -> StressAdmission<BatchRole> {
    let role = |r| RunTemplate::new(r, 4096, 1024).unwrap();
    StressAdmission::new(
        BatchTemplates::new(
            role(BatchRole::Generator),
            Some(role(BatchRole::Validator)),
            Some(role(BatchRole::Brute)),
            role(BatchRole::Candidate),
            Some(role(BatchRole::Checker)),
        )
        .unwrap(),
        Schedule::Search {
            start_seed: 19,
            budget: SearchBudget {
                iterations: n,
                time: Duration::from_secs(60),
            },
            keep_going: true,
        },
        cancel,
        true,
        Box::<MonotonicClock>::default(),
    )
    .unwrap()
}

async fn records(
    live: &mut LiveIteration<'_, BatchRole>,
    ready: &ReadyJob<'_>,
    fake: &mut Injected,
) -> Vec<RetirementRun> {
    let mut records = Vec::new();
    let free = match &ready.planned_target {
        PlannedTarget::BuildFree(free) => free,
        PlannedTarget::Build(_) => panic!("injected Python role needs no build"),
    };
    while let Some(run) = live.admit_run().unwrap() {
        let directory = run.id.directory(&ready.dir);
        let (streams, metadata) = reserve(&ready.budget, 5120).unwrap();
        fs::create_dir_all(directory.join("out")).unwrap();
        fs::create_dir(directory.join("cwd")).unwrap();
        let resolved = free.run(directory.join("cwd")).unwrap();
        let command = resolved.launch_command(Platform::host()).unwrap();
        let mut spec = Box::new(ExecutionSpec {
            // Retirement uses the typed iteration identity. This execution
            // adapter offers no run events and allocates no report run ids.
            run: RunIdentity::Iteration(run.id),
            execution: 0,
            role: Role::Test {
                test: "injected".into(),
            },
            stage: Stage::Run,
            command,
            resolved,
            stdin: None,
            stage_input: None,
            output_file: None,
            directory: directory.clone(),
            output_events: false,
            digest: None,
        });
        spec.digest = Some(spec.spec_digest());
        let encoded_spec = crate::report::wire::encode_spec(&spec.wire_spec()).unwrap();
        let digest = spec.digest;
        let retained_bytes = Arc::new(AtomicU64::new(0));
        let mut resources = RunResources {
            #[cfg(any(
                target_os = "linux",
                target_os = "macos",
                all(test, feature = "storage-test-hooks")
            ))]
            file_io: None,
            release: None,
            reservation: streams,
            _owner: ready.owner.clone(),
            retained_bytes: retained_bytes.clone(),
        };
        let release = resources.track_release();
        let events = RunEvents {
            events: Events::new(),
            run: spec.run.clone(),
            output: false,
        };
        let record = Arc::new(
            fake.execute(Arc::from(spec), live_cancel(live), events, resources)
                .await,
        );
        live.record_finalized(RunCompletion {
            started: true,
            captured_bytes: 4,
            gate: RunGate::Continue,
            retention_refused: false,
        })
        .unwrap();
        records.push(RetirementRun {
            id: run.id,
            encoded_spec: Some(encoded_spec),
            digest,
            record,
            metadata: Some(metadata),
            writable: true,
            release,
            retained_bytes,
            staging_release: RunRelease::not_held(),
        });
    }
    records
}

fn live_cancel(live: &LiveIteration<'_, BatchRole>) -> CancelToken {
    // Child fixtures exercise admission arbitration with the same intent/token.
    live.cancellation_token()
}

fn all_published(result: &RetirementResult, ready: &ReadyJob<'_>) {
    assert_eq!(result.records.len(), 5);
    for run in &result.records {
        for reference in [&run.spec, &run.record, &run.cleanup] {
            assert_eq!(reference.error, None);
            let path = reference.path.as_ref().unwrap();
            assert!(path.starts_with("iterations"));
            assert!(ready.dir.join(path).is_file());
        }
        let bytes = fs::read(ready.dir.join(run.record.path.as_ref().unwrap())).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json.is_object());
    }
}

#[derive(Debug, Default)]
struct Hooks {
    promotions: usize,
    fail: Option<PathBuf>,
}
impl RetirementHooks for Hooks {
    fn promote<'a>(&'a mut self, _runs: &'a [RetirementRun]) -> JobFuture<'a, SaveOutcome> {
        self.promotions += 1;
        Box::pin(async { SaveOutcome::Promoted })
    }
    fn remove(
        &mut self,
        root: &Path,
        path: &Path,
    ) -> Result<crate::storage::WalkReport, crate::storage::StorageError> {
        if let Some(fail) = self.fail.as_ref().filter(|f| f.starts_with(path)) {
            return Ok(crate::storage::WalkReport {
                errors: vec![(
                    fail.clone(),
                    std::io::Error::other("injected removal failure"),
                )],
                ..Default::default()
            });
        }
        crate::storage::remove_owned_tree(root, path)
    }
}

#[test]
fn st_l01_st_l11_batch_retirement_evicts_and_releases_before_next_iteration() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let baseline = ready.budget.usage();
        let mut admission = controller(100, CancelState::new());
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let mut fake = Injected::default();
        for k in 0..100 {
            let mut live = admission.admit().unwrap();
            let runs = records(&mut live, &ready, &mut fake).await;
            let batch = RetirementBatch::new(runs, tokio::time::Instant::now()).unwrap();
            let probe = batch.probe();
            let debug = batch.debug(&persistence);
            assert_eq!(
                (debug.live_iterations, debug.records, debug.specs),
                (1, 5, 5)
            );
            assert_eq!(debug.metadata_reserved, 5 << 20);
            let result = batch
                .retire(
                    live.begin_retirement(IterationClass::Passed).unwrap(),
                    ready.owner.clone(),
                    &ready.budget,
                    &mut persistence,
                    &mut Hooks::default(),
                )
                .await
                .unwrap();
            assert!(result.cleanup_failed.paths.is_empty());
            assert!(result.records.is_empty());
            assert_eq!(admission.counters().retired, k + 1);
            assert_eq!(ready.budget.usage(), baseline);
            assert_eq!(probe.snapshot(&persistence), RetirementDebug::default());
            assert_eq!(
                fs::read_dir(ready.dir.join("iterations")).unwrap().count(),
                0
            );
        }
        assert_eq!(admission.counters().runs_started, 500);
        assert_eq!(
            admission.admit().unwrap_err().reason,
            StopReason::Iterations
        );
        let budget = ready.budget.clone();
        drop(persistence);
        drop(ready);
        released_budget(&budget).await;
    });
}

#[test]
fn r11_never_releases_more_than_a_run_committed() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let baseline = ready.budget.usage();
        let mut admission = controller(1, CancelState::new());
        let mut persistence = JobPersistence::new(&ready).unwrap();
        // Each run's counter claims more than its 5 120-byte stream reservation,
        // whose commit is clamped to the reservation.
        let mut fake = Injected {
            extra: 10_000,
            ..Default::default()
        };
        let mut live = admission.admit().unwrap();
        let runs = records(&mut live, &ready, &mut fake).await;
        assert_eq!(ready.budget.usage().retained, baseline.retained + 5 * 5120);
        RetirementBatch::new(runs, tokio::time::Instant::now())
            .unwrap()
            .retire(
                live.begin_retirement(IterationClass::Passed).unwrap(),
                ready.owner.clone(),
                &ready.budget,
                &mut persistence,
                &mut Hooks::default(),
            )
            .await
            .unwrap();
        assert_eq!(ready.budget.usage(), baseline);
        assert_eq!(admission.counters().retired, 1);
    });
}

#[test]
fn a_run_without_resources_holds_nothing_for_r6() {
    run_test(move || async move {
        tokio::time::timeout(Duration::from_millis(100), RunRelease::not_held().wait())
            .await
            .expect("a refused run holds no resources");
    });
}

#[test]
fn st_l03_cancelled_next_role_keeps_admitted_evidence() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let cancel = CancelState::new();
        let mut admission = controller(100, cancel.clone());
        admission.observe_engine(move |point| {
            if matches!(
                point,
                Point::StressRunAdmit(IterationRunId {
                    iteration: 7,
                    role: BatchRole::Candidate
                })
            ) {
                cancel.request_cancel(CancelSource::Api);
            }
        });
        let mut persistence = JobPersistence::new(&ready).unwrap();
        for k in 0..8 {
            let mut live = admission.admit().unwrap();
            let runs = records(&mut live, &ready, &mut Injected::default()).await;
            let batch = RetirementBatch::new(runs, tokio::time::Instant::now()).unwrap();
            let lease = live.begin_retirement(IterationClass::Passed).unwrap();
            assert_eq!(
                lease.disposition(),
                if k == 7 {
                    Disposition::Keep
                } else {
                    Disposition::Discard
                }
            );
            batch
                .retire(
                    lease,
                    ready.owner.clone(),
                    &ready.budget,
                    &mut persistence,
                    &mut Hooks::default(),
                )
                .await
                .unwrap();
        }
        assert_eq!(admission.admit().unwrap_err().reason, StopReason::Cancelled);
        assert_eq!(admission.counters().retired, 8);
        for role in [BatchRole::Generator, BatchRole::Validator, BatchRole::Brute] {
            let directory = IterationRunId { iteration: 7, role }.directory(&ready.dir);
            for name in ["spec.json", "record.json", "cleanup.log"] {
                assert!(directory.join(name).is_file());
            }
        }
        assert!(
            !IterationRunId {
                iteration: 7,
                role: BatchRole::Candidate
            }
            .directory(&ready.dir)
            .exists()
        );
    });
}

#[test]
fn st_l05_cancellation_at_promotion_finishes_retirement() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let cancel = CancelState::new();
        let mut admission = controller(100, cancel.clone());
        let mut live = admission.admit().unwrap();
        let runs = records(&mut live, &ready, &mut Injected::default()).await;
        let batch = RetirementBatch::new(runs, tokio::time::Instant::now())
            .unwrap()
            .observe(move |point| {
                if matches!(
                    point,
                    Point::StressRetire {
                        step: RetirementStep::Promote,
                        ..
                    }
                ) {
                    cancel.request_cancel(CancelSource::Api);
                }
            });
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let mut hooks = Hooks::default();
        let result = batch
            .retire(
                live.begin_retirement(IterationClass::CandidateFailure)
                    .unwrap(),
                ready.owner.clone(),
                &ready.budget,
                &mut persistence,
                &mut hooks,
            )
            .await
            .unwrap();
        assert_eq!(hooks.promotions, 1);
        assert_eq!(result.saved, Some(SaveOutcome::Promoted));
        all_published(&result, &ready);
        assert_eq!(admission.counters().retired, 1);
        assert_eq!(admission.admit().unwrap_err().reason, StopReason::Cancelled);
    });
}

#[test]
fn st_l09_removal_failure_retains_bytes_and_closes_admission() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let baseline = ready.budget.usage().retained;
        let mut admission = controller(100, CancelState::new());
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let failed = IterationRunId {
            iteration: 5,
            role: BatchRole::Candidate,
        }
        .directory(&ready.dir)
        .join("cwd")
        .join("held");
        let mut hooks = Hooks::default();
        for k in 0..6 {
            let mut live = admission.admit().unwrap();
            let runs = records(&mut live, &ready, &mut Injected::default()).await;
            if k == 5 {
                fs::write(&failed, b"child output").unwrap();
                hooks.fail = Some(failed.clone());
            }
            let result = RetirementBatch::new(runs, tokio::time::Instant::now())
                .unwrap()
                .retire(
                    live.begin_retirement(IterationClass::Passed).unwrap(),
                    ready.owner.clone(),
                    &ready.budget,
                    &mut persistence,
                    &mut hooks,
                )
                .await
                .unwrap();
            if k == 5 {
                assert_eq!(
                    result.cleanup_failed.paths,
                    [failed.strip_prefix(&ready.dir).unwrap().to_path_buf()]
                );
                assert_eq!(ready.budget.usage().retained, baseline + 4);
            }
        }
        assert_eq!(
            admission.admit().unwrap_err().reason,
            StopReason::CleanupFailed
        );
        assert_eq!(admission.counters().retired, 6);
        assert!(failed.is_file());
    });
}

#[test]
fn st_l10_stalled_writer_skips_promotion_and_keeps_all_scratch() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let mut admission = controller(100, CancelState::new());
        let mut live = admission.admit().unwrap();
        let mut fake = Injected {
            hold: true,
            ..Default::default()
        };
        let runs = records(&mut live, &ready, &mut fake).await;
        let finalized = tokio::time::Instant::now();
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let mut hooks = Hooks::default();
        let result = RetirementBatch::new(runs, finalized)
            .unwrap()
            .retire(
                live.begin_retirement(IterationClass::CandidateFailure)
                    .unwrap(),
                ready.owner.clone(),
                &ready.budget,
                &mut persistence,
                &mut hooks,
            )
            .await
            .unwrap();
        let elapsed = finalized.elapsed();
        assert!((Duration::from_secs(5)..Duration::from_millis(5500)).contains(&elapsed));
        assert_eq!(hooks.promotions, 0);
        assert_eq!(
            result.saved,
            Some(SaveOutcome::NotSaved(NotSavedReason::ResourcesHeld))
        );
        assert_eq!(result.cleanup_failed.paths.len(), 1);
        assert_eq!(
            admission.admit().unwrap_err().reason,
            StopReason::CleanupFailed
        );
        assert!(ready.budget.usage().reserved >= 5120);
        drop(fake);
        for role in BatchRole::ALL {
            assert!(
                IterationRunId { iteration: 0, role }
                    .directory(&ready.dir)
                    .join("out")
                    .join("stdout")
                    .is_file()
            );
        }
    });
}

#[test]
fn st_l20_metadata_owned_until_r5_and_saved_artifacts_settle() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let baseline = ready.budget.usage().reserved;
        let mut admission = controller(11, CancelState::new());
        let mut persistence = JobPersistence::new(&ready).unwrap();
        for k in 0..11 {
            let mut live = admission.admit().unwrap();
            let runs = records(&mut live, &ready, &mut Injected::default()).await;
            assert_eq!(ready.budget.usage().reserved, baseline + (5 << 20));
            let class = if k == 10 {
                IterationClass::CandidateFailure
            } else {
                IterationClass::Passed
            };
            let result = RetirementBatch::new(runs, tokio::time::Instant::now())
                .unwrap()
                .retire(
                    live.begin_retirement(class).unwrap(),
                    ready.owner.clone(),
                    &ready.budget,
                    &mut persistence,
                    &mut Hooks::default(),
                )
                .await
                .unwrap();
            assert_eq!(ready.budget.usage().reserved, baseline);
            if k == 10 {
                all_published(&result, &ready);
            }
        }
        let budget = ready.budget.clone();
        drop(persistence);
        drop(ready);
        released_budget(&budget).await;
    });
}

#[test]
fn st_l21_cleanup_unverified_does_not_prevent_retirement() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let mut admission = controller(100, CancelState::new());
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let mut fake = Injected {
            unverified: true,
            ..Default::default()
        };
        for _ in 0..100 {
            let mut live = admission.admit().unwrap();
            let runs = records(&mut live, &ready, &mut fake).await;
            for run in &runs {
                let input = run.record.input(false, [supervisor::Exceed::Kill; 2]);
                assert_eq!(report::classify_run(&input), RunClass::ExitedZero);
                assert!(report::run_notes(&input).contains(&report::RunNote::CleanupUnverified));
            }
            RetirementBatch::new(runs, tokio::time::Instant::now())
                .unwrap()
                .retire(
                    live.begin_retirement(IterationClass::Passed).unwrap(),
                    ready.owner.clone(),
                    &ready.budget,
                    &mut persistence,
                    &mut Hooks::default(),
                )
                .await
                .unwrap();
        }
        assert_eq!(admission.counters().retired, 100);
    });
}

#[test]
fn save_attempt_cap_keeps_metadata_and_counted_failures_plateau() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let baseline = ready.budget.usage().reserved;
        let mut admission = controller(25, CancelState::new());
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let mut plateau = None;
        for k in 0..25 {
            let mut live = admission.admit().unwrap();
            let runs = records(&mut live, &ready, &mut Injected::default()).await;
            let result = RetirementBatch::new(runs, tokio::time::Instant::now())
                .unwrap()
                .retire(
                    live.begin_retirement(IterationClass::CandidateFailure)
                        .unwrap(),
                    ready.owner.clone(),
                    &ready.budget,
                    &mut persistence,
                    &mut NoRetirementHooks,
                )
                .await
                .unwrap();
            assert_eq!(
                result.saved,
                (k < 20).then_some(SaveOutcome::NotSaved(NotSavedReason::Io))
            );
            assert_eq!(ready.budget.usage().reserved, baseline);
            assert_eq!(persistence.iteration_debug(), PersistenceDebug::default());
            if k == 19 {
                plateau = Some(ready.budget.usage().retained);
            }
            if k >= 20 {
                assert_eq!(Some(ready.budget.usage().retained), plateau);
            }
        }
        assert_eq!(admission.counters().not_saved, 25);
        assert_eq!(admission.counters().failures_dropped, 5);
        let iterations = fs::read_dir(ready.dir.join("iterations"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(iterations.len(), 20 * 5);
        for entry in iterations {
            let mut names = fs::read_dir(entry.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(
                names,
                [
                    OsString::from("cleanup.log"),
                    OsString::from("record.json"),
                    OsString::from("spec.json")
                ]
            );
        }
    });
}

#[test]
fn saved_failure_still_promotes_when_r10_removal_fails() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let mut admission = controller(2, CancelState::new());
        let mut live = admission.admit().unwrap();
        let runs = records(&mut live, &ready, &mut Injected::default()).await;
        let failed = IterationRunId {
            iteration: 0,
            role: BatchRole::Candidate,
        }
        .directory(&ready.dir)
        .join("out")
        .join("stdout");
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let mut hooks = Hooks {
            fail: Some(failed.clone()),
            ..Default::default()
        };
        let result = RetirementBatch::new(runs, tokio::time::Instant::now())
            .unwrap()
            .retire(
                live.begin_retirement(IterationClass::CandidateFailure)
                    .unwrap(),
                ready.owner.clone(),
                &ready.budget,
                &mut persistence,
                &mut hooks,
            )
            .await
            .unwrap();
        assert_eq!(result.saved, Some(SaveOutcome::Promoted));
        assert_eq!(hooks.promotions, 1);
        assert_eq!(
            result.cleanup_failed.paths,
            [failed.strip_prefix(&ready.dir).unwrap().to_path_buf()]
        );
        assert!(failed.is_file());
        assert_eq!(admission.counters().promoted, 1);
        assert_eq!(
            admission.admit().unwrap_err().reason,
            StopReason::CleanupFailed
        );
        all_published(&result, &ready);
    });
}
