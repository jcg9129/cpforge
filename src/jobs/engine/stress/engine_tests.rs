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
    request_captures(ready, n, five, 4, 0)
}

fn request_captures(
    ready: &ReadyJob<'_>,
    n: u64,
    five: bool,
    stdout: u64,
    stderr: u64,
) -> StressRequest {
    let free = match &ready.planned_target {
        PlannedTarget::BuildFree(free) => free,
        PlannedTarget::Build(_) => panic!("build-free fixture"),
    };
    let mut stage = free.run(ready.dir.join("template").join("cwd")).unwrap();
    stage.stdout.bytes.value = stdout;
    stage.stderr.bytes.value = stderr;
    stage.stdout.policy = CapPolicy::Kill;
    let template = || RunTemplate::new(Box::new(stage.clone()), stdout, stderr).unwrap();
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

/// ST-L01: the scheduled iteration follows 100 passing batch iterations.
const HOLD_ITERATION: u64 = 100;

/// A held stderr sink and its run's slot and credits (behind barrier H).
type Holder = (
    fs::File,
    RunResources,
    supervisor::Admission,
    supervisor::Credits,
);

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
    record_seeds: bool,
    capture_sizes: Option<(usize, usize)>,
    slots: bool,
    holders: Vec<Holder>,
    hold_schedule: bool,
    max_executing: usize,
    executing: usize,
    occupancy: usize,
    released_at: Option<tokio::time::Instant>,
    release_gate: Option<Arc<tokio::sync::Notify>>,
    writer: Option<tokio::task::JoinHandle<()>>,
    fail_checkers: bool,
    failing_iterations: Option<[u64; 3]>,
    warning_at: Option<u64>,
    warn_every_generator: bool,
    tail_spec_bytes: usize,
    measure_tail: bool,
    resume_events: Option<Arc<tokio::sync::Notify>>,
}
impl Executor for Script {
    fn inline_judge(&self) -> bool {
        true
    }
    fn cwd_overflow(&self, run: &RunIdentity) -> Option<u64> {
        let RunIdentity::Iteration(run) = run else {
            return None;
        };
        ((self.warning_at == Some(run.iteration) && run.role == BatchRole::Candidate)
            || (self.warn_every_generator && run.role == BatchRole::Generator))
            .then_some(4097)
    }
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
            if self.warning_at == Some(id.iteration) && id.role == BatchRole::Generator {
                if let Some(resume) = &self.resume_events {
                    resume.notified().await;
                }
            }
            self.calls += 1;
            if self.measure_tail && id.iteration == 2 {
                self.tail_spec_bytes += report::wire::encode_spec(&spec.wire_spec()).unwrap().len();
            }
            if self.record_seeds && id.role == BatchRole::Generator {
                let (crate::exec::ResolvedCommand::Argv { args, .. }
                | crate::exec::ResolvedCommand::Shell { args, .. }) = &spec.resolved.command;
                self.seeds.extend(args.last().cloned());
            }
            let slot = self.slots.then(|| supervisor::admit().unwrap());
            let credit = self.slots.then(|| {
                supervisor::global_bounds()
                    .unwrap()
                    .thread_credits
                    .try_reserve(2)
                    .unwrap()
            });
            self.executing += 1;
            self.max_executing = self.max_executing.max(self.executing);
            if self.hold_schedule
                && id.iteration == HOLD_ITERATION
                && id.role == BatchRole::Candidate
            {
                self.occupancy = supervisor::debug_counters().slots_in_use;
                assert_eq!(self.occupancy, 4); // H is still closed here.
                assert_eq!(self.holders.len(), 3);
                self.holders.clear(); // Open H only after sampling the fourth slot.
            }
            if self.transfer == Some(id) {
                let (_, cancel) = self.cancel.as_ref().expect("transferred cancellation");
                let gate = self.release_gate.clone().expect("blocked creation gate");
                let (entered, started) = tokio::sync::oneshot::channel();
                self.writer = Some(tokio::spawn(async move {
                    let _held = (resources, slot, credit);
                    entered.send(()).unwrap();
                    gate.notified().await;
                }));
                started.await.unwrap();
                cancel.request_cancel(CancelSource::Api);
                self.executing -= 1;
                return ExecutionRecord::Transferred;
            }
            let mut outcome = root(
                if self.fail == Some(id)
                    || ((self.fail_checkers
                        || self
                            .failing_iterations
                            .is_some_and(|ks| ks.contains(&id.iteration)))
                        && id.role == BatchRole::Checker)
                {
                    1
                } else {
                    0
                },
            );
            let (stdout, stderr) = self.capture_sizes.unwrap_or((4, 0));
            // Fixed-size buffers, real file sinks, no history of captures.
            let bytes = [b'x'; 4096];
            for (name, size, stream) in [
                ("stdout", stdout, Stream::Stdout),
                ("stderr", stderr, Stream::Stderr),
            ] {
                let data = if self.capture_sizes.is_some() {
                    &bytes[..size]
                } else {
                    b"yes\n".as_slice()
                };
                if name == "stderr" && self.capture_sizes.is_none() {
                    continue;
                }
                let sink = spec.directory.join("out").join(name);
                fs::write(&sink, data).unwrap();
                resources
                    .retained_counter()
                    .fetch_add(data.len() as u64, Ordering::Release);
                events.output(stream, data);
                let capture = supervisor::StreamCapture {
                    observed: data.len() as u64,
                    retained: Vec::new(),
                    stored: data.len() as u64,
                    forwarded: 0,
                    eof: true,
                    exceeded_at: None,
                    losses: Vec::new(),
                    sink: Some(sink),
                };
                if name == "stdout" {
                    outcome.stdout = Some(capture);
                } else {
                    outcome.stderr = Some(capture);
                }
            }
            if self.unverified && id.role == BatchRole::Candidate {
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
            if self.warning_at == Some(id.iteration) && id.role == BatchRole::Checker {
                let input = spec.directory.join("cwd").join("input");
                fs::remove_file(&input).unwrap();
                fs::create_dir(input).unwrap(); // controlled staged-copy removal failure
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
                    sink: Some(spec.directory.join("out").join("stderr")),
                });
                if let Some(gate) = self.release_gate.clone() {
                    let mut sink =
                        fs::File::create(spec.directory.join("out").join("stderr")).unwrap();
                    self.writer = Some(tokio::spawn(async move {
                        let _held = (slot, credit);
                        gate.notified().await;
                        std::io::Write::write_all(&mut sink, b"late").unwrap();
                        drop(sink);
                        resources.retained_counter().fetch_add(4, Ordering::Release);
                        drop(resources);
                    }));
                } else {
                    self.held.push(resources);
                }
            } else if self.hold_schedule
                && id.iteration == HOLD_ITERATION
                && role_index(id.role) < 3
            {
                self.holders.push((
                    fs::File::create(spec.directory.join("out").join("stderr")).unwrap(),
                    resources,
                    slot.unwrap(),
                    credit.unwrap(),
                ));
            } else {
                drop(resources);
                drop((slot, credit));
            }
            if self.hold_schedule && id.iteration == HOLD_ITERATION {
                let target = Duration::from_secs(role_index(id.role).min(3) as u64);
                let elapsed = target - self.clock.now();
                tokio::time::advance(elapsed).await;
                *self.clock.0.lock().unwrap() = target;
                self.released_at = Some(tokio::time::Instant::now());
            }
            self.executing -= 1;
            ExecutionRecord::Started(Box::new(outcome))
        })
    }
}

