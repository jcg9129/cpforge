//! File-I/O configuration, precedence, and commands that ignore the declaration.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SHARED_CWD: &str = "file-based I/O needs the run's own directory";
const TEST_REFUSAL: &str = "file-I/O test execution is not supported in this version";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "cli-file-io-{}",
                cpforge::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&root).unwrap();
        let fixture = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(root).unwrap(),
        ));
        fixture.write("cpforge.toml", "schema = 1\n");
        fixture.write("user/cpforge/config.toml", "schema = 1\n");
        fixture.write(
            ".cpforge/job-seq",
            "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
        );
        fixture.problem("");
        fixture
    }

    fn path(&self, relative: &str) -> PathBuf {
        relative
            .split('/')
            .fold(self.0.clone(), |path, component| path.join(component))
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn problem(&self, config: &str) {
        self.write(
            "problems/A/problem.toml",
            &format!("schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\n{config}"),
        );
    }

    fn invoke(&self, args: &[&str], status: i32) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(&self.0)
            .env("HOME", self.path("user"))
            .env("USERPROFILE", self.path("user"))
            .env("XDG_CONFIG_HOME", self.path("user"))
            .env("APPDATA", self.path("user"))
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .env_remove("CPF_HOOK_DEPTH")
            .stdin(Stdio::null())
            .args(["--problem", "A"])
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(status), "{args:?}: {output:?}");
        output
    }

    fn run(&self, args: &[&str], status: i32) -> Output {
        let sequence = fs::read(self.path(".cpforge/job-seq")).unwrap();
        let output = self.invoke(args, status);
        assert_eq!(fs::read(self.path(".cpforge/job-seq")).unwrap(), sequence);
        assert!(
            !self.path(".cpforge/runs").exists(),
            "job admitted: {args:?}"
        );
        assert!(
            !self.path(".cpforge/lock").exists(),
            "lock acquired: {args:?}"
        );
        output
    }

    fn diagnostic(&self, output: &Output, key: &str, message: &str) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let file = self.path("problems/A/problem.toml");
        for expected in [file.to_str().unwrap(), key, message] {
            assert!(stderr.contains(expected), "missing {expected:?}: {stderr}");
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn shared_cwd_refuses_all_problem_commands_before_admission() {
    let fixture = Fixture::new();
    fixture.write(
        "cpforge.toml",
        "schema = 1\n[commands.inspect]\nargv = ['unused']\n",
    );
    for declaration in [
        "io = { input = 'in.txt' }\n",
        "io = { output = 'out.txt' }\n",
        "io = { input = 'in.txt', output = 'out.txt' }\n",
    ] {
        for cwd in [
            "{ selector = 'problem' }",
            "{ selector = 'workspace' }",
            "{ path = 'x' }",
        ] {
            fixture.problem(&format!("{declaration}[config.run]\ncwd = {cwd}\n"));
            for args in [
                vec!["test"],
                vec!["build"],
                vec!["run"],
                vec!["cmd", "inspect"],
                vec!["config", "check"],
                vec!["config", "show"],
                vec!["doctor"],
            ] {
                let output = fixture.run(&args, 7);
                fixture.diagnostic(&output, "config.io", SHARED_CWD);
                assert!(String::from_utf8_lossy(&output.stderr).contains("config.run.cwd"));
            }
            let output = fixture.run(&["test", "--json"], 7);
            let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(doc["outcome"], "refused");
            assert_eq!(doc["refusal"]["class"], "config");
        }
    }
}

#[test]
fn valid_declarations_are_inspectable_with_literal_names_and_origins() {
    let fixture = Fixture::new();
    for fields in [
        "input = 'input'",
        "output = 'output'",
        "input = 'in.txt', output = 'out.txt'",
        "input = '{literal}.in'",
    ] {
        fixture.problem(&format!("io = {{ {fields} }}\n"));
        fixture.run(&["config", "check"], 0);
        let output = fixture.run(&["config", "show", "--origin"], 0);
        let human = String::from_utf8_lossy(&output.stdout);
        let file = fixture.path("problems/A/problem.toml");
        for key in ["input", "output"] {
            let prefix = format!("config.io.{key} = ");
            let line = human.lines().find(|line| line.starts_with(&prefix));
            assert_eq!(
                line.is_some(),
                fields.contains(&format!("{key} =")),
                "{key}: {human}"
            );
            if let Some(line) = line {
                assert!(line.contains(file.to_str().unwrap()), "{line}");
                assert!(line.contains(&format!("[config.io.{key}]")), "{line}");
            }
        }
        let output = fixture.run(&["config", "show", "--json", "--origin"], 0);
        let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let io = doc["config"]["config"]["io"].as_object().unwrap();
        assert_eq!(
            io.len(),
            usize::from(fields.contains("input =")) + usize::from(fields.contains("output ="))
        );
        for key in ["input", "output"] {
            if let Some(value) = io.get(key) {
                assert!(value.is_string(), "{doc}");
                let origin = doc["origins"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|o| o["path"] == serde_json::json!(["config", "io", key]))
                    .unwrap();
                assert_eq!(origin["origin"]["kind"], "file");
                let path: serde_json::Value =
                    serde_json::from_slice(&cpforge::report::wire::encode_native(
                        fixture.path("problems/A/problem.toml").as_os_str(),
                    ))
                    .unwrap();
                assert_eq!(origin["origin"]["path"], path);
                assert_eq!(origin["origin"]["line"], 6);
                assert!(origin["origin"]["col"].is_u64());
            }
        }
        if fields.contains("{literal}") {
            assert_eq!(io["input"], "{literal}.in");
            assert!(
                !doc["deferred"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v["path"] == serde_json::json!(["config", "io", "input"]))
            );
        }
    }
    fixture.write(
        "problems/A/problem.toml",
        "schema = 1\n[problem]\nid = 'local:A'\n[config]\nio = { input = 'in.txt' }\n",
    );
    fixture.run(&["config", "check"], 0);
}

#[test]
fn profile_only_shared_cwd_is_checked_when_selected_and_by_config_check() {
    let fixture = Fixture::new();
    for cwd in [
        "{ selector = 'problem' }",
        "{ selector = 'workspace' }",
        "{ path = 'x' }",
    ] {
        fixture.problem("io = { input = 'in.txt' }\n");
        fixture.write(
            "cpforge.toml",
            &format!("schema = 1\n[languages.cpp.profiles.shared.run]\ncwd = {cwd}\n"),
        );
        let output = fixture.run(&["config", "show"], 0);
        assert!(!String::from_utf8_lossy(&output.stderr).contains(SHARED_CWD));
        for args in [
            vec!["build", "--profile", "shared"],
            vec!["run", "--profile", "shared"],
            vec!["test", "--profile", "shared"],
            vec!["config", "show", "--profile", "shared"],
            vec!["config", "check"],
        ] {
            let output = fixture.run(&args, 7);
            fixture.diagnostic(&output, "config.io", SHARED_CWD);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("languages.cpp.profiles.shared.run.cwd"),
                "{stderr}"
            );
            assert!(
                stderr.contains(fixture.path("cpforge.toml").to_str().unwrap()),
                "{stderr}"
            );
        }
        let output = fixture.run(&["config", "show", "--profile", "shared", "--json"], 7);
        let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(doc["class"], "config");
        assert_eq!(doc["diagnostics"][0]["key"], "config.io");
        // With no profile selected the effective run is valid: config check
        // accepts it, and the test stops at the execution-support refusal.
        let output = fixture.run(&["test"], if cfg!(windows) { 10 } else { 7 });
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(SHARED_CWD), "{stderr}");
        assert!(
            stderr.contains(if cfg!(windows) {
                "unsupported platform"
            } else {
                TEST_REFUSAL
            }),
            "{stderr}"
        );
        fixture.problem("io = { input = 'in.txt' }\n[config.run]\ncwd = { selector = 'run' }\n");
        fixture.run(&["config", "check"], 0);
    }
}

