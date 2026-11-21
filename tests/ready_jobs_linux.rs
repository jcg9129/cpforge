#![cfg(target_os = "linux")]

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
    let command = stage.launch_command(Platform::Linux).unwrap();
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
        let script = format!("g++ '{}' -o \"$2\"", original.display());
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
