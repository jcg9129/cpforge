//! Portable full-engine fixtures: no child process or platform executor.
use super::*;
use crate::jobs::engine::stress::*;
use crate::jobs::persist::JobPersistence;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, Default)]
struct Clock(Arc<Mutex<Duration>>);
impl StressClock for Clock {
    fn now(&self) -> Duration {
        *self.0.lock().unwrap()
    }
}

fn request(ready: &ReadyJob<'_>, n: u64, five: bool) -> StressRequest {
    let free = match &ready.planned_target {
        PlannedTarget::BuildFree(free) => free,
        PlannedTarget::Build(_) => panic!("build-free fixture"),
    };
    let mut stage = free.run(ready.dir.join("template").join("cwd")).unwrap();
    stage.stdout.bytes.value = 4;
    stage.stderr.bytes.value = 0;
    stage.stdout.policy = CapPolicy::Kill;
    let template = || RunTemplate::new(Box::new(stage.clone()), 4, 0).unwrap();
    let mut request = StressRequest::new(
        BatchTemplates::new(
            template(),
            five.then(template),
            Some(template()),
            template(),
            five.then(template),
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
        Comparator::Exact,
    );
    request.clock = Box::<Clock>::default();
    request
}

#[derive(Debug, Default)]
struct Script {
    calls: u64,
    clock: Clock,
    iteration_time: Duration,
    cancel: Option<(IterationRunId, CancelState)>,
    held: Vec<RunResources>,
    stall: Option<IterationRunId>,
    transfer: Option<IterationRunId>,
    fail: Option<IterationRunId>,
    child_file: Option<IterationRunId>,
    unverified: bool,
    /// The last generator argument of every generator run, in order.
    seeds: Vec<std::ffi::OsString>,
}
impl Executor for Script {
    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        _cancel: CancelToken,
        events: RunEvents,
        resources: RunResources,
    ) -> JobFuture<'_, ExecutionRecord> {
        Box::pin(async move {
            let RunIdentity::Iteration(id) = &spec.run else {
                panic!("no builds in this fixture");
            };
            let id = *id;
            self.calls += 1;
            if id.role == BatchRole::Generator {
                let (crate::exec::ResolvedCommand::Argv { args, .. }
                | crate::exec::ResolvedCommand::Shell { args, .. }) = &spec.resolved.command;
                self.seeds.extend(args.last().cloned());
            }
            if self.transfer == Some(id) {
                let (_, cancel) = self.cancel.as_ref().expect("transferred cancellation");
                cancel.request_cancel(CancelSource::Api);
                self.held.push(resources);
                return ExecutionRecord::Transferred;
            }
            let path = spec.directory.join("out").join("stdout");
            fs::write(&path, b"yes\n").unwrap();
            resources.retained_counter().fetch_add(4, Ordering::Release);
            events.output(Stream::Stdout, b"yes\n");
            let mut outcome = root(if self.fail == Some(id) { 1 } else { 0 });
            outcome.stdout.as_mut().unwrap().sink = Some(path);
            if self.unverified {
                outcome.termination.cleanup = supervisor::Cleanup::Unverified {
                    reason: supervisor::UnverifiedReason::Unsupported,
                };
            }
            if let Some((_, cancel)) = self.cancel.as_ref().filter(|(at, _)| *at == id) {
                cancel.request_cancel(CancelSource::Api);
                outcome.stop_trigger = Some(supervisor::StopTrigger::Cancelled);
            }
            if id.role == BatchRole::Candidate {
                *self.clock.0.lock().unwrap() += self.iteration_time;
            }
            if self.child_file == Some(id) {
                fs::write(spec.directory.join("cwd").join("held"), b"child").unwrap();
            }
            if self.stall == Some(id) {
                outcome.stderr = Some(supervisor::StreamCapture {
                    losses: vec![supervisor::CaptureLoss::WriteStalled],
                    stored: 0,
                    observed: 0,
                    retained: Vec::new(),
                    forwarded: 0,
                    eof: false,
                    exceeded_at: None,
                    sink: None,
                });
                self.held.push(resources);
            } else {
                drop(resources);
            }
            ExecutionRecord::Started(Box::new(outcome))
        })
    }
}