fn role_index(role: BatchRole) -> usize {
    BatchRole::ALL.iter().position(|r| *r == role).unwrap()
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
    if !isolated(
        "batch_engine_runs_classifies_and_retires_one_hundred_iterations",
        false,
    ) {
        return;
    }
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let budget = ready.budget.clone();
        let baseline = budget.usage();
        supervisor::init_process().unwrap();
        let req = request(&ready, 100, true);
        let directory = ready.dir.clone();
        let retired = Arc::new(Mutex::new(0));
        let observed = retired.clone();
        let engine = Engine::stress(ready, req)
            .unwrap()
            .observe(move |point| match point {
                Point::StressIterationAdmit(k) => {
                    assert_eq!(*observed.lock().unwrap(), k);
                    assert_eq!(supervisor::debug_counters().slots_in_use, 0);
                }
                Point::StressRunAdmit(run) => {
                    assert_eq!(*observed.lock().unwrap(), run.iteration);
                    assert!(
                        !id(run.iteration + 1, BatchRole::Generator)
                            .directory(&directory)
                            .exists()
                    );
                }
                Point::StressRetired { iteration, debug } => {
                    assert_eq!(debug, RetirementDebug::default());
                    *observed.lock().unwrap() = iteration + 1;
                }
                _ => {}
            });
        let mut fake = Script {
            slots: true,
            record_seeds: true,
            ..Default::default()
        };
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
        assert!(fake.max_executing <= 2);
        assert_eq!(fake.executing, 0);
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
    if !isolated(
        "st_l10_engine_stalled_sink_reaches_terminal_and_retains_leftovers",
        false,
    ) {
        return;
    }
    run_test(move || async move {
        deadline_case(false).await;
    });
}

/// Real-time bound of every busy wait in these fixtures: a paused Tokio clock
/// never advances while a task stays runnable, so its timeouts cannot fire.
const REAL_TIME_LIMIT: Duration = Duration::from_secs(60);

async fn yield_until(what: &str, mut ready: impl FnMut() -> bool) {
    let limit = std::time::Instant::now() + REAL_TIME_LIMIT;
    while !ready() {
        assert!(
            std::time::Instant::now() < limit,
            "timed out waiting for {what}"
        );
        tokio::task::yield_now().await;
    }
}

