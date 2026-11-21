#![cfg(target_os = "linux")]

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
                &mut SupervisorExecutor::default(),
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
        let mut executor = SupervisorExecutor::default();
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