fn id(iteration: u64, role: BatchRole) -> IterationRunId {
    IterationRunId { iteration, role }
}
fn stress(report: &FinalizedReport) -> &StressReport {
    report.data().details.stress.as_deref().unwrap()
}
fn wire(report: &FinalizedReport) -> serde_json::Value {
    let bytes = report::wire::encode_report_data(report.data()).unwrap();
    assert!(bytes.len() <= STRESS_REPORT_BOUND);
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn batch_engine_runs_classifies_and_retires_one_hundred_iterations() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let budget = ready.budget.clone();
        let baseline = budget.usage();
        let req = request(&ready, 100, true);
        let engine = Engine::stress(ready, req).unwrap();
        let mut fake = Script::default();
        let report = engine
            .run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks)
            .await;
        let summary = stress(&report);
        assert_eq!(
            (
                summary.counters.admitted,
                summary.counters.retired,
                fake.calls
            ),
            (100, 100, 500)
        );
        assert_eq!(summary.counters.captured_bytes, 2000);
        // Stress §3.2: `gen <g_k>` with g_k = 19 + k, one argument per run.
        assert_eq!(
            fake.seeds,
            (19..119u64)
                .map(|seed| seed.to_string().into())
                .collect::<Vec<std::ffi::OsString>>()
        );
        assert_eq!(summary.counters.classes, [100, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(summary.stop.reason, StopReason::Iterations);
        assert_eq!(report.data().details.pre_delivery_exit.code, 0);
        assert!(report.data().executions.is_empty());
        assert_eq!(
            fs::read_dir(report.data().job.dir.join("iterations"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(budget.usage(), baseline);
        assert_eq!(wire(&report)["stress"]["iterations"]["retired"], "100");
        drop(report);
        released_budget(&budget).await;
    });
}

#[test]
fn st_l02_engine_intent_before_iteration_seven() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 100, true);
        let engine = Engine::stress(ready, req).unwrap();
        let cancel = engine.cancellation();
        let engine = engine.observe(move |point| {
            if point == Point::StressIterationAdmit(7) {
                cancel.request_cancel(CancelSource::Api);
            }
        });
        let report = engine
            .run(
                &mut Script::default(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        assert_eq!(stress(&report).counters.retired, 7);
        assert_eq!(
            stress(&report).stop,
            Stop {
                reason: StopReason::Cancelled,
                iteration: Some(7)
            }
        );
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        assert_eq!(report.data().details.pre_delivery_exit.code, 6);
        assert!(
            !id(7, BatchRole::Generator)
                .directory(&report.data().job.dir)
                .exists()
        );
    });
}

#[test]
fn st_l04_engine_cancels_running_candidate_and_keeps_evidence() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 100, true);
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let budget = ready.budget.clone();
        let engine = Engine::stress(ready, req).unwrap();
        let mut fake = Script {
            cancel: Some((id(7, BatchRole::Candidate), engine.cancellation())),
            ..Default::default()
        };
        let report = engine.run(&mut fake, &mut persistence, &mut NoHooks).await;
        assert_eq!(stress(&report).counters.retired, 8);
        assert_eq!(stress(&report).counters.classes[7], 1);
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        assert_eq!(report.data().details.pre_delivery_exit.code, 6);
        for role in BatchRole::ALL.into_iter().take(4) {
            let dir = id(7, role).directory(&report.data().job.dir);
            for name in ["spec.json", "record.json", "cleanup.log"] {
                assert!(dir.join(name).is_file());
            }
            let record: serde_json::Value =
                serde_json::from_slice(&fs::read(dir.join("record.json")).unwrap()).unwrap();
            assert!(record.is_object());
        }
        assert!(
            !id(7, BatchRole::Checker)
                .directory(&report.data().job.dir)
                .exists()
        );
        assert!(
            !id(8, BatchRole::Generator)
                .directory(&report.data().job.dir)
                .exists()
        );
        drop((report, persistence, fake));
        released_budget(&budget).await;
    });
}

#[derive(Debug)]
struct RemoveFailure(PathBuf);
impl RetirementHooks for RemoveFailure {
    fn remove(
        &mut self,
        root: &Path,
        path: &Path,
    ) -> Result<crate::storage::WalkReport, crate::storage::StorageError> {
        if self.0.starts_with(path) {
            return Ok(crate::storage::WalkReport {
                errors: vec![(
                    self.0.clone(),
                    std::io::Error::other("injected removal failure"),
                )],
                ..Default::default()
            });
        }
        crate::storage::remove_owned_tree(root, path)
    }
}
#[test]
fn st_l09_engine_cleanup_failure_verdict_and_later_pruning() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 100, false);
        let budget = ready.budget.clone();
        let baseline = budget.usage().retained;
        let directory = ready.dir.clone();
        let failed = id(5, BatchRole::Candidate)
            .directory(&directory)
            .join("cwd")
            .join("held");
        let mut hooks = RemoveFailure(failed.clone());
        let engine = Engine::stress(ready, req).unwrap();
        let mut fake = Script {
            child_file: Some(id(5, BatchRole::Candidate)),
            ..Default::default()
        };
        let report = engine
            .run_stress(&mut fake, &mut MemoryPersistence::default(), &mut hooks)
            .await;
        assert_eq!(
            (
                stress(&report).counters.admitted,
                stress(&report).counters.retired
            ),
            (6, 6)
        );
        assert_eq!(stress(&report).stop.reason, StopReason::CleanupFailed);
        assert_eq!(
            report.data().result.verdict,
            JobVerdict::StressCleanupFailed
        );
        assert_eq!(report.data().details.pre_delivery_exit.code, 10);
        assert_eq!(
            stress(&report).cleanup_failed.paths,
            [failed.strip_prefix(&directory).unwrap().to_path_buf()]
        );
        assert_eq!(budget.usage().retained, baseline + 4);
        assert!(failed.is_file());
        assert!(!id(6, BatchRole::Generator).directory(&directory).exists());
        wire(&report);
        drop(report);
        released_budget(&budget).await;
        // `cpf clean --apply`: the inactive job is pruned with its leftovers.
        ready_support::released(&directory);
        f.prune();
        assert!(!directory.exists());
    });
}