// F-28: phase-2 I/O refusals precede any platform or execution refusal.
#[test]
fn interactive_and_stress_file_io_refuse_before_admission() {
    let fixture = Fixture::new();
    for declarations in [
        "schema = 1\n[problem]\nid = 'local:A'\nkind = 'interactive'\n[config]\nlanguage = 'cpp'\nio = { input = 'in.txt' }\n",
        "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\nio = { output = 'out.txt' }\n[check]\nkind = 'interactive'\n[check.interactive]\nchecking = 'interactor'\ninteractor = { program = 'judge', profile = 'simple' }\n",
    ] {
        fixture.write("problems/A/problem.toml", declarations);
        for args in [
            vec!["test"],
            vec!["build"],
            vec!["run"],
            vec!["config", "show"],
            vec!["config", "check"],
        ] {
            let output = fixture.run(&args, 7);
            fixture.diagnostic(
                &output,
                "config.io",
                "file I/O is not allowed for interactive problems",
            );
        }
    }
    fixture.problem("io = { input = 'in.txt' }\n");
    let output = fixture.run(&["stress"], 7);
    fixture.diagnostic(
        &output,
        "config.io",
        "stress with file-based I/O is unsupported in this version",
    );
    // Without `io`, and with only an unrelated configuration error, stress
    // keeps its ordinary unfinished-surface refusal.
    for config in ["", "[config.run]\ncwd = 1\n"] {
        fixture.problem(config);
        let output = fixture.run(&["stress"], 2);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("cpf stress: not available yet"),
            "{output:?}"
        );
    }
}

