#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use cpf_fixture::harness::fixture_path;
use cpforge::config::{CommandPlan, ConfigRequest, Environment, Platform, load_config};
use cpforge::exec::{
    Baseline, CommandBindings, Context, InvocationDirs, LaunchKind, NativeProbe, PreparedStage,
    ProblemBinding, Resolution, ResolveErrorKind, ResolveErrors, ResolvedStage, prepare, resolve,
};
use cpforge::supervisor::{
    CancelToken, OutputSpec, ParentReader, ProgramSpec, ReadResult, RunControls, SpawnOptions,
    StdinSpec, Termination, admit, debug_counters, init_process, spawn_root, stdout_pipe,
    wait_root,
};

// Each test owns its runtime and children through cleanup, as in supervisor_linux.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn run_test(body: impl Future<Output = ()>) {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    init_process().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
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
            .expect("execution test exceeded 10 seconds");
        });
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let mut bits = [0; 16];
        getrandom::fill(&mut bits).unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "exec-linux-{:032x} space $",
                u128::from_le_bytes(bits)
            ));
        fs::create_dir_all(root.join("cwd")).unwrap();
        // A shell glob of `*` in the stage cwd would match this file, so an
        // unexpanded `*` proves that no pathname expansion occurred.
        fs::write(root.join("cwd").join("glob-sentinel"), "").unwrap();
        Self(root)
    }

    fn load(&self, body: &str) -> Result<CommandPlan, cpforge::config::ConfigErrors> {
        let file = self.0.join("config.toml");
        fs::write(&file, format!("[commands.c]\n{body}\n")).unwrap();
        load_config(&ConfigRequest {
            environment: Environment::default(),
            platform: Platform::host(),
            current_dir: self.0.clone(),
            config_path: Some(file),
            ..ConfigRequest::default()
        })
        .command("c")
    }

    fn prepare(&self, body: &str, baseline: &Baseline, extra: &[OsString]) -> PreparedStage {
        let plan = self.load(body).unwrap();
        let ctx = Context::command(
            &plan,
            InvocationDirs {
                run_cwd: self.0.join("cwd"),
                problem: Some(ProblemBinding {
                    dir: self.0.join("problem space $p"),
                    id: "p".into(),
                }),
                workspace: Some(self.0.join("workspace space $w")),
            },
            CommandBindings::default(),
        )
        .unwrap();
        prepare(&plan.stage, &ctx, baseline, Platform::host(), extra).unwrap()
    }

    fn resolve(
        &self,
        body: &str,
        baseline: &Baseline,
        extra: &[OsString],
    ) -> Result<ResolvedStage, ResolveErrors> {
        let prepared = self.prepare(body, baseline, extra);
        match resolve(prepared, &NativeProbe, Platform::host())? {
            Resolution::Resolved(stage) => Ok(stage),
            Resolution::AwaitingProducer(_) => {
                panic!("external test program must resolve immediately")
            }
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn quoted(value: &str) -> String {
    toml_edit::Value::from(value).to_string()
}

fn array(values: &[&str]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| quoted(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

const TRICKY: &[&str] = &[
    "a b",
    "\"q\"",
    "back\\slash",
    "$HOME",
    "a;b|c&d>e<f",
    "*",
    "'single'",
    "",
    "line1\nline2",
    "{{x}}",
];
const EXTRA: &[&str] = &["{sources}", "--x=$y"];

fn extras() -> Vec<OsString> {
    EXTRA.iter().map(|value| OsString::from(*value)).collect()
}

fn nul_bytes(values: &[&str]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.bytes().chain([0]))
        .collect()
}

fn expected_tricky(shell: bool) -> Vec<u8> {
    let mut values = Vec::new();
    if shell {
        values.push("cpf");
    }
    values.extend_from_slice(&TRICKY[..TRICKY.len() - 1]);
    values.push("{x}");
    values.extend_from_slice(EXTRA);
    nul_bytes(&values)
}

fn argv(args: &[&str]) -> String {
    let fixture = fixture_path();
    let mut elements = vec![fixture.to_str().unwrap(), "echo-argv"];
    elements.extend_from_slice(args);
    format!("argv = {}", array(&elements))
}

fn shell(script: &str, args: &[&str]) -> String {
    format!(
        "shell = {{ interpreter = ['sh', '-c'], script = {}, args = {} }}",
        quoted(script),
        array(args)
    )
}

fn shell_baseline() -> Baseline {
    Baseline::from_entries(vec![("PATH".into(), "/bin:/usr/bin".into())])
}

async fn drain(mut reader: ParentReader) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match reader.read_chunk(&mut buffer).await {
            ReadResult::Data(n) => bytes.extend_from_slice(&buffer[..n]),
            ReadResult::Eof => return bytes,
            ReadResult::Failed(error) => panic!("stdout read failed: {error}"),
        }
    }
}

async fn launch(stage: ResolvedStage) -> Vec<u8> {
    let command = stage.launch_command(Platform::host()).unwrap();
    assert_eq!(command.launch_kind, LaunchKind::Native);
    let pipe = stdout_pipe().unwrap();
    // Test-only bridge. Job admission will supply CPF_*, sinks and normalized
    // limits when it owns the conversion to the supervisor's ProgramSpec.
    let spec = ProgramSpec {
        program: command.program,
        args: command.args,
        cwd: stage.cwd,
        env: stage.environment,
        stdin: StdinSpec::Null,
        stdout: OutputSpec::Pipe(pipe.child),
        stderr: OutputSpec::Null,
        limits: Vec::new(),
        measure: Vec::new(),
    };
    let root = spawn_root(spec, admit().unwrap(), &SpawnOptions::default()).unwrap();
    let (bytes, outcome) = tokio::join!(
        drain(pipe.parent),
        wait_root(
            root,
            RunControls {
                wall_deadline: Duration::from_secs(2),
                cancel: CancelToken::new(),
                events: None,
            }
        )
    );
    assert_eq!(outcome.status, Termination::Exited { code: 0 });
    assert!(outcome.errors.is_empty(), "{outcome:?}");
    assert!(outcome.violations.is_empty(), "{outcome:?}");
    bytes
}

#[test]
fn argv_reaches_fixture_byte_exactly_with_verbatim_cli_extras() {
    run_test(async {
        let dir = Directory::new();
        let stage = dir
            .resolve(&argv(TRICKY), &Baseline::default(), &extras())
            .unwrap();
        assert_eq!(launch(stage).await, expected_tricky(false));
    });
}

#[test]
fn shell_positional_arguments_and_zero_reach_printf_byte_exactly() {
    run_test(async {
        let dir = Directory::new();
        let body = shell(r#"printf "%s\0" "$0" "$@""#, TRICKY);
        let stage = dir.resolve(&body, &shell_baseline(), &extras()).unwrap();
        assert_eq!(launch(stage).await, expected_tricky(true));
    });
}

#[test]
fn shell_script_braces_stay_literal_through_the_entire_chain() {
    run_test(async {
        let dir = Directory::new();
        let stage = dir
            .resolve(&shell("printf '{x}'", &[]), &shell_baseline(), &[])
            .unwrap();
        assert_eq!(launch(stage).await, b"{x}");
    });
}

#[test]
fn path_placeholders_with_spaces_and_dollars_are_single_elements() {
    run_test(async {
        let dir = Directory::new();
        let problem = dir.0.join("problem space $p");
        let workspace = dir.0.join("workspace space $w");
        let expected = nul_bytes(&[problem.to_str().unwrap(), workspace.to_str().unwrap()]);
        for body in [
            argv(&["{problem_dir}", "{workspace}"]),
            shell(r#"printf "%s\0" "$@""#, &["{problem_dir}", "{workspace}"]),
        ] {
            let stage = dir.resolve(&body, &shell_baseline(), &[]).unwrap();
            assert_eq!(launch(stage).await, expected);
        }
    });
}

#[test]
fn environment_values_remain_literal_and_removed_baseline_names_are_absent() {
    run_test(async {
        let dir = Directory::new();
        let invalid = format!("{}\nenv = {{ CPF_TEST_V = 'x' }}", argv(&[]));
        let error = dir.load(&invalid).unwrap_err().to_string();
        assert!(
            error.contains("CPF_TEST_V") && error.contains("reserved"),
            "{error}"
        );
        let body = format!(
            "{}\nenv = {{ ZZ_TEST = 'a b$c' }}\nenv_remove = ['ZZ_REMOVED']",
            shell(r#"printf '%s\0%s' "$ZZ_TEST" "${ZZ_REMOVED-unset}""#, &[])
        );
        let baseline = Baseline::from_entries(vec![
            ("PATH".into(), "/bin:/usr/bin".into()),
            ("ZZ_TEST".into(), "baseline".into()),
            ("ZZ_REMOVED".into(), "must disappear".into()),
        ]);
        let stage = dir.resolve(&body, &baseline, &[]).unwrap();
        assert_eq!(launch(stage).await, b"a b$c\0unset");
    });
}

#[test]
fn stage_path_selects_tool_and_missing_path_stops_before_launch() {
    run_test(async {
        let dir = Directory::new();
        let bin = dir.0.join("tools space $t");
        let empty = dir.0.join("empty");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&empty).unwrap();
        let tool = bin.join("mytool");
        fs::write(&tool, "#!/bin/sh\nprintf ok\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let body = format!(
            "argv = ['mytool']\nenv = {{ PATH = {} }}",
            quoted(bin.to_str().unwrap())
        );
        let baseline =
            Baseline::from_entries(vec![("PATH".into(), empty.clone().into_os_string())]);
        let stage = dir.resolve(&body, &baseline, &[]).unwrap();
        assert_eq!(
            stage.launch_command(Platform::host()).unwrap().program,
            tool
        );
        assert_eq!(launch(stage).await, b"ok");

        // Keep the tool present, but exclude its directory from stage PATH.
        let missing = format!(
            "argv = ['mytool']\nenv = {{ PATH = {} }}",
            quoted(empty.to_str().unwrap())
        );
        let errors = dir.resolve(&missing, &baseline, &[]).unwrap_err();
        assert_eq!(errors.errors[0].kind, ResolveErrorKind::NotFound);
        assert_eq!(
            errors.errors[0].trace.skipped_candidates[0].path,
            empty.join("mytool")
        );
    });
}