#[test]
fn st_l10_engine_stalled_sink_reaches_terminal_and_retains_leftovers() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 100, false);
        let budget = ready.budget.clone();
        let engine = Engine::stress(ready, req).unwrap();
        let mut receiver = engine.events().subscribe().unwrap();
        let mut fake = Script {
            stall: Some(id(5, BatchRole::Candidate)),
            ..Default::default()
        };
        let report = engine
            .run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks)
            .await;
        while receiver.recv().await.is_some() {}
        let terminal = receiver.terminal().await;
        assert_eq!(terminal.verdict, JobVerdict::StressCleanupFailed);
        assert_eq!(stress(&report).counters.classes[0], 6);
        assert_eq!(stress(&report).counters.retired, 6);
        assert_eq!(stress(&report).stop.reason, StopReason::CleanupFailed);
        assert_eq!(
            stress(&report).cleanup_failed.paths,
            [PathBuf::from("iterations").join("5.candidate")]
        );
        assert!(
            report
                .data()
                .job
                .dir
                .join("iterations")
                .join("5.candidate")
                .is_dir()
        );
        assert_eq!(fake.held.len(), 1);
        drop((terminal, receiver, report, fake));
        released_budget(&budget).await;
    });
}

#[test]
fn st_l13_engine_counter_headroom_refuses_before_first_run() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let mut req = request(&ready, 100, true);
        req.counters.runs_started = u64::MAX - 3;
        let engine = Engine::stress(ready, req).unwrap();
        let mut fake = Script::default();
        let report = engine
            .run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks)
            .await;
        assert_eq!(fake.calls, 0);
        assert_eq!(stress(&report).counters.retired, 0);
        assert_eq!(stress(&report).stop.reason, StopReason::CounterLimit);
        assert_eq!(report.data().result.verdict, JobVerdict::NoIterations);
        assert_eq!(report.data().details.pre_delivery_exit.code, 4);
        wire(&report);
    });
}

#[test]
fn st_l14_engine_clock_admits_at_zero_three_six_and_nine_seconds() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let mut req = request(&ready, 100, false);
        let clock = Clock::default();
        req.clock = Box::new(clock.clone());
        req.schedule = Schedule::Search {
            start_seed: 19,
            budget: SearchBudget {
                iterations: 100,
                time: Duration::from_secs(10),
            },
            keep_going: true,
        };
        let engine = Engine::stress(ready, req).unwrap();
        let mut fake = Script {
            clock: clock.clone(),
            iteration_time: Duration::from_secs(3),
            ..Default::default()
        };
        let report = engine
            .run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks)
            .await;
        assert_eq!(stress(&report).counters.retired, 4);
        assert_eq!(stress(&report).stop.reason, StopReason::TimeBudget);
        assert_eq!(clock.now(), Duration::from_secs(12));
        assert_eq!(report.data().details.pre_delivery_exit.code, 0);
    });
}

#[test]
fn st_l15_engine_iteration_budgets_one_and_three() {
    run_test(move || async move {
        for n in [1, 3] {
            let f = Fixture::new();
            let ready = ready(&f, "python", 0, &Tools);
            let req = request(&ready, n, true);
            let engine = Engine::stress(ready, req).unwrap();
            let report = engine
                .run(
                    &mut Script::default(),
                    &mut MemoryPersistence::default(),
                    &mut NoHooks,
                )
                .await;
            assert_eq!(stress(&report).counters.retired, n);
            assert_eq!(stress(&report).counters.runs_started, n * 5);
            assert_eq!(stress(&report).stop.reason, StopReason::Iterations);
            assert_eq!(
                wire(&report)["stress"]["iterations"]["admitted"],
                n.to_string()
            );
        }
    });
}

