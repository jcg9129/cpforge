#![cfg(any(target_os = "linux", target_os = "macos"))]

#[allow(dead_code)] // Shared portable fixture also serves filesystem-only tests.
mod ready_support;

use cpforge::build::{self, BuildExecutionOutcome, PendingBuild, PlannedTarget, ValidatedBuild};
use cpforge::config::Platform;
use cpforge::exec::{LaunchKind, NativeProbe, ResolvedStage};
use cpforge::jobs::{CaptureCheck, CaptureCheckReason, prepare_build_job, prepare_job};
use cpforge::storage::inputs::CapturedSource;
use cpforge::storage::prune::PrunePolicy;
use cpforge::supervisor::{
    CancelToken, OutputSpec, ParentReader, ProgramSpec, ReadResult, RootOutcome, RunControls,
    SpawnOptions, StdinSpec, Termination, admit, debug_counters, init_process, spawn_root,
    stdout_pipe, wait_root,
};
use ready_support::Fixture;
use std::fs;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

static TEST_LOCK: Mutex<()> = Mutex::new(());
static PROBE: NativeProbe = NativeProbe;

fn run_test(body: impl Future<Output = ()>) {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    init_process().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // Allow real g++ builds and supervisor cleanup on shared runners.
            tokio::time::timeout(Duration::from_secs(150), async {
                body.await;
                loop {
                    let counters = debug_counters();
                    if counters.slots_in_use == 0
                        && counters.service_pending == 0
                        && counters.credits_in_use == 1
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("real-build test exceeded 150 seconds");
        });
}

struct Execution {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    outcome: RootOutcome,
}

impl Execution {
    fn success(&self) {
        assert_eq!(
            self.outcome.status,
            Termination::Exited { code: 0 },
            "{}",
            self.diagnostics()
        );
        self.clean();
    }

    fn clean(&self) {
        assert!(self.outcome.errors.is_empty(), "{}", self.diagnostics());
        assert!(self.outcome.violations.is_empty(), "{}", self.diagnostics());
        assert!(
            self.outcome.stop_trigger.is_none(),
            "{}",
            self.diagnostics()
        );
        assert!(self.outcome.enforced_at.is_none(), "{}", self.diagnostics());
    }

    fn diagnostics(&self) -> String {
        format!(
            "{:?}\nstderr: {}",
            self.outcome,
            String::from_utf8_lossy(&self.stderr)
        )
    }
}

async fn drain(mut reader: ParentReader) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match reader.read_chunk(&mut buffer).await {
            ReadResult::Data(n) => bytes.extend_from_slice(&buffer[..n]),
            ReadResult::Eof => return bytes,
            ReadResult::Failed(error) => panic!("output read failed: {error}"),
        }
    }
}

async fn launch(stage: ResolvedStage, stdin: StdinSpec) -> Execution {
    let command = stage.launch_command(Platform::host()).unwrap();
    assert_eq!(command.launch_kind, LaunchKind::Native);
    let stdout = stdout_pipe().unwrap();
    let stderr = stdout_pipe().unwrap();
    let wall_deadline = Duration::from_millis(stage.deadline.milliseconds.value);
    // Explicit test adapter, as in exec_linux. Job admission owns CPF_*,
    // normalized limits and persistent sinks; these fixtures need none of them.
    let spec = ProgramSpec {
        program: command.program,
        args: command.args,
        cwd: stage.cwd,
        env: stage.environment,
        stdin,
        stdout: OutputSpec::Pipe(stdout.child),
        stderr: OutputSpec::Pipe(stderr.child),
        limits: Vec::new(),
        measure: Vec::new(),
    };
    let root = spawn_root(spec, admit().unwrap(), &SpawnOptions::default()).unwrap();
    let (stdout, stderr, outcome) = tokio::join!(
        drain(stdout.parent),
        drain(stderr.parent),
        wait_root(
            root,
            RunControls {
                wall_deadline,
                cancel: CancelToken::new(),
                events: None,
            }
        )
    );
    Execution {
        stdout,
        stderr,
        outcome,
    }
}

async fn compile(pending: PendingBuild<'_>) -> ValidatedBuild {
    let execution = launch(pending.build_stage().clone(), StdinSpec::Null).await;
    execution.success();
    build::finish_build(pending, BuildExecutionOutcome::from(&execution.outcome))
        .unwrap_or_else(|error| panic!("{error}\n{}", execution.diagnostics()))
}

#[test]
fn prepared_argv_build_and_build_free_run_execute_captured_bytes_after_edit() {
    run_test(async {
        for language in ["cpp", "python"] {
            let f = Fixture::new();
            let (name, original, edited) = if language == "cpp" {
                (
                    "main.cpp",
                    "#include <iostream>\nint main() { std::cout << \"old\\n\"; }\n",
                    "#include <iostream>\nint main() { std::cout << \"new\\n\"; }\n",
                )
            } else {
                ("main.py", "print('old')\n", "print('new')\n")
            };
            f.write(&format!("problem/{name}"), original);
            let target = f.target(language, "");
            let registered = f.job(128 << 20, PrunePolicy::default());
            let frozen = registered
                .inputs()
                .freeze(
                    &f.store(),
                    &[],
                    &CapturedSource::from_target(&target),
                    &f.path("problem"),
                )
                .unwrap();
            let ready = prepare_job(registered, frozen, &target, f.inputs(&PROBE)).unwrap();
            assert_eq!(ready.capture_check, CaptureCheck::Checked);
            f.write(&format!("problem/{name}"), edited);
            let cwd = ready.dir.join("r1").join("cwd");
            fs::create_dir_all(&cwd).unwrap();
            let stage = match ready.planned_target {
                PlannedTarget::Build(pending) => compile(pending).await.run(cwd).unwrap(),
                PlannedTarget::BuildFree(run) => run.run(cwd).unwrap(),
            };
            let execution = launch(stage, StdinSpec::Null).await;
            execution.success();
            assert_eq!(execution.stdout, b"old\n");
            // A partial field move retains ready.owner until execution finishes.
            drop(ready.owner);
        }
    });
}