// The injected writer/creation owns resources independently of its immutable
// snapshot. Both gates stay closed through R1 and R6, and open at t = 8 s.
async fn deadline_case(transfer: bool) {
    tokio::time::pause();
    supervisor::init_process().unwrap();
    let f = Fixture::new();
    let ready = ready(&f, "python", 0, &Tools);
    let req = request_captures(&ready, 100, transfer, 4, 4);
    let budget = ready.budget.clone();
    let r6 = Arc::new(Mutex::new(None));
    let observed = r6.clone();
    let engine = Engine::stress(ready, req).unwrap().observe(move |point| {
        if matches!(point, Point::StressRetire { iteration, step: RetirementStep::ResourcesReleased }
            if iteration == if transfer { 0 } else { 5 }) {
            *observed.lock().unwrap() = Some(tokio::time::Instant::now());
        }
    });
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut receiver = engine.events().subscribe().unwrap();
    let mut fake = Script {
        slots: true,
        transfer: transfer.then_some(id(0, BatchRole::Checker)),
        cancel: transfer.then(|| (id(0, BatchRole::Checker), engine.cancellation())),
        stall: (!transfer).then_some(id(5, BatchRole::Candidate)),
        release_gate: Some(gate.clone()),
        ..Default::default()
    };
    let finished = Arc::new(Mutex::new(false));
    let completed = finished.clone();
    let mut persistence = MemoryPersistence::default();
    let mut hooks = NoHooks;
    let run = async {
        let report = engine.run(&mut fake, &mut persistence, &mut hooks).await;
        *completed.lock().unwrap() = true;
        report
    };
    let driver = async {
        // Yielding keeps the paused clock still: R6's deadline cannot pass
        // before the explicit advances below.
        yield_until("the R6 step of the stalled iteration", || {
            r6.lock().unwrap().is_some()
        })
        .await;
        let begin = r6.lock().unwrap().unwrap();
        // R1 accepted the snapshot while the physical operation is blocked.
        assert_eq!(supervisor::debug_counters().slots_in_use, 1);
        assert!(!*finished.lock().unwrap());
        tokio::time::advance(Duration::from_millis(4999)).await;
        tokio::task::yield_now().await;
        assert!(!*finished.lock().unwrap());
        tokio::time::advance(Duration::from_millis(1)).await;
        // Tokio rounds a timer up to its next millisecond tick, and the clock
        // was paused at a fractional offset: exactly 5 s may still be short of
        // R6's tick. Sleeping lets the idle runtime auto-advance to it, which
        // busy yielding would prevent forever.
        let limit = std::time::Instant::now() + REAL_TIME_LIMIT;
        while !*finished.lock().unwrap() {
            assert!(
                std::time::Instant::now() < limit,
                "the job did not finish after R6's deadline"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let waited = begin.elapsed();
        assert!(waited >= Duration::from_secs(5));
        assert!(waited <= Duration::from_millis(5100));
    };
    let (report, ()) = tokio::join!(run, driver);
    let mut stalled = false;
    let mut snapshot = false;
    while let Some(event) = receiver.recv().await {
        if let EventPayload::IterationRunFinished { run, summary } = event.payload {
            if transfer && run == id(0, BatchRole::Checker) {
                assert!(matches!(
                    summary.record.as_ref(),
                    ExecutionRecord::Transferred
                ));
                snapshot = true;
            }
            if run == id(5, BatchRole::Candidate) && !transfer {
                let ExecutionRecord::Started(root) = summary.record.as_ref() else {
                    panic!("started writer");
                };
                assert_eq!(
                    root.stderr.as_ref().unwrap().losses,
                    [supervisor::CaptureLoss::WriteStalled]
                );
                stalled = true;
            }
        }
    }
    let terminal = receiver.terminal().await;
    assert_eq!(supervisor::debug_counters().slots_in_use, 1);
    assert_eq!(
        stress(&report).stop.reason,
        if transfer {
            StopReason::Cancelled
        } else {
            StopReason::CleanupFailed
        }
    );
    assert_eq!(
        stress(&report).counters.runs_started,
        if transfer { 4 } else { 18 }
    );
    assert_eq!(stress(&report).counters.saved, 0);
    assert_eq!(stress(&report).counters.not_saved, 0);
    assert_eq!(stalled, !transfer);
    assert_eq!(snapshot, transfer);
    assert_eq!(
        report.data().result.verdict,
        if transfer {
            JobVerdict::Cancelled
        } else {
            JobVerdict::StressCleanupFailed
        }
    );
    assert_eq!(terminal.verdict, report.data().result.verdict);
    assert_eq!(
        stress(&report).counters.retired,
        if transfer { 1 } else { 6 }
    );
    assert_eq!(
        stress(&report).counters.classes[if transfer { 7 } else { 0 }],
        if transfer { 1 } else { 6 }
    );
    assert_eq!(
        report.data().details.pre_delivery_exit.code,
        if transfer { 6 } else { 10 }
    );
    let leftover =
        PathBuf::from("iterations").join(if transfer { "0.checker" } else { "5.candidate" });
    assert_eq!(
        stress(&report).cleanup_failed.paths,
        std::slice::from_ref(&leftover)
    );
    assert!(report.data().job.dir.join(&leftover).is_dir());
    assert!(!fake.writer.as_ref().unwrap().is_finished());
    let release_at = r6.lock().unwrap().unwrap() + Duration::from_secs(8);
    tokio::time::advance(release_at.saturating_duration_since(tokio::time::Instant::now())).await;
    assert_eq!(
        r6.lock().unwrap().unwrap().elapsed(),
        Duration::from_secs(8)
    );
    gate.notify_one();
    fake.writer.take().unwrap().await.unwrap();
    assert_eq!(supervisor::debug_counters().slots_in_use, 0);
    if !transfer {
        assert_eq!(
            fs::read(
                report
                    .data()
                    .job
                    .dir
                    .join(leftover)
                    .join("out")
                    .join("stderr")
            )
            .unwrap(),
            b"late"
        );
    }
    drop((terminal, receiver, report, fake));
    released_budget(&budget).await;
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
#[ignore = "full engine event lifecycle; dedicated stress CI step"]
fn st_l16_engine_events_use_iteration_ids_beyond_ten_thousand() {
    if !isolated(
        "st_l16_engine_events_use_iteration_ids_beyond_ten_thousand",
        true,
    ) {
        return;
    }
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let mut req = request(&ready, 10_005, false);
        req.clock = Box::<Clock>::default();
        // Identities only: `=output` chunks are R-S09's, not this row's.
        let engine = Engine::stress(ready, req).unwrap();
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
                // Retain the bound check for every envelope; the dedicated
                // CI step allows the full consumer to run beyond 120 seconds.
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
                    EventPayload::OutputChunk { .. } => panic!("output events are off"),
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
    if !isolated(
        "st_l19_batch_transferred_checker_snapshot_precedes_cleanup_verdict",
        false,
    ) {
        return;
    }
    run_test(move || async move {
        deadline_case(true).await;
    });
}

// Process isolation makes whole-process OS samples exact, including libtest's
// own threads. Each child runs one fixture and starts no candidate process.
fn isolated(name: &str, slow: bool) -> bool {
    if std::env::var("CPF_STRESS_ISOLATED").as_deref() == Ok(name) {
        return true;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("jobs::engine::tests::stress_engine::{name}"),
            "--include-ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("CPF_STRESS_ISOLATED", name);
    if slow {
        command.env("CPF_STRESS_LONG_TEST", "1");
    }
    // Bounded by real time beyond the child's own watchdog (run_test).
    let limit = std::time::Instant::now()
        + if slow {
            Duration::from_secs(21600 + 120)
        } else {
            Duration::from_secs(300)
        };
    let mut child = command.spawn().unwrap();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated fixture {name} did not finish in real time");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "isolated fixture {name}");
    false
}

#[test]
fn st_l01_batch_hold_schedule_samples_four_slots_before_opening_h() {
    if !isolated(
        "st_l01_batch_hold_schedule_samples_four_slots_before_opening_h",
        false,
    ) {
        return;
    }
    run_test(move || async move {
        tokio::time::pause();
        supervisor::init_process().unwrap();
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, HOLD_ITERATION + 2, true);
        let directory = ready.dir.clone();
        let retired = Arc::new(Mutex::new(0));
        let observed = retired.clone();
        let clock = Clock::default();
        let schedule_clock = clock.clone();
        let mut req = req;
        req.clock = Box::new(clock.clone());
        let engine = Engine::stress(ready, req)
            .unwrap()
            .observe(move |point| match point {
                Point::StressIterationAdmit(k) => {
                    assert_eq!(*observed.lock().unwrap(), k);
                    assert_eq!(supervisor::debug_counters().slots_in_use, 0);
                }
                Point::StressRunAdmit(run) => {
                    assert_eq!(*observed.lock().unwrap(), run.iteration);
                    assert!(
                        !id(run.iteration + 1, BatchRole::Generator)
                            .directory(&directory)
                            .exists()
                    );
                    if run.iteration == HOLD_ITERATION {
                        assert_eq!(
                            schedule_clock.now(),
                            Duration::from_secs(
                                role_index(run.role).saturating_sub(1).min(3) as u64
                            )
                        );
                    }
                }
                Point::StressRetired { iteration, debug } => {
                    assert_eq!(debug, RetirementDebug::default());
                    assert_eq!(supervisor::debug_counters().slots_in_use, 0);
                    *observed.lock().unwrap() = iteration + 1;
                }
                _ => {}
            });
        let mut fake = Script {
            slots: true,
            hold_schedule: true,
            clock,
            ..Default::default()
        };
        let report = engine
            .run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks)
            .await;
        assert_eq!(fake.occupancy, 4);
        assert!(fake.max_executing <= 2);
        assert_eq!(fake.executing, 0);
        assert!(fake.released_at.unwrap().elapsed() < Duration::from_secs(5));
        assert_eq!(stress(&report).counters.retired, HOLD_ITERATION + 2);
        assert_eq!(
            stress(&report).counters.runs_started,
            5 * (HOLD_ITERATION + 2)
        );
        assert_eq!(stress(&report).stop.reason, StopReason::Iterations);
        assert_eq!(report.data().details.pre_delivery_exit.code, 0);
        assert!(stress(&report).cleanup_failed.paths.is_empty());
    });
}

fn tree_size(path: &Path) -> (u64, u64) {
    let mut files = 0;
    let mut bytes = 0;
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.file_type().unwrap();
        assert!(!meta.is_symlink());
        if meta.is_dir() {
            let (n, size) = tree_size(&entry.path());
            files += n;
            bytes += size;
        } else {
            files += 1;
            bytes += entry.metadata().unwrap().len();
        }
    }
    (files, bytes)
}

#[derive(Clone, Debug)]
struct PlateauSample {
    files: (u64, u64),
    retained: u64,
    process: supervisor::ProcessSample,
    /// Every thread of the first sample, printed if a later count differs.
    threads: String,
}

async fn plateau(iterations: u64, stdout: usize, stderr: usize) {
    supervisor::init_process().unwrap();
    let f = Fixture::new();
    let ready = ready(&f, "python", 0, &Tools);
    let req = request_captures(&ready, iterations, true, stdout as u64, stderr as u64);
    let budget = ready.budget.clone();
    let directory = ready.dir.clone();
    // The injected creation worker and the real idle persistence worker hold
    // the job's two credits; cleanup has its own process-lifetime credit.
    let creation = supervisor::global_bounds()
        .unwrap()
        .thread_credits
        .try_reserve(1)
        .unwrap();
    let mut persistence = JobPersistence::new(&ready).unwrap();
    let samples = Arc::new(Mutex::new(Vec::<PlateauSample>::with_capacity(10)));
    let observed = samples.clone();
    let accounting = budget.clone();
    let engine = Engine::stress(ready, req).unwrap().observe(move |point| {
        if let Point::StressRetire {
            iteration,
            step: RetirementStep::Remove,
        } = point
        {
            if (iteration + 1) % (iterations / 10) == 0 {
                for role in BatchRole::ALL {
                    let run = id(iteration, role).directory(&directory).join("out");
                    assert_eq!(fs::read(run.join("stdout")).unwrap(), vec![b'x'; stdout]);
                    assert_eq!(fs::read(run.join("stderr")).unwrap(), vec![b'x'; stderr]);
                }
            }
        }
        if let Point::StressRetired { iteration, debug } = point {
            assert_eq!(debug, RetirementDebug::default());
            let resources = supervisor::debug_counters();
            assert_eq!(resources.slots_in_use, 0);
            assert_eq!(resources.credits_in_use, 3); // job 2 + cleanup 1
            assert_eq!(resources.service_pending, 0);
            assert_eq!(resources.transferred_workers, 0);
            if (iteration + 1) % (iterations / 10) == 0 {
                let files = tree_size(&directory);
                let usage = accounting.usage();
                assert_eq!(usage.reserved, 2 * STRESS_REPORT_BOUND as u64);
                let process = supervisor::process_sample();
                // A failed sample must not poison the later ones' report.
                let mut samples = observed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(first) = samples.first() {
                    assert_eq!(files, first.files);
                    assert_eq!(usage.retained, first.retained);
                    assert_eq!(
                        process.threads,
                        first.process.threads,
                        "threads after iteration {iteration}:\n{}\nfirst sample:\n{}",
                        supervisor::thread_report(),
                        first.threads
                    );
                    assert_eq!(process.descriptors, first.process.descriptors);
                    assert!(process.resident <= first.process.resident + (16 << 20));
                }
                let threads = if samples.is_empty() {
                    supervisor::thread_report()
                } else {
                    String::new()
                };
                samples.push(PlateauSample {
                    files,
                    retained: usage.retained,
                    process,
                    threads,
                });
            }
        }
    });
    let mut fake = Script {
        capture_sizes: Some((stdout, stderr)),
        slots: true,
        ..Default::default()
    };
    let started = Instant::now();
    let report = engine.run(&mut fake, &mut persistence, &mut NoHooks).await;
    eprintln!("{iterations} iterations: {:?}", started.elapsed());
    assert_eq!(
        samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        10
    );
    let summary = stress(&report);
    assert_eq!(summary.counters.admitted, iterations);
    assert_eq!(summary.counters.retired, iterations);
    assert_eq!(summary.counters.runs_started, iterations * 5);
    assert_eq!(fake.calls, iterations * 5);
    assert_eq!(
        summary.counters.captured_bytes,
        iterations * 5 * (stdout + stderr) as u64
    );
    assert_eq!(summary.counters.classes, [iterations, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(summary.counters.saved, 0);
    assert_eq!(summary.counters.not_saved, 0);
    assert!(summary.failures.is_empty());
    assert_eq!(report.data().details.pre_delivery_exit.code, 0);
    let value = wire(&report);
    assert_eq!(
        value["stress"]["iterations"]["retired"],
        iterations.to_string()
    );
    assert_eq!(value["stress"]["classes"].as_array().unwrap().len(), 1);
    assert_eq!(
        value["stress"]["classes"][0]["count"],
        iterations.to_string()
    );
    drop((report, persistence, creation));
    released_budget(&budget).await;
}

#[test]
#[ignore = "real sinks and exact OS plateau; dedicated stress CI step"]
fn st_l06_batch_plateau_ten_thousand_iterations() {
    if !isolated("st_l06_batch_plateau_ten_thousand_iterations", true) {
        return;
    }
    run_test(move || async move {
        plateau(10_000, 4096, 1024).await;
    });
}

#[test]
#[ignore = "five million injected run lifecycles; weekly plateau workflow"]
fn st_l22_batch_plateau_one_million_iterations() {
    if !isolated("st_l22_batch_plateau_one_million_iterations", true) {
        return;
    }
    run_test(move || async move {
        plateau(1_000_000, 0, 0).await;
    });
}

#[test]
fn iteration_warnings_keep_occurrence_order_and_reserve_late_cancellation() {
    run_test(move || async move {
        for n in [5, 24, 30] {
            for late in [false, true] {
                let f = Fixture::new();
                let ready = ready(&f, "python", 0, &Tools);
                let req = request(&ready, n, true);
                let engine = Engine::stress(ready, req).unwrap();
                let cancel = engine.cancellation();
                let engine = engine.observe(move |point| {
                    if late && point == Point::AfterPrimaryFreeze {
                        cancel.request_cancel(CancelSource::Api);
                    }
                });
                let mut fake = Script {
                    warn_every_generator: true,
                    ..Default::default()
                };
                let report = engine
                    .run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks)
                    .await;
                let value = wire(&report);
                let warnings = value["warnings"].as_array().unwrap();
                let ordinary = n.min(if late { 23 } else { 24 }) as usize;
                assert_eq!(warnings.len(), ordinary + usize::from(late));
                for (k, warning) in warnings[..ordinary].iter().enumerate() {
                    assert_eq!(warning["kind"], "retention");
                    assert_eq!(warning["run"], format!("{k}.generator"));
                    assert!(serde_json::to_vec(warning).unwrap().len() <= 768);
                }
                if late {
                    assert_eq!(warnings.last().unwrap()["kind"], "late_cancellation");
                }
                assert_eq!(value["warnings_dropped"], n - ordinary as u64);
            }
        }
    });
}

#[test]
#[ignore = "full stalled event consumer and iteration warnings; dedicated stress CI step"]
fn r_s08_r_s09_stress_event_gaps_output_and_iteration_warnings() {
    if !isolated(
        "r_s08_r_s09_stress_event_gaps_output_and_iteration_warnings",
        true,
    ) {
        return;
    }
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 10_005, true);
        let release = Arc::new(tokio::sync::Notify::new());
        let signal = release.clone();
        let engine = Engine::stress(ready, req)
            .unwrap()
            .output_events(true)
            .observe(move |point| {
                if point == Point::StressIterationAdmit(10_003) {
                    signal.notify_one();
                }
            });
        let mut receiver = engine.events().subscribe().unwrap();
        let resume = Arc::new(tokio::sync::Notify::new());
        let mut fake = Script {
            resume_events: Some(resume.clone()),
            warning_at: Some(10_003),
            fail: Some(id(10_003, BatchRole::Checker)),
            ..Default::default()
        };
        let mut persistence = MemoryPersistence::default();
        let mut hooks = NoHooks;
        let drain = async {
            release.notified().await; // stall until well beyond the queue's bound
            resume.notify_one();
            let mut received = 0u64;
            let mut previous = None;
            let mut gaps = 0;
            let mut output = false;
            while let Some(event) = receiver.recv().await {
                gaps += event.seq - previous.map_or(0, |seq| seq + 1);
                previous = Some(event.seq);
                received += 1;
                let bytes = report::wire::encode_event(&event).unwrap();
                assert!(bytes.len() < 7 * 1024); // leaves room for the line feed
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert!(value.get("rid").is_none());
                if value["event"] == "output_chunk" && value["run"] == "10003.candidate" {
                    output = true;
                }
                if matches!(
                    event.payload,
                    EventPayload::IterationRunStarted { .. }
                        | EventPayload::IterationRunFinished { .. }
                ) {
                    assert!(value["run"].as_str().unwrap().contains('.'));
                }
            }
            assert!(output);
            let terminal = receiver.terminal().await;
            gaps += terminal.seq - previous.unwrap() - 1;
            assert_eq!(terminal.seq, 1 + 10_005 * 17);
            assert_eq!(terminal.seq, received + terminal.dropped);
            assert_eq!(terminal.dropped, gaps);
            assert!(gaps > 0);
            terminal
        };
        let (report, terminal) =
            tokio::join!(engine.run(&mut fake, &mut persistence, &mut hooks), drain);
        assert_eq!(terminal.verdict, JobVerdict::CandidateFailure);
        let value = wire(&report);
        let warnings = value["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 17);
        assert_eq!(warnings[0]["detail"], "cwd_overflow");
        assert_eq!(warnings[0]["run"], "10003.candidate");
        assert_eq!(warnings[1]["kind"], "judge_scratch");
        assert_eq!(warnings[1]["run"], "10003.checker");
        for (index, warning) in warnings[2..].iter().enumerate() {
            assert_eq!(warning["kind"], "persist");
            assert_eq!(
                warning["run"],
                format!("10003.{}", BatchRole::ALL[index / 3].token())
            );
            assert_eq!(
                warning["artefact"],
                ["spec", "record", "cleanup_log"][index % 3]
            );
            assert_eq!(warning["error"]["kind"], "worker_unavailable");
        }
        assert_eq!(value["warnings_dropped"], 0);
        assert!(
            warnings
                .iter()
                .all(|w| serde_json::to_vec(w).unwrap().len() <= 768)
        );
    });
}

