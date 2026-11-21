#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use cpforge::build::{
    self, BuildArea, BuildExecutionOutcome, BuildFailureKind, BuildId, PendingBuild, PlanInputs,
    PlannedTarget, ValidatedBuild,
};
use cpforge::config::{ConfigRequest, Environment, Platform, TargetPlan, load_config};
use cpforge::exec::{
    Baseline, InvocationDirs, LaunchKind, NativeProbe, ProblemBinding, ResolvedStage,
};
use cpforge::supervisor::{
    CancelToken, OutputSpec, ParentReader, ProgramSpec, ReadResult, RootOutcome, RunControls,
    SpawnOptions, StdinSpec, Termination, admit, debug_counters, init_process, spawn_root,
    stdout_pipe, wait_root,
};

// Own the runtime and children until quiescence, as in exec_linux.
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
            // Real javac can take several seconds on shared CI runners. Allow
            // two 60-second builds plus runs and cleanup (the stale-output case).
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

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "build-linux-{} space $",
                BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&root).unwrap();
        let dir = Self(fs::canonicalize(root).unwrap());
        for name in ["problem", "inputs/src", "builds"] {
            fs::create_dir_all(dir.path(name)).unwrap();
        }
        fs::write(dir.path("user.toml"), "schema = 1\n").unwrap();
        dir.workspace("");
        dir
    }

    /// Language tables (and so `artifacts`) are not problem-layer keys; a
    /// target-specific artifact declaration belongs to the workspace layer.
    fn workspace(&self, extra: &str) {
        #[cfg(target_os = "macos")]
        let extra = format!(
            "[languages.cpp.build]\nargv = ['g++-14', '-std=gnu++23', '-O2', '{{sources}}', '-o', '{{artifact}}']\n{extra}"
        );
        fs::write(
            self.path("cpforge.toml"),
            format!("schema = 1\n[build]\ndeadline_s = 60\n[run]\ndeadline_s = 10\n{extra}"),
        )
        .unwrap();
    }

    fn path(&self, relative: &str) -> PathBuf {
        // Join per component so `/` in fixtures becomes the native separator.
        Path::new(relative)
            .components()
            .fold(self.0.clone(), |path, part| path.join(part))
    }

    fn fresh_cwd(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir(&path).unwrap();
        path
    }

    fn source(&self, name: &str, contents: &str) {
        let path = self.path("problem").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn target(&self, language: &str, extra: &str) -> TargetPlan {
        let file = self.path("problem/problem.toml");
        fs::write(
            &file,
            format!(
                "schema = 1\n[problem]\nid = 'local:p'\n[config]\nlanguage = '{language}'\n{extra}\n"
            ),
        )
        .unwrap();
        load_config(&ConfigRequest {
            environment: Environment::default(),
            platform: Platform::host(),
            current_dir: self.0.clone(),
            config_path: Some(self.path("user.toml")),
            workspace_root: Some(self.0.clone()),
            problem_file: Some(file),
            ..ConfigRequest::default()
        })
        .target()
        .unwrap()
    }

    fn capture(&self, target: &TargetPlan) {
        for file in target.sources.iter().chain(&target.support_files) {
            let relative: PathBuf = file.value.components().iter().collect();
            let original = self.path("problem").join(&relative);
            let captured = self.path("inputs/src").join(&relative);
            fs::create_dir_all(captured.parent().unwrap()).unwrap();
            fs::copy(&original, captured).unwrap();
            // Removing originals makes fallback to the problem tree fail.
            fs::remove_file(original).unwrap();
        }
    }

    fn plan(&self, target: &TargetPlan, id: &str) -> PlannedTarget<'static> {
        let outputs = target.build.as_ref().map(|_| {
            build::allocate(
                &BuildArea::new(self.path("builds")).unwrap(),
                BuildId::new(id).unwrap(),
            )
            .unwrap()
        });
        let sources = target
            .sources
            .iter()
            .map(|source| {
                source
                    .value
                    .components()
                    .iter()
                    .fold(self.path("inputs/src"), |path, part| path.join(part))
            })
            .collect();
        // Only PATH is inherited: ambient Python/Java options must not change
        // bytecode generation, classpaths or the fixture's output.
        let baseline = Baseline::from_entries(vec![
            (
                "PATH".into(),
                std::env::var_os("PATH").expect("Toolchain tests require PATH"),
            ),
            ("LANG".into(), "C.UTF-8".into()),
        ]);
        build::plan_target(
            target,
            PlanInputs {
                sources,
                dirs: InvocationDirs {
                    run_cwd: self.fresh_cwd(&format!("build-cwd-{id}")),
                    problem: Some(ProblemBinding {
                        dir: self.path("problem"),
                        id: "local:p".into(),
                    }),
                    workspace: Some(self.0.clone()),
                },
                baseline,
                platform: Platform::host(),
                build_bytes: 64 << 20,
                probe: &PROBE,
            },
            outputs,
        )
        .unwrap_or_else(|errors| {
            panic!(
                "Build proofs require the C++ compiler, python3, javac and java on PATH; planning failed: {errors}"
            )
        })
    }

    fn pending(&self, target: &TargetPlan, id: &str) -> PendingBuild<'static> {
        let PlannedTarget::Build(pending) = self.plan(target, id) else {
            panic!("expected a build-backed target");
        };
        assert_ne!(pending.build_stage().cwd, pending.output_root());
        assert_eq!(pending.build_stage().deadline.milliseconds.value, 60_000);
        pending
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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