#[test]
fn config_check_rejects_file_io_in_workspace_and_user_layers() {
    let fixture = Fixture::new();
    for file in ["cpforge.toml", "user/cpforge/config.toml"] {
        fixture.write(file, "io = { input = 'task.in' }\n");
        let output = fixture.run(&["config", "check"], 7);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(fixture.path(file).to_str().unwrap()),
            "{stderr}"
        );
        assert!(stderr.contains("[io]"), "{stderr}");
        assert!(stderr.contains("unknown configuration key"), "{stderr}");
        assert!(!stderr.contains(SHARED_CWD), "{stderr}");
        fixture.write(file, "schema = 1\n");
    }
}

#[cfg(windows)]
#[test]
fn valid_config_on_windows_still_refuses_execution_before_admission() {
    let fixture = Fixture::new();
    fixture.problem("io = { input = 'in.txt', output = 'out.txt' }\n");
    fixture.run(&["config", "show"], 0);
    fixture.run(&["config", "check"], 0);
    for command in ["test", "build", "run"] {
        let output = fixture.run(&[command], 10);
        assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported platform"));
    }
}

#[test]
fn config_check_reports_invalid_file_io_without_the_phase_two_rejection() {
    let fixture = Fixture::new();
    for value in [
        "{ input = 'a/b' }",
        "{ output = 'a\\b' }",
        "{ input = '..' }",
        "{ input = 'a:b' }",
        "{ input = 'NUL.txt' }",
        "{ input = 'trailing.' }",
        "{ output = 'trailing ' }",
        "{}",
    ] {
        fixture.problem(&format!("io = {value}\n"));
        let output = fixture.run(&["config", "check"], 7);
        fixture.diagnostic(
            &output,
            "config.io",
            if value == "{}" {
                "io requires at least one of input or output"
            } else {
                "expected a portable file-I/O name"
            },
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains(SHARED_CWD));
    }
}

#[test]
fn portable_collision_keys_and_byte_limits_are_enforced_before_admission() {
    let fixture = Fixture::new();
    for (input, output) in [
        ("same", "same"),
        ("a.txt", "A.txt"),
        ("résultat.txt", "re\u{301}sultat.txt"),
        ("ß", "ss"),
    ] {
        fixture.problem(&format!(
            "io = {{ input = '{input}', output = '{output}' }}\n"
        ));
        for args in [vec!["config", "check"], vec!["test"], vec!["stress"]] {
            let output = fixture.run(&args, 7);
            fixture.diagnostic(
                &output,
                "config.io",
                "name the same file on some filesystems",
            );
            assert!(!String::from_utf8_lossy(&output.stderr).contains(SHARED_CWD));
        }
    }
    let name = format!("{}a", "é".repeat(127));
    fixture.problem(&format!("io = {{ input = '{name}' }}\n"));
    fixture.run(&["config", "check"], 0);
    fixture.problem(&format!("io = {{ input = '{name}b' }}\n"));
    fixture.diagnostic(
        &fixture.run(&["config", "check"], 7),
        "config.io.input",
        "maximum is 255",
    );
}

// Keep the partial execution boundary explicit: inspection succeeds, but a
// test must not be admitted until the file capture stream can honor its io.
#[test]
fn file_io_test_execution_is_refused_without_admitting_a_job() {
    let fixture = Fixture::new();
    for fields in [
        "input = 'in.txt'",
        "output = 'out.txt'",
        "input = 'in.txt', output = 'out.txt'",
    ] {
        fixture.problem(&format!("io = {{ {fields} }}\n"));
        fixture.run(&["config", "check"], 0);
        fixture.run(&["config", "show"], 0);
        if cfg!(windows) {
            let output = fixture.run(&["test", "--json"], 10);
            let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(doc["outcome"], "refused");
            assert_eq!(doc["refusal"]["class"], "infrastructure");
            assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported platform"));
            continue;
        }
        let output = fixture.run(&["test"], 7);
        fixture.diagnostic(&output, "config.io", TEST_REFUSAL);
        let output = fixture.run(&["test", "--json"], 7);
        let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(doc["outcome"], "refused");
        assert_eq!(doc["refusal"]["class"], "config");
        assert_eq!(doc["refusal"]["diagnostics"][0]["key"], "config.io");
        assert_eq!(doc["refusal"]["diagnostics"][0]["message"], TEST_REFUSAL);
    }
}