#[test]
fn st_l16_engine_events_use_iteration_ids_beyond_ten_thousand() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let mut req = request(&ready, 10_005, false);
        req.clock = Box::<Clock>::default();
        let engine = Engine::stress(ready, req).unwrap().output_events(true);
        let mut receiver = engine.events().subscribe().unwrap();
        let mut fake = Script::default();
        let mut persistence = MemoryPersistence::default();
        let mut hooks = NoHooks;
        let drain = async {
            let mut previous = None;
            let mut found = false;
            while let Some(event) = receiver.recv().await {
                if let Some(seq) = previous {
                    assert!(event.seq > seq);
                }
                previous = Some(event.seq);
                // Every envelope encodes within its bound; only the sampled
                // one is parsed, keeping 10 005 iterations fast on every OS.
                let bytes = report::wire::encode_event(&event).unwrap();
                match event.payload {
                    EventPayload::IterationRunStarted { run, .. }
                        if run == id(10_004, BatchRole::Candidate) =>
                    {
                        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(value["run"], "10004.candidate");
                        assert!(value.get("execution").is_none());
                        found = true;
                    }
                    EventPayload::RunStarted { .. }
                    | EventPayload::RunFinished { .. }
                    | EventPayload::Progress { .. } => panic!("build-only event without a build"),
                    EventPayload::OutputChunk { run, .. } => {
                        assert!(matches!(run, RunIdentity::Iteration(_)))
                    }
                    _ => {}
                }
            }
            assert!(found);
            receiver.terminal().await
        };
        let (report, terminal) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), drain);
        assert_eq!(stress(&report).counters.retired, 10_005);
        assert!(report.data().executions.is_empty());
        assert_eq!(terminal.verdict, JobVerdict::Passed);
        wire(&report);
    });
}

#[test]
fn st_l17_engine_retention_refusal_keeps_evidence_and_closes_admission() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 100, false);
        let budget = ready.budget.clone();
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let hold = Arc::new(Mutex::new(None));
        let held = hold.clone();
        let accounting = budget.clone();
        let engine = Engine::stress(ready, req).unwrap().observe(move |point| {
            if point == Point::StressRunAdmit(id(3, BatchRole::Brute)) {
                let usage = accounting.usage();
                *held.lock().unwrap() = Some(
                    accounting
                        .reserve(accounting.limit() - usage.retained - usage.reserved)
                        .unwrap(),
                );
            }
        });
        let report = engine
            .run(&mut Script::default(), &mut persistence, &mut NoHooks)
            .await;
        assert_eq!(stress(&report).counters.retired, 4);
        assert_eq!(stress(&report).stop.reason, StopReason::AdmissionClosed);
        assert_eq!(stress(&report).counters.classes[6], 1);
        assert_eq!(report.data().result.verdict, JobVerdict::Infrastructure);
        assert_eq!(report.data().details.pre_delivery_exit.code, 10);
        assert!(
            id(3, BatchRole::Generator)
                .directory(&report.data().job.dir)
                .is_dir()
        );
        assert!(
            !id(3, BatchRole::Candidate)
                .directory(&report.data().job.dir)
                .exists()
        );
        assert_eq!(
            stress(&report).classes[6].as_ref().unwrap().diagnostics[0]
                .decision
                .reason,
            "retention_budget"
        );
        drop((report, persistence));
        drop(hold.lock().unwrap().take());
        released_budget(&budget).await;
    });
}

#[test]
fn late_intent_keeps_completed_verdict_but_obeys_cancellation_exit_precedence() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 1, false);
        let engine = Engine::stress(ready, req).unwrap();
        let cancel = engine.cancellation();
        let engine = engine.observe(move |point| {
            if point == Point::AfterPrimaryFreeze {
                cancel.request_cancel(CancelSource::Api);
            }
        });
        let report = engine
            .run(
                &mut Script::default(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        assert!(report.data().result.cancellation.is_some());
        assert_eq!(report.data().result.verdict, JobVerdict::Passed);
        assert_eq!(report.data().details.pre_delivery_exit.code, 6);
    });
}

#[test]
fn st_l19_batch_transferred_checker_snapshot_precedes_cleanup_verdict() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 1, true);
        let budget = ready.budget.clone();
        let engine = Engine::stress(ready, req).unwrap();
        let mut fake = Script {
            transfer: Some(id(0, BatchRole::Checker)),
            cancel: Some((id(0, BatchRole::Checker), engine.cancellation())),
            ..Default::default()
        };
        let report = engine
            .run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks)
            .await;
        assert_eq!(stress(&report).counters.retired, 1);
        assert_eq!(stress(&report).counters.classes[7], 1);
        assert_eq!(
            stress(&report).cleanup_failed.paths,
            [PathBuf::from("iterations").join("0.checker")]
        );
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        assert_eq!(report.data().details.pre_delivery_exit.code, 6);
        drop((report, fake));
        released_budget(&budget).await;
    });
}