async fn run(build: &ValidatedBuild, dir: &Directory, name: &str, input: &str) -> Vec<u8> {
    let cwd = dir.fresh_cwd(name);
    let stdin = cwd.join("stdin.txt");
    fs::write(&stdin, input).unwrap();
    let stage = build.run(cwd.clone()).unwrap();
    assert_eq!(stage.cwd, cwd);
    let execution = launch(stage, StdinSpec::File(stdin)).await;
    execution.success();
    execution.stdout
}

fn no_pycache(root: &Path) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        assert_ne!(entry.file_name(), "__pycache__");
        if entry.file_type().unwrap().is_dir() {
            no_pycache(&entry.path());
        }
    }
}

const SUM: &str = r#"#include <iostream>
int main() {
    int a, b;
    std::cin >> a >> b;
    std::cout << a + b << '\n';
}
"#;

#[test]
fn cpp_generated_program_runs_twice_from_one_validated_build() {
    run_test(async {
        let dir = Directory::new();
        dir.source("main.cpp", SUM);
        let target = dir.target("cpp", "");
        dir.capture(&target);
        let pending = dir.pending(&target, "sum");
        let artifact = pending.output_root().join("main");
        assert_eq!(pending.required_outputs(), std::slice::from_ref(&artifact));
        assert!(!artifact.exists());
        let build = compile(pending).await;
        assert_eq!(
            build
                .run(dir.fresh_cwd("inspect-cwd"))
                .unwrap()
                .launch_command(Platform::host())
                .unwrap()
                .program,
            artifact
        );
        assert_eq!(run(&build, &dir, "run-one", "2 3\n").await, b"5\n");
        assert_eq!(run(&build, &dir, "run-two", "-8 19\n").await, b"11\n");
    });
}

#[test]
fn cpp_grader_compiles_sources_with_captured_support_header() {
    run_test(async {
        let dir = Directory::new();
        dir.source(
            "solution.cpp",
            "#include \"grader.h\"\nint twice(int n) { return 2 * n; }\n",
        );
        dir.source("grader.h", "int twice(int);\n");
        dir.source("grader.cpp", "#include \"grader.h\"\n#include <iostream>\nint main() { std::cout << twice(21) << '\\n'; }\n");
        let target = dir.target("cpp", "sources = ['solution.cpp', 'grader.cpp']\nsupport_files = ['grader.h']\nsolution_files = ['solution.cpp']");
        dir.capture(&target);
        let pending = dir.pending(&target, "grader");
        let args = pending
            .build_stage()
            .launch_command(Platform::host())
            .unwrap()
            .args;
        for source in ["solution.cpp", "grader.cpp"] {
            assert!(args.contains(&dir.path("inputs/src").join(source).into_os_string()));
        }
        assert!(
            !args
                .iter()
                .any(|arg| Path::new(arg).file_name() == Some(OsStr::new("grader.h"))),
            "{args:?}"
        );
        assert!(dir.path("inputs/src/grader.h").is_file());
        let build = compile(pending).await;
        assert_eq!(run(&build, &dir, "run", "").await, b"42\n");
    });
}