#[derive(Debug, Default)]
struct PromoteIterations(u64);
impl RetirementHooks for PromoteIterations {
    fn promote<'a>(&'a mut self, runs: &'a [RetirementRun]) -> JobFuture<'a, SaveOutcome> {
        Box::pin(async move {
            // The publication boundary is injected here; promotion mechanics
            // have their own saved-failure fixtures. Verify the staging inputs
            // remain readable even when metadata persistence is unavailable.
            assert_eq!(runs.len(), 5);
            for run in runs {
                let capture = run.record.stdout().unwrap();
                assert_eq!(fs::read(capture.sink.as_ref().unwrap()).unwrap(), b"yes\n");
            }
            self.0 += 1;
            SaveOutcome::Promoted
        })
    }
}

#[test]
fn st_l23_engine_saved_iterations_preserve_one_persistence_tail() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 13, true);
        let directory = ready.dir.clone();
        let budget = ready.budget.clone();
        let (mut persistence, mut writes) = JobPersistence::controlled_writer(&ready);
        let clock = persistence.clock.clone();
        let enqueued = Arc::new(tokio::sync::Notify::new());
        let signal = enqueued.clone();
        let done = Arc::new(tokio::sync::Notify::new());
        let retired = done.clone();
        let tail = Arc::new(Mutex::new(None));
        let samples = tail.clone();
        let accounting = budget.clone();
        let engine = Engine::stress(ready, req).unwrap().observe(move |point| {
            if point
                == (Point::StressRetire {
                    iteration: 2,
                    step: RetirementStep::ResourcesReleased,
                })
            {
                signal.notify_one();
            }
            if let Point::StressRetired { iteration, debug } = point {
                assert_eq!(
                    (
                        debug.live_iterations,
                        debug.records,
                        debug.specs,
                        debug.metadata_reserved
                    ),
                    (0, 0, 0, 0)
                );
                let state = debug.persistence;
                if iteration >= 2 {
                    assert_eq!(
                        (
                            state.pending_cells,
                            state.queued_artifacts,
                            state.tail_artifacts
                        ),
                        (0, 0, 15)
                    );
                    assert_eq!(state.tail_reserved, 5 << 20);
                    assert!(state.tail_charges <= 2 << 20);
                    assert_eq!(accounting.usage().reserved, (2 + 5) << 20);
                    let mut first = samples.lock().unwrap();
                    if let Some(first) = *first {
                        assert_eq!(state, first);
                    } else {
                        *first = Some(state);
                    }
                }
                if iteration == 12 {
                    retired.notify_one();
                }
            }
        });
        let driver = async {
            for _ in 0..30 {
                // first two saved iterations: all fifteen each
                let write = writes.recv().await.unwrap();
                clock.advance(Duration::from_millis(4));
                write.release.send(()).unwrap();
            }
            let blocked = writes.recv().await.unwrap();
            assert_eq!(
                blocked.path,
                PathBuf::from("iterations")
                    .join("2.generator")
                    .join("spec.json")
            );
            enqueued.notified().await;
            clock.advance(Duration::from_secs(5));
            done.notified().await;
            let bytes = fs::metadata(directory.join(&blocked.path)).unwrap().len();
            blocked.release.send(()).unwrap();
            bytes
        };
        let mut fake = Script {
            fail_checkers: true,
            measure_tail: true,
            ..Default::default()
        };
        let mut hooks = PromoteIterations::default();
        let (report, late_bytes) = tokio::join!(
            engine.run_stress(&mut fake, &mut persistence, &mut hooks),
            driver
        );
        assert_eq!(hooks.0, 13);
        let summary = stress(&report);
        assert_eq!(summary.counters.retired, 13);
        assert_eq!(summary.counters.runs_started, 65);
        assert_eq!(summary.counters.saved, 13);
        assert_eq!(summary.counters.promoted, 13);
        assert_eq!(summary.counters.not_saved, 0);
        assert_eq!(summary.failures.len(), 13);
        assert_eq!(summary.counters.classes[5], 13);
        for (k, failure) in summary.failures.iter().enumerate() {
            assert_eq!(failure.saved, SaveOutcome::Promoted);
            assert_eq!(failure.records.len(), 5);
            for run in &failure.records {
                for reference in [&run.spec, &run.record, &run.cleanup] {
                    assert_eq!(
                        reference.error,
                        match k.cmp(&2) {
                            std::cmp::Ordering::Less => None,
                            std::cmp::Ordering::Equal => Some(PersistError::Timeout),
                            std::cmp::Ordering::Greater => Some(PersistError::WorkerUnavailable),
                        }
                    );
                    if k < 2 {
                        assert!(directory.join(reference.path.as_ref().unwrap()).is_file());
                    } else {
                        assert!(reference.path.is_none());
                    }
                }
            }
        }
        let expected_charges = fake.tail_spec_bytes
            + 5 * (3 * 8192
                + 64 * 1024
                + b"record finalized\ncleanup: Confirmed\nkill attempts: 0\n".len());
        assert_eq!(tail.lock().unwrap().unwrap().tail_charges, expected_charges);
        let retained = budget.usage().retained;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !persistence.join_finished_writer() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(persistence.iteration_debug(), PersistenceDebug::default());
        // S has settled the report reservation. Only the one late spec commits.
        assert_eq!(budget.usage().reserved, 0);
        assert_eq!(budget.usage().retained, retained + late_bytes);
        assert!(writes.try_recv().is_err());
        assert!(persistence.join_finished_writer());
        assert_eq!(budget.usage().retained, retained + late_bytes);
        assert_eq!(report.data().details.pre_delivery_exit.code, 1);
        wire(&report);
        drop((report, persistence, fake));
        released_budget(&budget).await;
    });
}

