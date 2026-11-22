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

fn run_test(future: impl Future<Output = ()>) {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    supervisor::init_process().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(120), future)
                .await
                .expect("job engine integration timed out");
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
    run_test(async {
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
    run_test(async {
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
    run_test(async {
        use cpforge::supervisor::CreationGate;
        use std::sync::Arc;

        struct Release(Arc<CreationGate>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.release();
            }
        }

        fn allowance(milliseconds: u64) -> Duration {
            Duration::from_millis(milliseconds * if cfg!(target_os = "macos") { 4 } else { 1 })
        }
        let fixture = Fixture::new();
        let ready = ready(&fixture, "int main() { return 0; }\n");
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: Comparator::Tokens,
            },
        )
        .unwrap();
        let cancel = engine.cancellation();
        let gate = Release(Arc::new(CreationGate::before_commit()));
        let mut executor = SupervisorExecutor::start().unwrap();
        executor.options.creation_gate = Some(gate.0.clone());
        let mut persistence = MemoryPersistence::default();
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
            .expect("job cancellation waited for uncommitted creation");
        drop(future);
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        assert_eq!(report.data().result.counts.primary.started, 0);
        gate.0.release();
        drop(executor);
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
    });
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
        Duration::from_secs(if cfg!(target_os = "macos") { 4 } else { 1 })
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
    run_test(cancel_paused_run_setup(
        cpforge::jobs::engine::Point::BeforeRunMkdir(0),
    ));
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancel_before_recipe_completion_keeps_the_runtime_responsive() {
    run_test(cancel_paused_run_setup(
        cpforge::jobs::engine::Point::BeforeRecipeCompletion(0),
    ));
}