async fn python_case(dir: &Directory, target: &TargetPlan, expected: &[u8]) {
    dir.capture(target);
    assert!(target.build.is_none());
    assert!(target.artifacts.is_none());
    let PlannedTarget::BuildFree(ready) = dir.plan(target, "python") else {
        panic!("Python must be build-free");
    };
    assert_eq!(fs::read_dir(dir.path("builds")).unwrap().count(), 0);
    let cwd = dir.fresh_cwd("run");
    let stage = ready.run(cwd.clone()).unwrap();
    assert_eq!(stage.cwd, cwd);
    let command = stage.launch_command(Platform::host()).unwrap();
    assert_eq!(command.program.file_name().unwrap(), "python3");
    assert_eq!(
        command.args,
        [
            OsString::from("-B"),
            dir.path("inputs/src")
                .join(target.sources[0].value.components().join("/"))
                .into_os_string()
        ]
    );
    let execution = launch(stage, StdinSpec::Null).await;
    execution.success();
    assert_eq!(execution.stdout, expected);
    no_pycache(&dir.path("inputs/src"));
    assert_eq!(fs::read_dir(dir.path("builds")).unwrap().count(), 0);
}

#[test]
fn python_build_free_runs_captured_script_without_output_root() {
    run_test(async {
        let dir = Directory::new();
        dir.source("main.py", "print('captured python')\n");
        let target = dir.target("python", "");
        python_case(&dir, &target, b"captured python\n").await;
    });
}

#[test]
fn python_grader_imports_captured_support_module_from_separate_cwd() {
    run_test(async {
        let dir = Directory::new();
        dir.source("grader.py", "import solution\nprint(solution.twice(21))\n");
        dir.source("solution.py", "def twice(n):\n    return 2 * n\n");
        let target = dir.target("python", "sources = ['grader.py']\nsupport_files = ['solution.py']\nsolution_files = ['solution.py']");
        python_case(&dir, &target, b"42\n").await;
    });
}

const MAIN: &str =
    "public class Main { public static void main(String[] args) { System.out.println(42); } }\n";

#[test]
fn java_default_requires_main_class_and_runs_with_output_root_classpath() {
    run_test(async {
        let dir = Directory::new();
        dir.source("Main.java", MAIN);
        let target = dir.target("java", "");
        dir.capture(&target);
        let pending = dir.pending(&target, "java");
        assert_eq!(
            pending.required_outputs(),
            &[pending.output_root().join("Main.class")]
        );
        let build = compile(pending).await;
        let command = build
            .run(dir.fresh_cwd("inspect-cwd"))
            .unwrap()
            .launch_command(Platform::host())
            .unwrap();
        assert_eq!(command.program.file_name().unwrap(), "java");
        assert_eq!(
            command.args,
            [
                OsString::from("-cp"),
                build.output_root().as_os_str().to_owned(),
                OsString::from("Main")
            ]
        );
        assert_eq!(run(&build, &dir, "run", "").await, b"42\n");
    });
}

#[test]
fn java_packaged_grader_uses_inner_and_auxiliary_classes_in_output_root() {
    run_test(async {
        let dir = Directory::new();
        dir.source(
            "judge/Grader.java",
            r#"package judge;
public class Grader {
    private static class Answer { int value() { return Solution.twice(21); } }
    public static void main(String[] args) { System.out.println(new Answer().value()); }
}
"#,
        );
        dir.source(
            "Solution.java",
            "package judge;\npublic class Solution { static int twice(int n) { return 2 * n; } }\n",
        );
        let target = dir.target("java", "entry = 'judge.Grader'\nsources = ['judge/Grader.java', 'Solution.java']\nsolution_files = ['Solution.java']");
        dir.capture(&target);
        let pending = dir.pending(&target, "packaged");
        assert_eq!(
            pending.required_outputs(),
            &[pending.output_root().join("judge/Grader.class")]
        );
        let build = compile(pending).await;
        for name in [
            "judge/Grader.class",
            "judge/Grader$Answer.class",
            "judge/Solution.class",
        ] {
            assert!(build.output_root().join(name).is_file(), "missing {name}");
        }
        let command = build
            .run(dir.fresh_cwd("inspect-cwd"))
            .unwrap()
            .launch_command(Platform::host())
            .unwrap();
        assert_eq!(
            command.args,
            [
                OsString::from("-cp"),
                build.output_root().as_os_str().to_owned(),
                OsString::from("judge.Grader")
            ]
        );
        assert_eq!(run(&build, &dir, "run", "").await, b"42\n");
    });
}