#[test]
fn literal_original_path_in_shell_script_observes_edit_and_is_unchecked() {
    run_test(async {
        let f = Fixture::new();
        f.write(
            "problem/main.cpp",
            "#include <iostream>\nint main() { std::cout << \"old\\n\"; }\n",
        );
        // Shell text is literal, including this absolute path to the original.
        // Source and artifact positional args still pass through the guard.
        let original = f.path("problem/main.cpp");
        #[cfg(target_os = "linux")]
        let compiler = "g++";
        #[cfg(target_os = "macos")]
        let compiler = "g++-14";
        let script = format!("{compiler} '{}' -o \"$2\"", original.display());
        f.write("user.toml", &format!("schema = 1\n[languages.cpp.build]\nshell = {{ interpreter = ['sh', '-c'], script = '''{script}''', args = ['{sources}', '{artifact}'] }}\n", sources = "{sources}", artifact = "{artifact}"));
        let target = f.target("cpp", "");
        let ready = prepare_build_job(
            f.job(128 << 20, PrunePolicy::default()),
            &target,
            f.inputs(&PROBE),
        )
        .unwrap();
        assert_eq!(
            ready.capture_check,
            CaptureCheck::Unchecked {
                reasons: vec![CaptureCheckReason::ShellStage]
            }
        );
        f.write(
            "problem/main.cpp",
            "#include <iostream>\nint main() { std::cout << \"new\\n\"; }\n",
        );
        let PlannedTarget::Build(pending) = ready.planned_target else {
            panic!()
        };
        let build = compile(pending).await;
        let cwd = ready.dir.join("r1").join("cwd");
        fs::create_dir_all(&cwd).unwrap();
        let execution = launch(build.run(cwd).unwrap(), StdinSpec::Null).await;
        execution.success();
        assert_eq!(execution.stdout, b"new\n");
        assert_eq!(
            fs::read(&ready.sources.source_bindings()[0]).unwrap(),
            b"#include <iostream>\nint main() { std::cout << \"old\\n\"; }\n"
        );
    });
}

#[test]
fn managed_two_test_job_persists_reports_and_capture_files() {
    use cpforge::jobs::engine::{Engine, JobRequest, NoHooks, SupervisorExecutor};
    use cpforge::jobs::persist::JobPersistence;
    use cpforge::report::{JobVerdict, wire};
    run_test(async {
        let f = Fixture::new();
        f.write(
            "problem/main.cpp",
            "#include <iostream>\nint main() { std::cout << \"yes\\n\"; }\n",
        );
        f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n[[test]]\nid = 'b'\ninput = 'tests/b.in'\nanswer = 'tests/b.ans'\norigin = 'custom'\nenabled = true\n");
        for id in ["a", "b"] {
            f.write(&format!("problem/tests/{id}.in"), "");
            f.write(&format!("problem/tests/{id}.ans"), "yes\n");
        }
        let target = f.target("cpp", "");
        let registered = f.job(256 << 20, PrunePolicy::default());
        let ids = ["a", "b"].map(|id| cpforge::names::Identifier::new(id).unwrap());
        let frozen = registered
            .inputs()
            .freeze(
                &f.store(),
                &ids,
                &CapturedSource::from_target(&target),
                &f.path("problem"),
            )
            .unwrap();
        let ready = prepare_job(registered, frozen, &target, f.inputs(&PROBE)).unwrap();
        let dir = ready.dir.clone();
        let mut persistence = JobPersistence::new(&ready).unwrap();
        let engine = Engine::new(
            ready,
            JobRequest::Test {
                comparator: cpforge::config::Comparator::Tokens,
            },
        )
        .unwrap();
        let mut receiver = engine.events().subscribe().unwrap();
        let report = engine
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut persistence,
                &mut NoHooks,
            )
            .await;
        assert_eq!(
            report.data().result.verdict,
            JobVerdict::Passed,
            "{:?}",
            report.data().executions
        );
        assert_eq!(report.data().tests.len(), 2);
        for run in ["r00000", "r00001", "r00002"] {
            let spec = fs::read(dir.join(run).join("spec.json")).unwrap();
            let record = fs::read(dir.join(run).join("record.json")).unwrap();
            assert_eq!(wire::read::read_json(&spec).unwrap()["schema"], 1);
            assert_eq!(wire::read::read_json(&record).unwrap()["schema"], 1);
            assert!(dir.join(run).join("out/stdout").is_file());
            assert!(dir.join(run).join("out/stderr").is_file());
            assert!(dir.join(run).join("cleanup.log").is_file());
        }
        assert_eq!(fs::read(dir.join("r00001/out/stdout")).unwrap(), b"yes\n");
        let bytes = fs::read(dir.join("report.json")).unwrap();
        assert_eq!(wire::read::report_schema_prefix(&bytes).unwrap(), 1);
        assert_eq!(bytes, wire::encode_report(&report).unwrap());
        let value = wire::read::read_json(&bytes).unwrap();
        assert_eq!(
            value["executions"][1]["summary"]["stdout"]["file"],
            "r00001/out/stdout"
        );
        assert_eq!(
            value["executions"][1]["summary"]["stdout"]["complete"],
            true
        );
        while receiver.recv().await.is_some() {}
        assert_eq!(
            receiver.terminal().await.report_id.as_ref(),
            Some(&report.data().job.id)
        );
        drop(persistence);
    });
}