fn run_test<F: std::future::Future<Output = ()>>(body: impl FnOnce() -> F + Send) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn_scoped(scope, move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(body());
            })
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}

// F-26: every command keeps its ordinary stdin, fresh cwd and report shape.
#[test]
fn build_raw_run_and_commands_ignore_file_io() {
    run_test(move || async move {
        for args in [vec!["build"], vec!["run"], vec!["cmd", "inspect"]] {
            let fixture = Fixture::new();
            fixture.problem("io = { input = 'in.txt', output = 'out.txt' }\n");
            fixture.write("problems/A/main.cpp", r#"#include <filesystem>
#include <iostream>
int main() {
    if (std::filesystem::directory_iterator(".") != std::filesystem::directory_iterator()) return 20;
    unsigned n = 0;
    char c;
    while (std::cin.get(c)) ++n;
    std::cout << n << '\n';
}
"#);
            fixture.write("problems/A/tests.toml", "schema = 1\n[[test]]\nid = 'one'\ninput = 'tests/one.in'\nanswer = 'tests/one.ans'\norigin = 'custom'\nenabled = true\n");
            fixture.write("problems/A/tests/one.in", "frozen input\n");
            fixture.write("problems/A/tests/one.ans", "answer\n");
            fixture.write(
                "cpforge.toml",
                r#"schema = 1
[build]
deadline_s = 90
[commands.inspect]
for_each_test = true
stdin = "test"
argv = ['sh', '-c', 'test -z "$(ls -A .)" && cat', 'cpf']
"#,
            );
            #[cfg(target_os = "macos")]
            fixture.write("user/cpforge/config.toml", "schema = 1\n[languages.cpp.build]\nargv = ['g++-14', '-std=gnu++23', '-O2', '-Wall', '{sources}', '-o', '{artifact}']\n");
            fixture.run(&["config", "check"], 0);
            let output = fixture.run(&["config", "show", "--json"], 0);
            let inspection: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(inspection["config"]["config"]["io"]["input"], "in.txt");
            if cfg!(windows) {
                fixture.run(&args, 10);
                continue;
            }
            let output = fixture.invoke(&args, 0);
            if args[0] == "run" {
                assert_eq!(output.stdout, b"0\n");
            }
            let jobs: Vec<_> = fs::read_dir(fixture.path(".cpforge/runs"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert_eq!(jobs.len(), 1);
            let job = &jobs[0];
            let doc: serde_json::Value =
                serde_json::from_slice(&fs::read(job.join("report.json")).unwrap()).unwrap();
            assert!(doc["target"].is_null() || doc["target"]["io"].is_null());
            assert_eq!(doc["result"]["pre_delivery_exit"]["code"], 0);
            let executions = doc["executions"].as_array().unwrap();
            assert!(!executions.is_empty());
            assert_eq!(
                doc["result"]["counts"]["primary"]["planned"],
                executions.len() as u64
            );
            assert_eq!(
                doc["result"]["counts"]["primary"]["started"],
                executions.len() as u64
            );
            for member in [
                "not_started",
                "skipped",
                "transferred",
                "admitted",
                "invalid",
            ] {
                assert_eq!(doc["result"]["counts"]["primary"][member], 0);
            }
            for execution in executions {
                assert_eq!(execution["coverage"]["kind"], "started");
                assert_eq!(execution["summary"]["class"]["kind"], "exited_zero");
                let cwd = job.join(execution["run"].as_str().unwrap()).join("cwd");
                assert_eq!(fs::read_dir(cwd).unwrap().count(), 0);
            }
            let tests = doc["tests"].as_array().unwrap();
            assert_eq!(doc["result"]["counts"]["tests"], 0);
            assert_eq!(doc["result"]["verdict"], "succeeded");
            assert_eq!(
                doc["result"]["counts"]["by_class"]["exited_zero"],
                executions.len() as u64
            );
            assert!(
                doc["result"]["counts"]["by_verdict"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|count| count == 0)
            );
            if args[0] == "cmd" {
                assert_eq!(tests.len(), 1);
                let test = &tests[0];
                for member in ["check", "verdict", "class", "output_file"] {
                    assert!(test[member].is_null(), "{member}: {doc}");
                }
                assert_eq!(test["executions"]["first"], 0);
                assert_eq!(test["executions"]["count"], 1);
                let capture = job
                    .join(executions[0]["run"].as_str().unwrap())
                    .join("out")
                    .join("stdout");
                assert_eq!(fs::read(capture).unwrap(), b"frozen input\n");
            } else {
                assert!(tests.is_empty());
            }
        }
    });
}