#[test]
fn java_zero_exit_without_required_class_cannot_validate() {
    run_test(async {
        let dir = Directory::new();
        dir.source("Main.java", MAIN);
        let target = dir.target(
            "java",
            "[config.build]\nshell = { interpreter = ['sh', '-c'], script = 'true' }",
        );
        dir.capture(&target);
        // Planning also resolves the external java consumer before this no-op
        // producer runs. Its availability cannot grant permission to run.
        let pending = dir.pending(&target, "noop");
        let required = pending.output_root().join("Main.class");
        let execution = launch(pending.build_stage().clone(), StdinSpec::Null).await;
        execution.success();
        let failure = build::finish_build(pending, (&execution.outcome).into()).unwrap_err();
        assert!(
            matches!(*failure.kind, BuildFailureKind::OutputMissing(ref path) if path == &required),
            "{failure}"
        );
    });
}

#[test]
fn java_explicit_required_list_rejects_missing_extra_class() {
    run_test(async {
        let dir = Directory::new();
        dir.source("Main.java", MAIN);
        dir.workspace(
            "[languages.java]\nartifacts = { dir = true, required = ['Main.class', 'Extra.class'] }\n",
        );
        let target = dir.target("java", "");
        dir.capture(&target);
        let pending = dir.pending(&target, "extra");
        let root = pending.output_root().to_path_buf();
        assert_eq!(
            pending.required_outputs(),
            &[root.join("Main.class"), root.join("Extra.class")]
        );
        let execution = launch(pending.build_stage().clone(), StdinSpec::Null).await;
        execution.success();
        assert!(root.join("Main.class").is_file());
        let failure = build::finish_build(pending, (&execution.outcome).into()).unwrap_err();
        assert!(
            matches!(*failure.kind, BuildFailureKind::OutputMissing(ref path) if path == &root.join("Extra.class")),
            "{failure}"
        );
    });
}

fn producer_failed(
    execution: &Execution,
    pending: PendingBuild<'_>,
) -> cpforge::build::BuildFailure {
    execution.clean();
    assert!(
        matches!(execution.outcome.status, Termination::Exited { code } if code != 0),
        "{}",
        execution.diagnostics()
    );
    assert!(
        !execution.stderr.is_empty(),
        "compiler should report the syntax error"
    );
    let failure = build::finish_build(pending, (&execution.outcome).into()).unwrap_err();
    let BuildFailureKind::ProducerFailed(outcome) = failure.kind.as_ref() else {
        panic!("expected producer failure: {failure}");
    };
    assert_eq!(outcome.termination, execution.outcome.status);
    failure
}

#[test]
fn failed_second_build_cannot_reuse_retained_first_artifacts() {
    run_test(async {
        let dir = Directory::new();
        dir.source("main.cpp", SUM);
        let target = dir.target("cpp", "");
        dir.capture(&target);
        let first = compile(dir.pending(&target, "first")).await;
        assert_eq!(run(&first, &dir, "first-run", "10 20\n").await, b"30\n");
        let artifact = first.output_root().join("main");
        let bytes = fs::read(&artifact).unwrap();
        let metadata = fs::metadata(&artifact).unwrap();

        dir.source("main.cpp", "int main( { this does not compile\n");
        dir.capture(&target);
        let second = dir.pending(&target, "second");
        assert_ne!(second.id(), first.id());
        assert_ne!(second.output_root(), first.output_root());
        assert_eq!(fs::read_dir(second.output_root()).unwrap().count(), 0);
        let execution = launch(second.build_stage().clone(), StdinSpec::Null).await;
        // finish_build consumes the pending attempt; Err carries no run recipe.
        // The compile_fail examples in build enforce that API boundary.
        let failure = producer_failed(&execution, second);
        assert_eq!(failure.id.as_str(), "second");
        assert_ne!(&failure.id, first.id());
        assert!(!failure.output_root.join("main").exists());
        assert_eq!(fs::read(&artifact).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&artifact).unwrap().modified().unwrap(),
            metadata.modified().unwrap()
        );
        assert_eq!(run(&first, &dir, "retained-run", "7 8\n").await, b"15\n");
    });
}

#[test]
fn cpp_syntax_error_preserves_nonzero_producer_exit_without_validated_build() {
    run_test(async {
        let dir = Directory::new();
        dir.source("main.cpp", "int main( { this does not compile\n");
        let target = dir.target("cpp", "");
        dir.capture(&target);
        let pending = dir.pending(&target, "syntax-error");
        let execution = launch(pending.build_stage().clone(), StdinSpec::Null).await;
        let failure = producer_failed(&execution, pending);
        assert_eq!(failure.id.as_str(), "syntax-error");
        assert!(!failure.output_root.join("main").exists());
    });
}