#[test]
#[ignore = "ten thousand iterations with stalled stderr; dedicated stress CI step"]
fn r_s07_stress_results_survive_stalled_stderr_in_contract_order() {
    if !isolated(
        "r_s07_stress_results_survive_stalled_stderr_in_contract_order",
        true,
    ) {
        return;
    }
    run_test(move || async move {
        human_delivery(false).await;
    });
}

async fn human_delivery(real: bool) {
    let f = Fixture::new();
    let ready = ready(&f, "python", 0, &Tools);
    let budget = ready.budget.clone();
    let req = request(&ready, 10_000, true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let req = if real {
        StressRequest {
            templates: shell_templates(&ready),
            ..req
        }
    } else {
        req
    };
    let mut persistence = JobPersistence::new(&ready).unwrap();
    let writer = crate::progress::StalledWriter::new();
    let engine = Engine::stress(ready, req)
        .unwrap()
        .progress(writer.queue.clone());
    let mut executor = human_executor(real);
    let report = engine
        .run(executor.as_mut(), &mut persistence, &mut NoHooks)
        .await;
    assert_eq!(stress(&report).counters.retired, 10_000);
    assert_eq!(stress(&report).counters.classes[5], 3);
    assert_eq!(stress(&report).counters.runs_started, 50_000);
    assert_eq!(report.data().details.pre_delivery_exit.code, 1);
    assert_eq!(stress(&report).failures.len(), 3);
    let dropped = report
        .data()
        .details
        .warnings
        .iter()
        .find_map(|w| {
            if let WarningKind::ProgressDropped { lines } = w.kind {
                Some(lines)
            } else {
                None
            }
        })
        .unwrap();
    assert!(dropped > 0);
    let lines = stress(&report).result_lines(report.data().timing.duration);
    assert_eq!(lines.len(), 14);
    assert!(lines[7].starts_with("candidate_failure:"));
    for (role, line) in BatchRole::ALL.into_iter().zip(&lines[8..13]) {
        assert!(line.starts_with(&format!(
            "{}: 10000 runs, wall min/max/total ",
            role.token()
        )));
    }
    assert_eq!(
        lines[13],
        "failures: 0 saved, 0 promoted, 3 not saved, 0 binaries bytes"
    );
    let mut final_lines: Vec<_> = report
        .data()
        .details
        .warnings
        .iter()
        .map(|w| format!("warning: {}", w.message))
        .collect();
    final_lines.push("summary".into());
    let output = writer.finish(&final_lines);
    let actual: Vec<_> = std::str::from_utf8(&output).unwrap().lines().collect();
    let mut expected = vec!["startup diagnostic".to_owned()];
    assert!(actual[1].starts_with("10000 iterations from seed 19 in "));
    let mut expected_lines = lines.clone();
    expected_lines[0] = actual[1].to_owned(); // elapsed is rendered before F
    expected.extend(expected_lines);
    expected.extend(final_lines);
    assert_eq!(actual, expected);
    assert_eq!(lines[1], "stress stop: iterations at 10000");
    assert!(lines[2].starts_with("passed: 9997, first iteration 0 seed 19"));
    assert!(lines[3].starts_with("candidate_failure: 3, first iteration 3 seed 22"));
    for line in &lines[4..7] {
        assert!(line.starts_with("not saved: candidate_failure checker wrong_answer"));
    }
    assert_eq!(actual.last(), Some(&"summary"));
    wire(&report);
    drop((report, persistence, executor));
    released_budget(&budget).await;
}

fn human_executor(real: bool) -> Box<dyn Executor> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if real {
        return Box::new(SupervisorExecutor::start().unwrap());
    }
    assert!(!real);
    Box::new(Script {
        failing_iterations: Some([3, 7, 9]),
        ..Default::default()
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn shell_templates(ready: &ReadyJob<'_>) -> BatchTemplates<Box<crate::exec::ResolvedStage>> {
    let PlannedTarget::BuildFree(free) = &ready.planned_target else {
        panic!("build-free fixture");
    };
    let base = free.run(ready.dir.join("template").join("cwd")).unwrap();
    let template = |role| {
        let mut stage = base.clone();
        let crate::exec::ResolvedCommand::Argv { program, args } = &mut stage.command else {
            panic!("argv fixture");
        };
        program.path = PathBuf::from("/bin/sh");
        program.record.chosen_path = program.path.clone();
        program.record.request = crate::exec::ProgramRequest::Path(program.path.clone());
        let body = match role {
            BatchRole::Generator => r#"printf '%s\n' "$1""#,
            BatchRole::Checker => {
                r#"read -r seed < input; case "$seed" in 22|26|28) exit 1;; *) printf 'yes\n';; esac"#
            }
            _ => r"printf 'yes\n'",
        };
        *args = vec!["-c".into(), body.into(), "fixture".into()];
        stage.stdout.bytes.value = 16;
        stage.stderr.bytes.value = 0;
        RunTemplate::new(Box::new(stage), 16, 0).unwrap()
    };
    BatchTemplates::new(
        template(BatchRole::Generator),
        Some(template(BatchRole::Validator)),
        Some(template(BatchRole::Brute)),
        template(BatchRole::Candidate),
        Some(template(BatchRole::Checker)),
    )
    .unwrap()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[ignore = "fifty thousand native fixture processes; dedicated stress CI step"]
fn r_s07_real_stress_results_survive_stalled_stderr() {
    if !isolated("r_s07_real_stress_results_survive_stalled_stderr", true) {
        return;
    }
    run_test(move || async move {
        human_delivery(true).await;
    });
}

#[test]
fn candidate_cleanup_unverified_is_folded_once_across_iterations() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(&f, "python", 0, &Tools);
        let req = request(&ready, 100, true);
        let engine = Engine::stress(ready, req).unwrap();
        let mut fake = Script {
            unverified: true,
            ..Default::default()
        };
        let report = engine
            .run(&mut fake, &mut MemoryPersistence::default(), &mut NoHooks)
            .await;
        assert_eq!(stress(&report).counters.retired, 100);
        assert_eq!(stress(&report).counters.classes[0], 100);
        assert_eq!(report.data().details.pre_delivery_exit.code, 0);
        let value = wire(&report);
        assert_eq!(value["warnings"].as_array().unwrap().len(), 1);
        assert_eq!(value["warnings"][0]["kind"], "cleanup_unverified");
        assert_eq!(value["warnings"][0]["runs"], 100);
        assert_eq!(value["warnings_dropped"], 0);
    });
}
