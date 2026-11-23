#![cfg(any(target_os = "linux", target_os = "macos"))]

#[allow(dead_code)]
mod ready_support;

use cpforge::config::Comparator;
use cpforge::exec::NativeProbe;
use cpforge::jobs::engine::{
    CancelSource, Engine, EventPayload, JobRequest, MemoryPersistence, NoHooks, SupervisorExecutor,
};
use cpforge::jobs::{ReadyJob, prepare_job};
use cpforge::names::Identifier;
use cpforge::report::{JobVerdict, TestVerdict};
use cpforge::storage::inputs::CapturedSource;
use cpforge::storage::prune::PrunePolicy;
use cpforge::supervisor::{self, Stream};
use ready_support::Fixture;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

static TEST_LOCK: Mutex<()> = Mutex::new(());
static PROBE: NativeProbe = NativeProbe;

/// Hosted-runner allowance scale (execution contract §10.1): ×4 on macOS,
/// ×2 on hosted Linux CI, ×1 in the Linux container. Policy deadlines are
/// never scaled.
#[allow(dead_code)]
fn allowance_scale() -> u64 {
    if cfg!(target_os = "macos") {
        4
    } else if std::env::var_os("GITHUB_ACTIONS").is_some() {
        2
    } else {
        1
    }
}

fn run_test<F: Future<Output = ()>>(future: impl FnOnce() -> F + Send) {
    run_test_with_timeout(Duration::from_secs(120), future);
}

/// Drive the job engine on a thread with the main thread's 8 MiB stack, as the
/// CLI does: debug-build poll frames of the job state machine exceed the
/// 2 MiB default of test threads.
fn run_test_with_timeout<F: Future<Output = ()>>(
    timeout: Duration,
    future: impl FnOnce() -> F + Send,
) {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    supervisor::init_process().unwrap();
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn_scoped(scope, move || {
                let future = future();
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        tokio::time::timeout(timeout, future)
                            .await
                            .expect("job engine integration timed out");
                    });
            })
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}

fn ready(f: &Fixture, source: &str) -> ReadyJob<'static> {
    f.write("problem/main.cpp", source);
    let target = f.target("cpp", "");
    f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'one'\ninput = 'tests/one.in'\nanswer = 'tests/one.ans'\norigin = 'custom'\nenabled = true\n[[test]]\nid = 'two'\ninput = 'tests/two.in'\nanswer = 'tests/two.ans'\norigin = 'sample'\nenabled = true\n");
    f.write("problem/tests/one.in", "1\n");
    f.write("problem/tests/one.ans", "2\n");
    f.write("problem/tests/two.in", "2\n");
    f.write("problem/tests/two.ans", "5\n");
    let job = f.job(256 << 20, PrunePolicy::default());
    let selected = [
        Identifier::new("one").unwrap(),
        Identifier::new("two").unwrap(),
    ];
    let frozen = job
        .inputs()
        .freeze(
            &f.store(),
            &selected,
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    prepare_job(job, frozen, &target, f.inputs(&PROBE)).unwrap()
}

#[test]
fn cpp_two_tests_through_the_real_supervisor() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(
            &f,
            "#include <iostream>\nint main() { int n; std::cin >> n; std::cout << n * 2 << '\\n'; }\n",
        );
        let budget = ready.budget.clone();
        let retained_before = budget.usage().retained;
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: Comparator::Tokens,
            },
        )
        .unwrap();
        let report = engine
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        assert_eq!(report.data().result.verdict, JobVerdict::WrongAnswer);
        assert_eq!(
            report.data().tests[0].verdict.as_ref().unwrap(),
            &TestVerdict::Passed
        );
        assert_eq!(
            report.data().tests[1].verdict.as_ref().unwrap(),
            &TestVerdict::WrongAnswer
        );
        assert_eq!(report.data().executions.len(), 3);
        assert!(budget.usage().retained >= retained_before + 4);
        assert_eq!(report.data().result.counts.primary.started, 3);
        for execution in &report.data().executions {
            assert_eq!(execution.result.invariant, None);
            assert!(execution.summary.is_some());
        }
        assert_eq!(
            report
                .data()
                .build
                .as_ref()
                .unwrap()
                .build_id
                .as_ref()
                .unwrap()
                .as_str(),
            "r00000"
        );
        assert!(
            report
                .data()
                .job
                .dir
                .join("r00001")
                .join("out")
                .join("stdout")
                .is_file()
        );
    });
}

#[test]
fn cpp_cancel_after_the_running_program_announces_readiness() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(
            &f,
            "#include <iostream>\n#include <unistd.h>\nint main() { std::cout << \"ready\\n\" << std::flush; for (;;) pause(); }\n",
        );
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: Comparator::Tokens,
            },
        )
        .unwrap()
        .output_events(true);
        let cancel = engine.cancellation();
        let mut receiver = engine.events().subscribe().unwrap();
        let mut executor = SupervisorExecutor::start().unwrap();
        let mut persistence = MemoryPersistence::default();
        let mut hooks = NoHooks;
        let (report, terminal) = tokio::join!(
            engine.run(&mut executor, &mut persistence, &mut hooks),
            async {
                let mut output = Vec::new();
                while let Some(event) = receiver.recv().await {
                    if let EventPayload::OutputChunk {
                        stream: Stream::Stdout,
                        bytes,
                        ..
                    } = event.payload
                    {
                        output.extend_from_slice(&bytes);
                        if output.windows(6).any(|bytes| bytes == b"ready\n") {
                            cancel.request_cancel(CancelSource::Api);
                        }
                    }
                }
                receiver.terminal().await
            }
        );
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        assert_eq!(
            report.data().tests[0].verdict.as_ref().unwrap(),
            &TestVerdict::Cancelled
        );
        assert_eq!(
            report.data().tests[1].verdict.as_ref().unwrap(),
            &TestVerdict::Cancelled
        );
        assert_eq!(terminal.verdict, JobVerdict::Cancelled);
        assert_eq!(
            report.data().result.cancellation.unwrap().source,
            CancelSource::Api
        );
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancel_during_executor_creation_keeps_the_runtime_responsive() {
    run_test(move || async move { cancel_gated_creation(false).await });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancel_inside_os_creation_persists_transferred_record_and_report() {
    run_test(move || async move { cancel_gated_creation(true).await });
}

#[cfg(feature = "storage-test-hooks")]
async fn cancel_gated_creation(during_creation: bool) {
    use cpforge::supervisor::CreationGate;
    use std::sync::Arc;

    struct Release(Arc<CreationGate>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    fn allowance(milliseconds: u64) -> Duration {
        Duration::from_millis(milliseconds * allowance_scale())
    }
    let fixture = Fixture::new();
    let ready = ready(&fixture, "int main() { return 0; }\n");
    let directory = ready.dir.clone();
    let mut persistence = cpforge::jobs::persist::JobPersistence::new(&ready).unwrap();
    let engine = Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap();
    let cancel = engine.cancellation();
    let gate = Release(Arc::new(if during_creation {
        CreationGate::during_creation().unwrap()
    } else {
        CreationGate::before_commit()
    }));
    let mut executor = SupervisorExecutor::start().unwrap();
    executor.options.creation_gate = Some(gate.0.clone());
    let mut hooks = NoHooks;
    let mut future = Box::pin(engine.run(&mut executor, &mut persistence, &mut hooks));
    tokio::time::timeout(allowance(1000), async {
        tokio::select! {
            _ = gate.0.entered() => {},
            _ = future.as_mut() => panic!("job finished before gated creation"),
        }
    })
    .await
    .expect("creation blocked the runtime");
    cancel.request_cancel(CancelSource::Api);
    let report = tokio::time::timeout(allowance(1000), future.as_mut())
        .await
        .expect("job cancellation waited for gated creation");
    drop(future);
    assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
    assert_eq!(report.data().result.counts.primary.started, 0);
    let execution = &report.data().executions[0];
    let record_path = execution.record.as_ref().unwrap().path.as_ref().unwrap();
    let persisted_record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join(record_path)).unwrap()).unwrap();
    let persisted_report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("report.json")).unwrap()).unwrap();
    assert_eq!(persisted_report["result"]["verdict"], "cancelled");
    if during_creation {
        assert_eq!(
            execution.result.coverage,
            cpforge::report::Coverage::Transferred
        );
        assert!(execution.result.invariant.is_none());
        assert_eq!(report.data().result.counts.primary.transferred, 1);
        assert_eq!(persisted_record["execution"]["kind"], "transferred");
        assert!(persisted_record["execution"].get("status").is_none());
        assert_eq!(
            persisted_report["executions"][0]["coverage"]["kind"],
            "transferred"
        );
        assert_eq!(
            persisted_report["executions"][0]["summary"]["class"]["kind"],
            "cancelled"
        );
        assert!(persisted_report["executions"][0]["summary"]["timing"]["observer"].is_null());
    } else {
        assert_eq!(persisted_record["execution"]["kind"], "not_started");
    }
    gate.0.release();
    drop(executor);
    drop(persistence);
    tokio::time::timeout(allowance(1000), async {
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
    .expect("creation resources did not return after cancellation");
}

#[cfg(feature = "storage-test-hooks")]
async fn cancel_paused_run_setup(point: cpforge::jobs::engine::Point) {
    use cpforge::config::Platform;
    use cpforge::exec::{self, PreparedCommand, PreparedStage, ProgramRequest, Resolution};
    use cpforge::jobs::engine::{CommandKind, Iteration, Point, Recipe, Role};
    use cpforge::report::{Coverage, NotStartedReason};
    use cpforge::storage::job::{ProbeOutcome, probe};
    use std::sync::mpsc;

    struct Release(mpsc::Sender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    fn allowance() -> Duration {
        Duration::from_secs(allowance_scale())
    }

    let fixture = Fixture::new();
    let ready = ready(&fixture, "int main() { return 0; }\n");
    let job_dir = ready.dir.clone();
    let directory = job_dir.join("r00000");
    // Deferral does not probe this missing program. Cancellation at completion
    // must return before that probe (and before any process can start).
    let output = job_dir.join("generated");
    let plan = ready.target.run.clone();
    let prepared = PreparedStage {
        kind: plan.kind,
        command: PreparedCommand::Argv {
            program: ProgramRequest::Path(output.join("tool")),
            args: Vec::new(),
        },
        artifact_dir: Some(output),
        cwd: directory.join("cwd"),
        shared_cwd: false,
        environment: Vec::new(),
        deadline: plan.deadline.clone(),
        stdout: plan.stdout.clone(),
        stderr: plan.stderr.clone(),
        limits: plan.limits.clone(),
        plan,
    };
    let Resolution::AwaitingProducer(deferred) =
        exec::resolve(prepared, &PROBE, Platform::host()).unwrap()
    else {
        panic!("expected deferred program");
    };
    let entered = supervisor::CancelToken::new();
    let entered_worker = entered.clone();
    let (resume, receiver) = mpsc::channel();
    let release = Release(resume);
    let receiver = Mutex::new(receiver);
    let runtime_thread = std::thread::current().id();
    let engine = Engine::new(
        ready,
        JobRequest::Iterations {
            kind: CommandKind::Cmd,
            iterations: vec![Iteration {
                role: Role::Command {
                    test: None,
                    iteration: 1,
                },
                recipe: Recipe::Deferred(Box::new(deferred)),
                stdin: None,
                depends_on_build: false,
            }],
            needs_build: false,
            keep_going: false,
        },
    )
    .unwrap()
    .observe(move |observed| {
        if observed == point {
            assert_ne!(std::thread::current().id(), runtime_thread);
            assert_eq!(std::thread::current().name(), Some("cpf-create"));
            entered_worker.cancel();
            // A bounded channel barrier also releases a broken test eventually;
            // the async runtime's timers alone cannot unstick a blocked poll.
            receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(15))
                .expect("run setup pause was not released");
        }
    });
    let cancel = engine.cancellation();
    let mut executor = SupervisorExecutor::start().unwrap();
    let mut persistence = MemoryPersistence::default();
    let mut hooks = NoHooks;
    let mut future = Box::pin(engine.run(&mut executor, &mut persistence, &mut hooks));
    tokio::time::timeout(allowance(), async {
        tokio::select! {
            _ = entered.cancelled() => {},
            _ = future.as_mut() => panic!("job finished before run setup pause"),
        }
    })
    .await
    .expect("run setup blocked the runtime");
    let progress = tokio::spawn(async {
        tokio::task::yield_now().await;
        42
    });
    assert_eq!(
        tokio::time::timeout(allowance(), progress)
            .await
            .expect("another runtime task did not progress during setup")
            .unwrap(),
        42,
    );
    cancel.request_cancel(CancelSource::Api);
    let report = tokio::time::timeout(allowance(), future.as_mut())
        .await
        .expect("cancellation waited for run setup");
    drop(future);
    assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
    assert_eq!(report.data().result.counts.primary.started, 0);
    let execution = &report.data().executions[0];
    assert_eq!(
        execution.result.coverage,
        Coverage::NotStarted {
            reason: NotStartedReason::CancelledBeforeStart,
        },
    );
    assert_eq!(execution.result.invariant, None);
    assert_eq!(execution.run.as_ref().unwrap().as_str(), "r00000");
    // Report §4.5: interrupted preparation leaves program and spec null.
    assert!(execution.program.is_none() && execution.spec.is_none());
    assert_eq!(
        directory.exists(),
        point == Point::BeforeRecipeCompletion(0),
    );
    if point == Point::BeforeRecipeCompletion(0) {
        assert!(directory.join("cwd").is_dir());
        assert!(directory.join("out").is_dir());
    }
    // The report and the settled persistence hold no lease clone: only the
    // paused setup on the creation thread keeps the job active.
    drop(report);
    drop(persistence);
    assert!(matches!(probe(&job_dir), ProbeOutcome::Held));
    drop(release);
    drop(executor);
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
    .expect("run setup worker did not release its resources");
    assert!(matches!(probe(&job_dir), ProbeOutcome::Acquired(_)));
    assert_eq!(
        directory.exists(),
        point == Point::BeforeRecipeCompletion(0),
    );
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancel_before_run_mkdir_keeps_the_runtime_responsive() {
    run_test(|| cancel_paused_run_setup(cpforge::jobs::engine::Point::BeforeRunMkdir(0)));
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancel_before_recipe_completion_keeps_the_runtime_responsive() {
    run_test(|| cancel_paused_run_setup(cpforge::jobs::engine::Point::BeforeRecipeCompletion(0)));
}

// A build-free ready job with a resolved fixture command avoids compiler and
// interpreter startup. Every iteration still uses the real supervisor, pipes,
// capture workers, creation thread, cleanup scan and engine event queue.
#[cfg(feature = "storage-test-hooks")]
fn fixture_command_job(f: &Fixture, count: u32, budget: u64) -> (ReadyJob<'static>, JobRequest) {
    use cpforge::build::PlannedTarget;
    use cpforge::exec::ResolvedCommand;
    use cpforge::jobs::engine::{CommandKind, Iteration, Recipe, Role};
    f.write(
        "problem/main.py",
        "# frozen input; the resolved command runs cpf-fixture\n",
    );
    f.write("problem/tests.toml", "schema = 1\n");
    let target = f.target("python", "");
    let registered = f.job(budget, PrunePolicy::default());
    let frozen = registered
        .inputs()
        .freeze(
            &f.store(),
            &[],
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    let ready = prepare_job(registered, frozen, &target, f.inputs(&ready_support::Tools)).unwrap();
    let PlannedTarget::BuildFree(free) = &ready.planned_target else {
        panic!("expected a build-free fixture");
    };
    let mut stage = free.run(ready.dir.join("cwd")).unwrap();
    let ResolvedCommand::Argv { program, args } = &mut stage.command else {
        panic!("expected argv");
    };
    program.path = cpf_fixture::harness::fixture_path();
    program.record.chosen_path = program.path.clone();
    program.record.request = cpforge::exec::ProgramRequest::Path(program.path.clone());
    *args = vec!["exit:0".into()];
    let iterations = (0..count)
        .map(|index| Iteration {
            role: Role::Command {
                test: None,
                iteration: index + 1,
            },
            recipe: Recipe::Resolved(Box::new(stage.clone())),
            stdin: None,
            depends_on_build: false,
        })
        .collect();
    (
        ready,
        JobRequest::Iterations {
            kind: CommandKind::Cmd,
            iterations,
            needs_build: false,
            keep_going: true,
        },
    )
}

// The contract fixes the count at 10 000 real runs, so the test is not scaled
// down. It runs once per platform, in the all-features pass, instead of in
// both test passes; no per-milestone bound is relaxed.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn ten_thousand_real_runs_with_stalled_events_keep_terminal_sequence_and_report() {
    // Give the real executor a separate suite watchdog for 10 000 forks and scans.
    let scale = allowance_scale();
    run_test_with_timeout(Duration::from_secs(600 * scale), move || async move {
        let f = Fixture::new();
        // MemoryPersistence retains metadata allowances until settlement.
        let (ready, request) = fixture_command_job(&f, 10_000, 64 << 30);
        let engine = Engine::new(ready, request).unwrap();
        let receiver = engine.events().subscribe().unwrap();
        let mut persistence = MemoryPersistence::default();
        let report = engine
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut persistence,
                &mut NoHooks,
            )
            .await;
        // The receiver was subscribed but never polled while any run executed.
        assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
        assert_eq!(report.data().executions.len(), 10_000);
        assert_eq!(report.data().result.counts.primary.started, 10_000);
        for execution in &report.data().executions {
            assert_eq!(execution.result.invariant, None);
            let Some(cpforge::jobs::engine::ExecutionRecord::Started(root)) =
                execution.evidence.as_deref()
            else {
                panic!("fixture did not start: {execution:?}");
            };
            assert_eq!(root.status, supervisor::Termination::Exited { code: 0 });
        }
        let mut delivery = cpforge::transport::Delivery::new(Vec::new(), false);
        delivery
            .events(receiver, cpforge::report::JobKind::Cmd, false)
            .await
            .unwrap();
        let bytes = delivery.into_inner();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let lines: Vec<serde_json::Value> = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(lines[0]["document"], "event_stream");
        let events = &lines[1..lines.len() - 1];
        let sequence: Vec<u64> = events
            .iter()
            .map(|event| event["seq"].as_str().unwrap().parse().unwrap())
            .collect();
        assert_eq!(sequence[0], 0);
        assert!(sequence.windows(2).all(|pair| pair[0] < pair[1]));
        let terminal = events.last().unwrap();
        assert_eq!(terminal["event"], "job_finished");
        let dropped: u64 = terminal["dropped"].as_str().unwrap().parse().unwrap();
        assert!(dropped > 0);
        assert_eq!(
            *sequence.last().unwrap(),
            (events.len() - 1) as u64 + dropped
        );
        assert_eq!(dropped, report.data().events.dropped);
        let final_event = lines.last().unwrap();
        assert_eq!(final_event["event"], "report");
        assert_eq!(final_event["report"]["document"], "job_report");
        assert_eq!(final_event["report"]["result"]["verdict"], "succeeded");
        assert_eq!(
            final_event["report"]["executions"]
                .as_array()
                .unwrap()
                .len(),
            10_000
        );
        assert!(persistence.pending.is_empty());
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn healthy_run_milestones_meet_each_bound_over_five_runs() {
    run_test(move || async move {
        use cpforge::jobs::persist::JobPersistence;
        use cpforge::supervisor::{Milestone as M, Milestones};
        use std::sync::Arc;
        let f = Fixture::new();
        let mut maxima = [Duration::ZERO; 6];
        for _ in 0..5 {
            let (ready, request) = fixture_command_job(&f, 1, 256 << 20);
            let mut persistence = JobPersistence::new(&ready).unwrap();
            let stamps = Arc::new(Milestones::default());
            let mut executor = SupervisorExecutor::start().unwrap();
            executor.options.milestones = Some(stamps.clone());
            let report = Engine::new(ready, request)
                .unwrap()
                .run(&mut executor, &mut persistence, &mut NoHooks)
                .await;
            assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
            let Some(cpforge::jobs::engine::ExecutionRecord::Started(root)) =
                report.data().executions[0].evidence.as_deref()
            else {
                panic!("healthy fixture did not start");
            };
            assert!(root.errors.is_empty());
            assert!(root.stdout.as_ref().unwrap().complete());
            assert!(root.stderr.as_ref().unwrap().complete());
            assert!(
                report.data().executions[0]
                    .record
                    .as_ref()
                    .unwrap()
                    .path
                    .is_some()
            );
            let observed = root.timing.exit_observed.unwrap();
            let cleanup_interval = stamps
                .get(M::CleanupResult)
                .unwrap()
                .duration_since(observed);
            let frozen_interval = stamps
                .get(M::CaptureFrozen)
                .unwrap()
                .duration_since(observed);
            assert!(
                frozen_interval
                    <= cleanup_interval + Duration::from_millis(250 * allowance_scale()),
                "root exit → frozen: {frozen_interval:?}; cleanup interval: {cleanup_interval:?}"
            );
            for (index, (from, to)) in [
                (M::CleanupResult, M::DrainStart),
                (M::DrainStart, M::ReadersStopped),
                (M::ReadersStopped, M::CaptureFrozen),
                (M::CaptureFrozen, M::RecordFinalized),
                (M::RecordFinalized, M::RecordPersisted),
                (M::RecordFinalized, M::ResourcesReleased),
            ]
            .into_iter()
            .enumerate()
            {
                let earlier = stamps.get(from).unwrap();
                let later = stamps.get(to).unwrap();
                // EOF may stop the readers before the supervisor begins drain.
                if index != 1 {
                    assert!(later >= earlier, "{from:?} → {to:?}");
                }
                let elapsed = later.saturating_duration_since(earlier);
                maxima[index] = maxima[index].max(elapsed);
            }
        }
        let scale = allowance_scale();
        for (max, (milestone, milliseconds)) in maxima.into_iter().zip([
            ("cleanup → drain", 50),
            ("drain → readers stopped", 100),
            ("readers stopped → frozen", 100),
            ("frozen → finalized", 100),
            ("finalized → persisted", 100),
            ("finalized → released", 100),
        ]) {
            assert!(
                max <= Duration::from_millis(milliseconds * scale),
                "{milestone}: maximum {max:?}; bound {milliseconds} ms × {scale}"
            );
        }
    });
}

#[cfg(feature = "storage-test-hooks")]
async fn wait_for_cleanup_counters(expected: supervisor::Counters) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while supervisor::debug_counters() != expected {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "cleanup did not reach {expected:?}: {:?}",
            supervisor::debug_counters()
        )
    });
}

#[cfg(feature = "storage-test-hooks")]
async fn persistence_handover(rename: bool) {
    use cpforge::jobs::engine::{PersistError, ReportPersistence};
    use cpforge::jobs::persist::{JobPersistence, RenameGate};
    use std::sync::Arc;

    // Release even if an assertion fails, so a failed test cannot strand the
    // worker and its process-wide credit for the rest of this test binary.
    struct ReleaseOnDrop(Arc<RenameGate>, Arc<tokio::sync::Notify>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.release.notify_one();
            self.1.notify_one();
        }
    }

    tokio::time::timeout(Duration::from_secs(10 * allowance_scale()), async {
        let baseline = supervisor::Counters {
            credits_in_use: 1, // The cleanup polling thread's lifetime credit.
            ..Default::default()
        };
        wait_for_cleanup_counters(baseline).await;
        let f = Fixture::new();
        let (ready, request) = fixture_command_job(&f, 1, 256 << 20);
        let directory = ready.dir.clone();
        let budget = ready.budget.clone();
        let owner = Arc::downgrade(&ready.owner);
        let gate = Arc::new(RenameGate::default());
        let write_gate = Arc::new(tokio::sync::Notify::new());
        let _release = ReleaseOnDrop(gate.clone(), write_gate.clone());
        let mut persistence = if rename {
            JobPersistence::stalled_rename(&ready, Duration::from_secs(1), gate.clone())
        } else {
            JobPersistence::stalled(&ready, Duration::from_secs(1), write_gate.clone())
        };
        let engine = Engine::new(ready, request).unwrap();
        let mut receiver = engine.events().subscribe().unwrap();
        let mut executor = SupervisorExecutor::start().unwrap();
        let mut hooks = NoHooks;
        let (report, ()) = tokio::join!(
            engine.run(&mut executor, &mut persistence, &mut hooks),
            async {
                if rename {
                    gate.reached.notified().await;
                }
            }
        );
        assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
        for reference in [
            &report.data().executions[0].spec,
            &report.data().executions[0].record,
        ] {
            let reference = reference.as_ref().expect("settled artefact");
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
        while receiver.recv().await.is_some() {}
        assert!(receiver.terminal().await.report_id.is_none());
        drop(receiver);
        drop(report);
        drop(executor);
        let held = budget.usage();
        assert!(held.reserved > 0);
        assert!(!directory.join("r00000/spec.json").exists());
        drop(persistence);

        // The thread's one credit bounds its transfer entry; no cleanup slot
        // belongs to a job worker. Its lease and all allowances remain live.
        wait_for_cleanup_counters(supervisor::Counters {
            credits_in_use: baseline.credits_in_use + 1,
            service_pending: baseline.service_pending + 1,
            transferred_workers: baseline.transferred_workers + 1,
            ..baseline
        })
        .await;
        assert_eq!(budget.usage(), held);
        assert!(owner.upgrade().is_some());

        // A stalled persistence thread must not occupy the cleanup poller.
        // Submit a real transferred root while the write/rename gate stays shut.
        let stamps = Arc::new(supervisor::Milestones::default());
        let root = supervisor::spawn_root(
            supervisor::ProgramSpec {
                program: cpf_fixture::harness::fixture_path(),
                args: vec!["hold".into()],
                cwd: std::env::current_dir().unwrap(),
                env: Vec::new(),
                stdin: supervisor::StdinSpec::Null,
                stdout: supervisor::OutputSpec::Null,
                stderr: supervisor::OutputSpec::Null,
                limits: Vec::new(),
                measure: Vec::new(),
            },
            supervisor::admit().unwrap(),
            &supervisor::SpawnOptions {
                milestones: Some(stamps.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let ticket = root.ticket();
        let submitted = std::time::Instant::now();
        drop(root);
        tokio::time::timeout(
            Duration::from_secs(1) + Duration::from_millis(150 * allowance_scale()),
            async {
                let outcome = ticket.outcome().await.expect("transferred root outcome");
                assert_eq!(
                    outcome.status,
                    supervisor::Termination::Signalled {
                        signal: libc::SIGKILL,
                        core_dumped: false
                    }
                );
                wait_for_cleanup_counters(supervisor::Counters {
                    credits_in_use: baseline.credits_in_use + 1,
                    service_pending: baseline.service_pending + 1,
                    transferred_workers: baseline.transferred_workers + 1,
                    ..baseline
                })
                .await;
                while stamps
                    .get(supervisor::Milestone::ResourcesReleased)
                    .is_none()
                {
                    tokio::task::yield_now().await;
                }
            },
        )
        .await
        .expect("stalled persistence blocked cleanup of a transferred root");
        assert!(stamps.get(supervisor::Milestone::RootReaped).unwrap() >= submitted);
        let released = stamps
            .get(supervisor::Milestone::ResourcesReleased)
            .unwrap();
        let finalized = stamps.get(supervisor::Milestone::RecordFinalized).unwrap();
        assert!(
            released.saturating_duration_since(finalized)
                <= Duration::from_millis(100 * allowance_scale())
        );
        assert!(owner.upgrade().is_some());
        assert_eq!(budget.usage(), held);

        gate.release.notify_one();
        write_gate.notify_one();
        if rename {
            gate.completed.notified().await;
        }
        tokio::time::timeout(
            Duration::from_millis(100 * allowance_scale()),
            wait_for_cleanup_counters(baseline),
        )
        .await
        .expect("returned persistence worker kept its credit past the release bound");
        assert_eq!(budget.usage().reserved, 0);
        assert_eq!(budget.usage().retained, 0);
        assert!(owner.upgrade().is_none());
        if rename {
            let bytes = std::fs::read(directory.join("r00000/spec.json")).unwrap();
            assert!(cpforge::report::wire::read::read_json(&bytes).is_ok());
        } else {
            assert!(!directory.join("r00000/spec.json").exists());
        }
        assert!(!directory.join("r00000/record.json").exists());
        assert!(!directory.join("r00000/cleanup.log").exists());
        assert!(!directory.join("report.json").exists());
    })
    .await
    .expect("persistence handover timed out");
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn fixed_control_and_output_threads_hold_no_worker_credits() {
    run_test(move || async move {
        use cpforge::jobs::persist::JobPersistence;
        use cpforge::progress::ProgressWriter;
        use cpforge::transport::Delivery;
        use std::sync::Arc;
        use tokio::sync::Notify;

        struct Witness {
            name: &'static str,
            ready: Arc<Notify>,
        }
        impl std::io::Write for Witness {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                assert_eq!(std::thread::current().name(), Some(self.name));
                self.ready.notify_one();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let baseline = supervisor::Counters {
            credits_in_use: 1,
            ..Default::default()
        };
        wait_for_cleanup_counters(baseline).await;
        let out_ready = Arc::new(Notify::new());
        let err_ready = Arc::new(Notify::new());
        let progress = ProgressWriter::new(
            Witness {
                name: "cpf-stderr",
                ready: err_ready.clone(),
            },
            0,
        )
        .unwrap();
        let f = Fixture::new();
        let (ready, request) = fixture_command_job(&f, 1, 256 << 20);
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let engine = Engine::new(ready, request)
            .unwrap()
            .progress(progress.queue());
        let signals =
            cpforge::cancel_signals::CancelSignals::install(engine.cancellation()).unwrap();
        let mut executor = SupervisorExecutor::start().unwrap();
        let output = Delivery::new(
            Witness {
                name: "cpf-stdout",
                ready: out_ready.clone(),
            },
            false,
        )
        .start_events(
            engine.events().subscribe().unwrap(),
            cpforge::report::JobKind::Cmd,
            false,
        )
        .unwrap();
        tokio::time::timeout(Duration::from_millis(1000 * allowance_scale()), async {
            assert!(progress.queue().offer("job ready"));
            tokio::join!(out_ready.notified(), err_ready.notified());
        })
        .await
        .expect("output threads did not start");
        assert_eq!(
            supervisor::debug_counters().credits_in_use,
            baseline.credits_in_use + 2
        );
        let report = engine
            .run(&mut executor, &mut persistence, &mut NoHooks)
            .await;
        assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
        // Sinks can return their credits just after logical finalization.
        // Leave all three fixed threads installed while they settle.
        tokio::time::timeout(
            Duration::from_millis(100 * allowance_scale()),
            wait_for_cleanup_counters(supervisor::Counters {
                credits_in_use: baseline.credits_in_use + 2,
                ..baseline
            }),
        )
        .await
        .expect("only cleanup and job workers should hold credits");
        output.finish().unwrap();
        progress.finish("finished").unwrap();
        drop(signals);
        drop(persistence);
        drop(executor);
        tokio::time::timeout(
            Duration::from_millis(100 * allowance_scale()),
            wait_for_cleanup_counters(baseline),
        )
        .await
        .expect("job credits did not return");
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn healthy_idle_persistence_returns_credit_without_cleanup_handover() {
    run_test(move || async move {
        use cpforge::jobs::persist::JobPersistence;
        let baseline = supervisor::Counters {
            credits_in_use: 1,
            ..Default::default()
        };
        wait_for_cleanup_counters(baseline).await;
        let handovers = supervisor::persistence_handovers();
        let f = Fixture::new();
        let (ready, request) = fixture_command_job(&f, 1, 256 << 20);
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let engine = Engine::new(ready, request).unwrap();
        let mut executor = SupervisorExecutor::start().unwrap();
        let report = engine
            .run(&mut executor, &mut persistence, &mut NoHooks)
            .await;
        assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
        assert!(matches!(
            report.persistence(),
            cpforge::jobs::engine::ReportPersistence::Published { .. }
        ));
        tokio::time::timeout(
            Duration::from_millis(100 * allowance_scale()),
            wait_for_cleanup_counters(supervisor::Counters {
                credits_in_use: baseline.credits_in_use + 2,
                ..baseline
            }),
        )
        .await
        .expect("healthy run resources did not settle");
        assert_eq!(
            supervisor::debug_counters().service_pending,
            baseline.service_pending
        );
        assert_eq!(
            supervisor::debug_counters().transferred_workers,
            baseline.transferred_workers
        );
        drop(persistence);
        drop(executor);
        tokio::time::timeout(
            Duration::from_millis(100 * allowance_scale()),
            wait_for_cleanup_counters(baseline),
        )
        .await
        .expect("healthy job workers kept their credits after job end");
        assert_eq!(supervisor::persistence_handovers(), handovers);
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn stalled_persistence_handover_holds_queued_reservations_until_worker_returns() {
    run_test(move || async move { persistence_handover(false).await });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn late_rename_handover_holds_credit_lease_and_metadata_until_worker_returns() {
    run_test(move || async move { persistence_handover(true).await });
}

#[cfg(feature = "storage-test-hooks")]
#[path = "support/tiny_filesystem.rs"]
mod tiny_filesystem;

#[cfg(feature = "storage-test-hooks")]
#[test]
fn real_enospc_record_and_report_failures_keep_verdict_refs_and_delivery() {
    run_test(move || async move {
        use cpforge::exit::ExitCode;
        use cpforge::jobs::engine::{PersistError, Point, ReportPersistence, Warning};
        use cpforge::jobs::persist::JobPersistence;
        use cpforge::transport::{Delivery, MachineMode, delivered_status};
        use std::sync::Arc;
        for records_fail in [false, true] {
            let Some(filesystem) = tiny_filesystem::TinyFs::one_mib() else {
                return;
            };
            let filesystem = Arc::new(filesystem);
            let f = Fixture::at(filesystem.directory.join("workspace"));
            let (ready, request) = fixture_command_job(&f, 1, 256 << 20);
            let directory = ready.dir.clone();
            let mut persistence = JobPersistence::new(&ready).unwrap();
            let fill = filesystem.clone();
            let spec_path = directory.join("r00000/spec.json");
            let engine = Engine::new(ready, request).unwrap().observe(move |point| {
                if (records_fail && point == Point::BeforeRecordEnqueue(0))
                    || (!records_fail && point == Point::BeforeSnapshot)
                {
                    if records_fail {
                        // Finish admission-time persistence before exhausting
                        // space, so no failed spec temporary can free pages
                        // concurrently with the record write.
                        let deadline = std::time::Instant::now() + Duration::from_secs(5);
                        while !spec_path.is_file() {
                            assert!(std::time::Instant::now() < deadline);
                            std::thread::yield_now();
                        }
                    }
                    fill.fill_completely();
                }
            });
            let mut receiver = engine.events().subscribe().unwrap();
            let report = engine
                .run(
                    &mut SupervisorExecutor::start().unwrap(),
                    &mut persistence,
                    &mut NoHooks,
                )
                .await;
            assert_eq!(report.data().result.verdict, JobVerdict::Succeeded);
            let enospc = PersistError::Io {
                os_error: Some(libc::ENOSPC),
            };
            let reference = report.data().executions[0].record.as_ref().unwrap();
            if records_fail {
                assert!(reference.path.is_none());
                assert_eq!(reference.error, Some(enospc.clone()));
                assert!(report.data().warnings.iter().any(|warning| matches!(warning, Warning::Persistence { artefact: "record", error, .. } if *error == enospc)));
            } else {
                assert!(reference.path.is_some());
                assert_eq!(reference.error, None);
                assert!(
                    report
                        .data()
                        .warnings
                        .iter()
                        .all(|warning| !matches!(warning, Warning::Persistence { .. }))
                );
            }
            assert!(
                matches!(report.persistence(), ReportPersistence::Failed { error, .. } if *error == enospc)
            );
            assert!(!directory.join("report.json").exists());
            while receiver.recv().await.is_some() {}
            let terminal = receiver.terminal().await;
            assert!(terminal.report_id.is_none());
            assert!(Arc::ptr_eq(&terminal.report, &report));
            let mut delivery = Delivery::new(Vec::new(), false);
            let delivered = delivery.report(&report);
            assert!(delivered.is_ok());
            assert_eq!(
                delivered_status(&report, MachineMode::Json, &delivered),
                ExitCode::Success
            );
            let bytes = delivery.into_inner();
            assert_eq!(bytes.last(), Some(&b'\n'));
            let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(document["document"], "job_report");
            assert_eq!(document["persistence"]["report"]["kind"], "failed");
            drop(persistence);
            drop(report);
            drop(terminal);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match cpforge::storage::job::probe(&directory) {
                        cpforge::storage::job::ProbeOutcome::Acquired(lock) => {
                            drop(lock);
                            break;
                        }
                        cpforge::storage::job::ProbeOutcome::Held => tokio::task::yield_now().await,
                        outcome => panic!("persistence lease cleanup failed: {outcome:?}"),
                    }
                }
            })
            .await
            .expect("persistence worker did not release its lease");
            drop(f);
            drop(filesystem);
        }
    });
}
