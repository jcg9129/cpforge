//! File-I/O configuration, precedence, and commands that ignore the declaration.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SHARED_CWD: &str = "file-based I/O needs the run's own directory";

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
        self.invoke_with_env(args, status, &[])
    }

    fn invoke_with_env(&self, args: &[&str], status: i32, settings: &[(&str, &str)]) -> Output {
        let output = self.command(args, settings).output().unwrap();
        assert_eq!(output.status.code(), Some(status), "{args:?}: {output:?}");
        output
    }

    fn command(&self, args: &[&str], settings: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
        command
            .current_dir(&self.0)
            .env("HOME", self.path("user"))
            .env("USERPROFILE", self.path("user"))
            .env("XDG_CONFIG_HOME", self.path("user"))
            .env("APPDATA", self.path("user"))
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .env_remove("CPF_HOOK_DEPTH")
            .env_remove("CPF_FILE_IO_FAULTS")
            .env_remove("CPF_FILE_IO_READ_SIZE")
            .env_remove("CPF_FILE_IO_READ_CHUNK")
            .env_remove("CPF_FILE_IO_LISTING_POSITION")
            .env_remove("CPF_FILE_IO_GROW_STDOUT")
            .env_remove("CPF_FILE_IO_LOG")
            .env_remove("CPF_FILE_IO_UNVERIFIED")
            .envs(settings.iter().copied())
            .stdin(Stdio::null())
            .args(["--problem", "A"])
            .args(args);
        command
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
        // Without the shared profile, the effective run remains valid.
        fixture.run(&["config", "show"], 0);
        #[cfg(windows)]
        fixture.run(&["test"], 10);
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
#include <fstream>
#include <iostream>
int main() {
    if (std::filesystem::exists("in.txt")) {
        std::ifstream input("in.txt"); std::string text; std::getline(input, text);
        std::ofstream output("out.txt"); output << (text == "frozen input" ? "answer\n" : "bad\n");
        return 0;
    }
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
                fixture.run(&["test", "--json"], 10);
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
            let (test_job, judged) = fixture.test_report(0).unwrap();
            assert_job(
                &judged,
                serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
            );
            assert_test(
                &judged,
                "one",
                "passed",
                "passed",
                "exited_zero",
                Some("collected"),
            );
            assert_collected(&test_job, test_entry(&judged, "one"), b"answer\n");
        }
    });
}

// A single compiled solution exercises file shapes without invoking a shell.
const FILE_SOLUTION: &str = r#"#include <fstream>
#include <iostream>
#include <filesystem>
#include <string>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>
int main() {
    std::ifstream staged("in.txt");
    std::istream& input = staged ? static_cast<std::istream&>(staged) : std::cin;
    std::string action;
    input >> action;
    auto write = [](const std::string& name, const std::string& bytes) {
        std::ofstream file(name, std::ios::binary); file << bytes;
    };
    if (action == "stdin") {
        std::string bytes; char c; while (std::cin.get(c)) bytes += c;
        write("out.txt", bytes); return 0;
    }
    if (action == "missing" || action == "exit") return action == "exit" ? 3 : 0;
    if (action == "stdout") { std::cout << "42\n"; return 0; }
    if (action == "case") { write("OUT.TXT", "42\n"); return 0; }
    if (action == "nfd") { write("re\xCC\x81sultat.txt", "42\n"); return 0; }
    if (action == "link") { write("answer", "42\n"); if (symlink("answer", "out.txt")) return 22; return 0; }
    if (action == "hosts") { if (symlink("/etc/hosts", "out.txt")) return 22; return 0; }
    if (action == "pipe-link") { mkfifo("pipe", 0600); if (symlink("pipe", "out.txt")) return 22; return 0; }
    if (action == "directory") { std::filesystem::create_directory("out.txt"); return 0; }
    if (action == "fifo") { mkfifo("out.txt", 0600); return 0; }
    if (action == "socket") {
        int fd = socket(AF_UNIX, SOCK_STREAM, 0); sockaddr_un address{};
        address.sun_family = AF_UNIX; std::string("out.txt").copy(address.sun_path, 7);
        if (bind(fd, reinterpret_cast<sockaddr*>(&address), sizeof(address))) return 21;
        close(fd); return 0;
    }
    if (action == "case-link") { if (symlink("/etc/hosts", "OUT.TXT")) return 22; return 0; }
    if (action == "case-directory") { std::filesystem::create_directory("OUT.TXT"); return 0; }
    if (action == "case-unreadable") { write("OUT.TXT", "42\n"); chmod("OUT.TXT", 0000); return 0; }
    if (action == "unreadable") { write("out.txt", "42\n"); chmod("out.txt", 0000); return 0; }
    if (action == "cwd-unreadable") { write("out.txt", "42\n"); chmod(".", 0000); return 0; }
    if (action == "many") {
        // Four inodes keep link counts below even HFS+'s per-file limit.
        // Hard links avoid allocating 65,537 separate inodes in CI.
        for (unsigned i = 0; i < 4; ++i) write("seed" + std::to_string(i), "");
        pid_t workers[4]{};
        for (unsigned worker = 0; worker < 4; ++worker) {
            workers[worker] = fork();
            if (workers[worker] < 0) return 24;
            if (workers[worker] == 0) {
                for (unsigned i = worker; i < 65533; i += 4) {
                    if (link(("seed" + std::to_string(worker)).c_str(), ("e" + std::to_string(i)).c_str())) _exit(23);
                }
                _exit(0);
            }
        }
        for (pid_t worker : workers) {
            int status = 0;
            if (waitpid(worker, &status, 0) != worker || !WIFEXITED(status) || WEXITSTATUS(status)) return 25;
        }
        return 0;
    }
    if (action == "replace-cwd") {
        write("out.txt", "42\n");
        std::filesystem::rename("../cwd", "../old");
        std::filesystem::create_directory("../cwd");
        write("../cwd/out.txt", "wrong\n");
        return 0;
    }
    if (action == "size") { unsigned n; input >> n; write("out.txt", std::string(n, 'x')); return 0; }
    if (action == "empty") { write("out.txt", ""); return 0; }
    if (action == "input-only") { std::cout << "42\n"; return 0; }
    if (action == "overwrite") { write("in.txt", "changed"); }
    if (action == "delete") { std::filesystem::remove("in.txt"); }
    if (action == "replace-directory") { std::filesystem::remove("in.txt"); std::filesystem::create_directory("in.txt"); }
    if (action == "replace-link") { std::filesystem::remove("in.txt"); if (symlink("/etc/hosts", "in.txt")) return 22; }
    if (action == "rename") { std::filesystem::rename("in.txt", "x.txt"); }
    write("out.txt", "42\n");
    if (action == "both") std::cout << "42\n";
    if (action == "noise") std::cout << std::string(1048576, 'z');
    if (action == "overflow") write("other", std::string(2097152, 'z'));
    if (action == "kill" || action == "discard") std::cout << std::string(8192, 'z');
    if (action == "echo") { unsigned n = 0; char c; while (std::cin.get(c)) ++n; std::cerr << n; }
}
"#;

impl Fixture {
    fn executable_problem(&self, io: &str, config: &str) {
        self.problem(&format!("io = {{ {io} }}\n{config}"));
        self.write("problems/A/main.cpp", FILE_SOLUTION);
        self.write("cpforge.toml", "schema = 1\n[build]\ndeadline_s = 90\n");
        #[cfg(target_os = "macos")]
        self.write("user/cpforge/config.toml", "schema = 1\n[languages.cpp.build]\nargv = ['g++-14', '-std=gnu++23', '-O2', '-Wall', '{sources}', '-o', '{artifact}']\n");
    }

    fn cases(&self, cases: &[(&str, &str, Option<&str>)]) {
        let mut catalog = String::from("schema = 1\n");
        for (id, input, answer) in cases {
            catalog.push_str(&format!("[[test]]\nid = '{id}'\ninput = 'tests/{id}.in'\norigin = 'custom'\nenabled = true\n"));
            self.write(&format!("problems/A/tests/{id}.in"), input);
            if let Some(answer) = answer {
                catalog.push_str(&format!("answer = 'tests/{id}.ans'\n"));
                self.write(&format!("problems/A/tests/{id}.ans"), answer);
            }
        }
        self.write("problems/A/tests.toml", &catalog);
    }

    fn test_report(&self, status: i32) -> Option<(PathBuf, serde_json::Value)> {
        self.test_report_with_env(status, &[])
    }

    fn test_report_with_env(
        &self,
        status: i32,
        settings: &[(&str, &str)],
    ) -> Option<(PathBuf, serde_json::Value)> {
        if cfg!(windows) {
            let output = self.run(&["test", "--json"], 10);
            let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(doc["outcome"], "refused");
            assert_eq!(doc["refusal"]["class"], "infrastructure");
            return None;
        }
        let output = self.invoke_with_env(&["test", "--json"], status, settings);
        let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        cpforge::report::wire::read::read_json(&output.stdout).unwrap();
        assert_eq!(doc["outcome"], "finished", "{doc}");
        assert_eq!(doc["result"]["pre_delivery_exit"]["code"], status);
        let relative = doc["job"]["dir"].as_str().unwrap();
        Some((self.path(relative), doc))
    }
}

fn test_entry<'a>(doc: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    doc["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap()
}

// Each real row uses these assertions: verdict/check/class, file outcome,
// applied I/O, all execution coverage, aggregate counts and process status.
fn assert_test(
    doc: &serde_json::Value,
    id: &str,
    verdict: &str,
    check: &str,
    class: &str,
    output: Option<&str>,
) {
    let test = test_entry(doc, id);
    assert_eq!(test["verdict"]["kind"], verdict, "{id}: {doc}");
    assert_eq!(test["check"]["kind"], check, "{id}: {doc}");
    assert_eq!(test["class"]["kind"], class, "{id}: {doc}");
    if let Some(output) = output {
        assert_eq!(test["output_file"]["kind"], output, "{id}: {doc}");
    } else {
        assert!(test["output_file"].is_null(), "{id}: {doc}");
    }
    let first = test["executions"]["first"].as_u64().unwrap() as usize;
    let count = test["executions"]["count"].as_u64().unwrap() as usize;
    assert!(count > 0);
    for execution in &doc["executions"].as_array().unwrap()[first..first + count] {
        if execution["coverage"]["kind"] == "skipped" {
            assert_eq!(
                execution["coverage"]["reason"], "not_eligible",
                "{id}: {doc}"
            );
            assert_ne!(output, Some("collected"));
        } else {
            assert_eq!(execution["coverage"]["kind"], "started", "{id}: {doc}");
        }
    }
}

fn assert_job(doc: &serde_json::Value, io: serde_json::Value) {
    assert_eq!(doc["target"]["io"], io);
    let tests = doc["tests"].as_array().unwrap();
    let executions = doc["executions"].as_array().unwrap();
    let counts = &doc["result"]["counts"];
    assert_eq!(counts["tests"], tests.len() as u64);
    assert_eq!(counts["primary"]["planned"], executions.len() as u64);
    for key in [
        "started",
        "not_started",
        "transferred",
        "skipped",
        "admitted",
        "invalid",
    ] {
        let actual = executions
            .iter()
            .filter(|e| e["coverage"]["kind"] == key)
            .count();
        assert_eq!(counts["primary"][key], actual as u64, "{doc}");
    }
    for (verdict, count) in counts["by_verdict"].as_object().unwrap() {
        let actual = tests
            .iter()
            .filter(|t| t["verdict"]["kind"] == verdict.as_str())
            .count();
        assert_eq!(*count, serde_json::json!(actual), "{doc}");
    }
    for (class, count) in counts["by_class"].as_object().unwrap() {
        let actual = executions
            .iter()
            .filter(|e| match class.as_str() {
                "skipped" => e["coverage"]["kind"] == "skipped",
                "unknown" => e["summary"].is_null() && e["coverage"]["kind"] != "skipped",
                _ => e["summary"]["class"]["kind"] == class.as_str(),
            })
            .count();
        assert_eq!(*count, serde_json::json!(actual), "{doc}");
    }
}

fn assert_collected(job: &Path, test: &serde_json::Value, bytes: &[u8]) {
    use sha2::{Digest, Sha256};
    let file = &test["output_file"];
    assert_eq!(file["bytes"], bytes.len().to_string());
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    assert_eq!(file["sha256"], {
        use std::fmt::Write;
        let mut hex = String::new();
        for byte in digest {
            write!(&mut hex, "{byte:02x}").unwrap();
        }
        hex
    });
    let relative = file["path"].as_str().unwrap();
    let path = relative
        .split('/')
        .fold(job.to_path_buf(), |p, c| p.join(c));
    assert_eq!(fs::read(path).unwrap(), bytes);
}

// F-01–F-04: the declaration selects stdin and the judged stream independently.
#[test]
fn file_input_output_and_stdin_modes() {
    run_test(move || async move {
        for (io, action, input, output, stdin, verdict) in [
            (
                "input = 'in.txt', output = 'out.txt'",
                "echo",
                Some("in.txt"),
                Some("out.txt"),
                "null_device",
                "passed",
            ),
            (
                "output = 'out.txt'",
                "echo",
                None,
                Some("out.txt"),
                "test_input",
                "passed",
            ),
            (
                "input = 'in.txt'",
                "input-only",
                Some("in.txt"),
                None,
                "null_device",
                "passed",
            ),
            (
                "input = 'in.txt', output = 'out.txt'",
                "stdin",
                Some("in.txt"),
                Some("out.txt"),
                "null_device",
                "wrong_answer",
            ),
        ] {
            let fixture = Fixture::new();
            fixture.executable_problem(io, "");
            fixture.cases(&[("one", action, Some("42\n"))]);
            let Some((job, doc)) = fixture.test_report(i32::from(verdict != "passed")) else {
                continue;
            };
            assert_job(
                &doc,
                serde_json::json!({"input":input,"output":output,"stdin":stdin}),
            );
            assert_test(
                &doc,
                "one",
                verdict,
                if verdict == "passed" {
                    "passed"
                } else {
                    "failed"
                },
                "exited_zero",
                output.map(|_| "collected"),
            );
            let test = test_entry(&doc, "one");
            let execution =
                &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
            let run = execution["run"].as_str().unwrap();
            let cwd = job.join(run).join("cwd");
            assert!(!cwd.join("in.txt").exists());
            assert!(!cwd.join("out.txt").exists());
            if output.is_some() {
                assert_collected(&job, test, if action == "stdin" { b"" } else { b"42\n" });
            }
            if input.is_some() && action == "echo" {
                assert_eq!(
                    fs::read(job.join(run).join("out").join("stderr")).unwrap(),
                    b"0"
                );
            }
            let spec: serde_json::Value =
                serde_json::from_slice(&fs::read(job.join(run).join("spec.json")).unwrap())
                    .unwrap();
            assert_eq!(spec["stage_input"].is_null(), input.is_none());
            assert_eq!(spec["output_file"].is_null(), output.is_none());
            if action == "stdin" {
                assert!(
                    execution["summary"]["timing"]["wall_us"]
                        .as_str()
                        .unwrap()
                        .parse::<u64>()
                        .unwrap()
                        < 2_000_000
                );
            }
        }
    });
}

// F-05, F-06, F-14: stdout is evidence; absence precedes answer absence.
#[test]
fn output_presence_and_empty_files_decide_comparison() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "");
        fixture.cases(&[
            ("noise", "noise", Some("42\n")),
            ("stdout", "stdout", Some("42\n")),
            ("both", "both", Some("42\n")),
            ("missing", "missing", Some("42\n")),
            ("answerless", "missing", None),
            ("exit", "exit", Some("42\n")),
            ("empty", "empty", Some("")),
            ("empty-wrong", "empty", Some("1")),
        ]);
        let Some((job, doc)) = fixture.test_report(1) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        for id in ["noise", "both", "empty"] {
            assert_test(
                &doc,
                id,
                "passed",
                "passed",
                "exited_zero",
                Some("collected"),
            );
        }
        for id in ["stdout", "missing", "answerless", "exit"] {
            let class = if id == "exit" {
                "nonzero_exit"
            } else {
                "exited_zero"
            };
            assert_test(
                &doc,
                id,
                if id == "exit" { class } else { "wrong_answer" },
                "output_rejected",
                class,
                Some("missing"),
            );
            let test = test_entry(&doc, id);
            assert_eq!(test["check"]["reason"], "output_missing");
            assert_eq!(test["diagnostic"], id == "exit");
        }
        assert_test(
            &doc,
            "empty-wrong",
            "wrong_answer",
            "failed",
            "exited_zero",
            Some("collected"),
        );
        assert!(!test_entry(&doc, "empty-wrong")["check"]["diag"].is_null());
        assert_collected(&job, test_entry(&doc, "empty"), b"");
        assert_collected(&job, test_entry(&doc, "empty-wrong"), b"");
        let test = test_entry(&doc, "noise");
        let execution = &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
        assert_eq!(execution["summary"]["stdout"]["stored"], "1048576");
        assert_eq!(
            fs::read(
                job.join(execution["run"].as_str().unwrap())
                    .join("out")
                    .join("stdout")
            )
            .unwrap(),
            vec![b'z'; 1048576]
        );
    });
}

// F-07, F-10, F-11 (fixed types), F-34 (fixed spellings): spelling wins over type.
#[test]
fn collection_is_spelling_sensitive_and_never_follows_special_files() {
    run_test(move || async move {
        for (output, actions) in [
            (
                "out.txt",
                vec![
                    "case",
                    "link",
                    "hosts",
                    "pipe-link",
                    "directory",
                    "fifo",
                    "socket",
                    "case-link",
                    "case-directory",
                    "case-unreadable",
                ],
            ),
            ("résultat.txt", vec!["nfd"]),
        ] {
            let fixture = Fixture::new();
            fixture.executable_problem(&format!("input = 'in.txt', output = '{output}'"), "");
            let cases: Vec<_> = actions
                .iter()
                .map(|action| (*action, *action, Some("42\n")))
                .collect();
            fixture.cases(&cases);
            let Some((_, doc)) = fixture.test_report(1) else {
                continue;
            };
            assert_job(
                &doc,
                serde_json::json!({"input":"in.txt","output":output,"stdin":"null_device"}),
            );
            for action in actions {
                let misnamed = action == "nfd" || action.starts_with("case");
                assert_test(
                    &doc,
                    action,
                    "wrong_answer",
                    "output_rejected",
                    "exited_zero",
                    Some(if misnamed { "misnamed" } else { "not_regular" }),
                );
                let test = test_entry(&doc, action);
                assert_eq!(
                    test["check"]["reason"],
                    if misnamed {
                        "output_misnamed"
                    } else {
                        "output_not_regular"
                    }
                );
                if !misnamed {
                    assert_eq!(
                        test["output_file"]["type"],
                        match action {
                            "link" | "hosts" | "pipe-link" => "link",
                            "directory" => "directory",
                            _ => "other",
                        }
                    );
                }
            }
        }
    });
}

// F-08: inclusive capture cap, an independent test class and no truncated evidence.
#[test]
fn output_file_cap_is_inclusive_and_raises_only_the_test_class() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem(
            "input = 'in.txt', output = 'out.txt'",
            "[config.run]\nstdout_cap = 4096\n",
        );
        let a = "x".repeat(4095);
        let b = "x".repeat(4096);
        let c = "x".repeat(4097);
        fixture.cases(&[
            ("below", "size 4095", Some(&a)),
            ("at", "size 4096", Some(&b)),
            ("above", "size 4097", Some(&c)),
        ]);
        let Some((job, doc)) = fixture.test_report(1) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        for (id, bytes) in [("below", a.as_bytes()), ("at", b.as_bytes())] {
            assert_test(
                &doc,
                id,
                "passed",
                "passed",
                "exited_zero",
                Some("collected"),
            );
            assert_collected(&job, test_entry(&doc, id), bytes);
        }
        assert_test(
            &doc,
            "above",
            "output_budget",
            "output_rejected",
            "output_budget",
            Some("too_large"),
        );
        let test = test_entry(&doc, "above");
        assert_eq!(test["class"]["stream"], "output_file");
        assert_eq!(test["class"]["streams"], serde_json::json!(["output_file"]));
        assert_eq!(test["check"]["reason"], "output_too_large");
        assert_eq!(test["diagnostic"], true);
        assert_eq!(test["output_file"]["bytes"], "4097");
        assert_eq!(test["output_file"]["budget"], "4096");
        let execution = &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
        assert_eq!(execution["summary"]["class"]["kind"], "exited_zero");
        assert!(
            !job.join(execution["run"].as_str().unwrap())
                .join("out")
                .join("output")
                .exists()
        );
    });
}

// F-15: both checker protocols receive the extracted output, and skip absence.
#[test]
fn checkers_judge_the_collected_file() {
    run_test(move || async move {
        for profile in ["testlib", "simple"] {
            let fixture = Fixture::new();
            fixture.executable_problem("input = 'in.txt', output = 'out.txt'", &format!("[check]\nkind = 'checker'\nchecker = {{ sources = ['checker.cpp'], language = 'cpp', profile = '{profile}' }}\n"));
            fixture.write("problems/A/checker.cpp", r#"#include <fstream>
#include <iterator>
#include <string>
int main(int argc, char** argv) {
    if (argc != 4 && argc != 6) return 3;
    auto read = [](char* path) { std::ifstream f(path); return std::string(std::istreambuf_iterator<char>(f), {}); };
    bool ok = read(argv[2]) == read(argv[3]);
    if (argc == 6) { std::ofstream report(argv[4]); report << "<result outcome='" << (ok ? "accepted" : "wrong-answer") << "'/>"; }
    return ok ? 0 : 1;
}
"#);
            fixture.cases(&[
                ("good", "noise", Some("42\n")),
                ("missing", "missing", Some("42\n")),
                ("exit", "exit", Some("42\n")),
            ]);
            let Some((job, doc)) = fixture.test_report(1) else {
                continue;
            };
            assert_job(
                &doc,
                serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
            );
            assert_test(
                &doc,
                "good",
                "passed",
                "passed",
                "exited_zero",
                Some("collected"),
            );
            assert_collected(&job, test_entry(&doc, "good"), b"42\n");
            for id in ["missing", "exit"] {
                let class = if id == "exit" {
                    "nonzero_exit"
                } else {
                    "exited_zero"
                };
                assert_test(
                    &doc,
                    id,
                    if id == "exit" { class } else { "wrong_answer" },
                    "output_rejected",
                    class,
                    Some("missing"),
                );
                let test = test_entry(&doc, id);
                assert_eq!(test["check"]["reason"], "output_missing");
                assert_eq!(test["diagnostic"], id == "exit");
                let checker =
                    &doc["executions"][test["judges"][0]["execution"].as_u64().unwrap() as usize];
                assert_eq!(
                    checker["coverage"],
                    serde_json::json!({"kind":"skipped","reason":"not_eligible"})
                );
                assert!(checker["run"].is_null());
            }
        }
    });
}

// F-16: validator stdin and staging are independent of the solution's io.input.
#[test]
fn validator_refusal_prevents_solution_staging() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "[check]\nvalidator = { sources = ['validator.cpp'], language = 'cpp', profile = 'testlib' }\n");
        fixture.write(
            "problems/A/validator.cpp",
            r#"#include <fstream>
#include <iostream>
#include <iterator>
#include <string>
int main() {
    if (std::ifstream("in.txt")) return 4;
    std::ifstream file("input");
    std::string staged(std::istreambuf_iterator<char>(file), {});
    std::string streamed(std::istreambuf_iterator<char>(std::cin), {});
    if (staged != streamed) return 4;
    return streamed == "bad" ? 3 : 0;
}
"#,
        );
        fixture.cases(&[("good", "echo", Some("42\n")), ("bad", "bad", Some("42\n"))]);
        // Invalid test data makes the job `checker_failed`: exit 5 (report §5.1).
        let Some((job, doc)) = fixture.test_report(5) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        assert_test(
            &doc,
            "good",
            "passed",
            "passed",
            "exited_zero",
            Some("collected"),
        );
        assert_collected(&job, test_entry(&doc, "good"), b"42\n");
        let test = test_entry(&doc, "bad");
        assert_eq!(test["validation"]["outcome"], "validation_failed");
        assert_eq!(test["verdict"]["kind"], "checker_failed");
        assert_eq!(test["check"]["kind"], "judge_failed");
        assert_eq!(test["check"]["reason"], "validation_failed");
        assert!(test["class"].is_null());
        assert_eq!(test["output_file"]["kind"], "not_attempted");
        let first = test["executions"]["first"].as_u64().unwrap() as usize;
        assert_eq!(test["executions"]["count"], 2);
        assert_eq!(doc["executions"][first]["coverage"]["kind"], "started");
        let solution = &doc["executions"][first + 1];
        assert_eq!(
            solution["coverage"],
            serde_json::json!({"kind":"skipped","reason":"input_not_validated"})
        );
        assert!(solution["run"].is_null());
    });
}

// F-17–F-19: remove I/O scratch before measurement; every run uses frozen bytes.
#[test]
fn scratch_cleanup_and_frozen_input_survive_solution_changes() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "");
        fixture.write(
            "cpforge.toml",
            "schema = 1\n[build]\ndeadline_s = 90\n[retention]\ncwd_bytes = '1MiB'\n",
        );
        let large = format!("echo {}", "x".repeat(2 * 1024 * 1024));
        fixture.cases(&[
            ("overflow", "overflow", Some("42\n")),
            ("large-input", &large, Some("42\n")),
            ("delete", "delete", Some("42\n")),
            ("directory", "replace-directory", Some("42\n")),
            ("link", "replace-link", Some("42\n")),
            ("rename", "rename", Some("42\n")),
            ("overwrite-a", "overwrite", Some("42\n")),
            ("overwrite-b", "overwrite", Some("42\n")),
        ]);
        let mut hashes = Vec::new();
        for _ in 0..2 {
            let Some((job, doc)) = fixture.test_report(0) else {
                return;
            };
            assert_job(
                &doc,
                serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
            );
            for test in doc["tests"].as_array().unwrap() {
                let id = test["id"].as_str().unwrap();
                assert_test(
                    &doc,
                    id,
                    "passed",
                    "passed",
                    "exited_zero",
                    Some("collected"),
                );
                assert_collected(&job, test, b"42\n");
                let execution =
                    &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
                let cwd = job.join(execution["run"].as_str().unwrap()).join("cwd");
                assert!(!cwd.join("out.txt").exists());
                if id == "overflow" {
                    // Cleared beyond cwd_bytes (whether cwd/ itself survives is the walker's).
                    assert!(!cwd.join("other").exists());
                } else if id == "directory" {
                    assert!(cwd.join("in.txt").is_dir());
                } else {
                    assert!(fs::symlink_metadata(cwd.join("in.txt")).is_err());
                }
                if id == "rename" {
                    assert_eq!(fs::read(cwd.join("x.txt")).unwrap(), b"rename");
                }
                if id.starts_with("overwrite") {
                    assert_eq!(
                        fs::read(job.join("inputs").join(format!("{id}.in"))).unwrap(),
                        b"overwrite"
                    );
                    hashes.push(test["input_sha256"].clone());
                }
            }
            let warnings = doc["warnings"].as_array().unwrap();
            assert!(!warnings.iter().any(|w| w["kind"] == "io_scratch"));
            let overflow: Vec<_> = warnings
                .iter()
                .filter(|w| w["kind"] == "retention" && w["detail"] == "cwd_overflow")
                .collect();
            assert_eq!(overflow.len(), 1);
            let test = test_entry(&doc, "overflow");
            assert_eq!(
                overflow[0]["run"],
                doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize]["run"]
            );
        }
        assert_eq!(hashes.len(), 4);
        assert!(hashes.iter().all(|h| *h == hashes[0]));
    });
}

// F-24 (Kill): stdout remains budgeted even when only the file is judged.
#[test]
fn stdout_budget_remains_independent_of_output_file() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem(
            "input = 'in.txt', output = 'out.txt'",
            "[config.run]\nstdout_cap = 4096\n",
        );
        fixture.cases(&[("one", "kill", Some("42\n"))]);
        let Some((job, doc)) = fixture.test_report(1) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        assert_test(
            &doc,
            "one",
            "output_budget",
            "passed",
            "output_budget",
            Some("collected"),
        );
        let test = test_entry(&doc, "one");
        assert_collected(&job, test, b"42\n");
        assert_eq!(test["class"]["stream"], "stdout");
        assert_eq!(test["diagnostic"], true);
    });
}

// F-12: permission errors are solution failures, not missing output.
#[test]
fn unreadable_output_and_directory_are_rejected() {
    run_test(move || async move {
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("permission row skipped: root bypasses mode-000 permissions");
            return;
        }
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "");
        fixture.cases(&[
            ("file", "unreadable", Some("42\n")),
            ("directory", "cwd-unreadable", Some("42\n")),
        ]);
        let Some((job, doc)) = fixture.test_report(1) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        for id in ["file", "directory"] {
            assert_test(
                &doc,
                id,
                "wrong_answer",
                "output_rejected",
                "exited_zero",
                Some("unreadable"),
            );
            let test = test_entry(&doc, id);
            assert_eq!(test["check"]["reason"], "output_unreadable");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(test["output_file"]["os_error"], libc::EACCES);
                let execution =
                    &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
                fs::set_permissions(
                    job.join(execution["run"].as_str().unwrap()).join("cwd"),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
        }
        #[cfg(not(unix))]
        let _ = job;
    });
}

// F-22: collection finishes before cancellation arbitrates the comparison.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancellation_after_capture_keeps_the_collected_file() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "");
        fixture.cases(&[("one", "echo", Some("42\n"))]);
        #[cfg(windows)]
        {
            fixture.test_report(0);
        }
        #[cfg(unix)]
        {
            use std::io::Read;
            use std::time::{Duration, Instant};
            let ready = fixture.path("signal-ready");
            let mut child = Command::new(env!("CARGO_BIN_EXE_cpf"))
                .current_dir(&fixture.0)
                .env("HOME", fixture.path("user"))
                .env("XDG_CONFIG_HOME", fixture.path("user"))
                .env("CPF_STORAGE_PAUSE_AT", "file_io.before_check")
                .env("CPF_STORAGE_BARRIER_DIR", &fixture.0)
                .env("CPF_STORAGE_SIGNAL_READY", &ready)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .args(["--problem", "A", "test", "--json"])
                .spawn()
                .unwrap();
            let mut stdout = child.stdout.take().unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                let mut marker = Vec::new();
                loop {
                    let mut byte = [0];
                    stdout.read_exact(&mut byte).unwrap();
                    marker.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                send.send((stdout, marker)).unwrap();
            });
            let (stdout, marker) = receive.recv_timeout(Duration::from_secs(120)).unwrap();
            reader.join().unwrap();
            child.stdout = Some(stdout);
            assert_eq!(marker, b"CPF-PAUSED file_io.before_check\n");
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
            let until = Instant::now() + Duration::from_secs(5);
            while !ready.exists() {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(1));
            }
            fixture.write("file_io.before_check.release", "release");
            let output = child.wait_with_output().unwrap();
            use std::os::unix::process::ExitStatusExt;
            assert!(
                output.status.code() == Some(130) || output.status.signal() == Some(libc::SIGINT),
                "{output:?}"
            );
            let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(doc["result"]["pre_delivery_exit"]["code"], 130);
            assert_job(
                &doc,
                serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
            );
            assert_test(
                &doc,
                "one",
                "cancelled",
                "not_admitted",
                "exited_zero",
                Some("collected"),
            );
            assert_eq!(test_entry(&doc, "one")["check"]["reason"], "cancelled");
            assert_collected(
                &fixture.path(doc["job"]["dir"].as_str().unwrap()),
                test_entry(&doc, "one"),
                b"42\n",
            );
        }
    });
}

// F-27: the report contract's worst case for kind `compare` with `io.output`
// (N = N_t = N_a = 10 000); the variant bounds are checked by wire/io_tests.
#[test]
fn file_io_compare_plan_uses_the_full_report_reservation() {
    let shape = cpforge::report::wire::ReportShape {
        output_file: true,
        ..Default::default()
    };
    assert_eq!(
        cpforge::report::wire::report_reservation(10_000, 10_000, 10_000, Some(shape)).unwrap(),
        254_982_144
    );
}

// F-27: the longest distinct portable names also work through the real command.
#[test]
fn maximum_length_io_names_are_applied_and_reported_verbatim() {
    run_test(move || async move {
        let fixture = Fixture::new();
        let input = format!("{}a", "é".repeat(127));
        let output = format!("{}b", "é".repeat(127));
        fixture.executable_problem(&format!("input = '{input}', output = '{output}'"), "");
        fixture.write(
            "problems/A/main.cpp",
            &format!(
                r#"#include <fstream>
#include <string>
int main() {{
    std::ifstream input("{input}"); std::string text; input >> text;
    std::ofstream output("{output}"); output << (text == "echo" ? "42\n" : "bad\n");
}}
"#
            ),
        );
        fixture.cases(&[("one", "echo", Some("42\n"))]);
        let Some((job, doc)) = fixture.test_report(0) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":input,"output":output,"stdin":"null_device"}),
        );
        assert_test(
            &doc,
            "one",
            "passed",
            "passed",
            "exited_zero",
            Some("collected"),
        );
        assert_collected(&job, test_entry(&doc, "one"), b"42\n");
    });
}

// F-25: collection/removal use the held directory even after cwd is replaced.
#[test]
fn renamed_cwd_collects_and_clears_through_the_original_handle() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "");
        fixture.cases(&[("one", "replace-cwd", Some("42\n"))]);
        let Some((job, doc)) = fixture.test_report(0) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        assert_test(
            &doc,
            "one",
            "passed",
            "passed",
            "exited_zero",
            Some("collected"),
        );
        let test = test_entry(&doc, "one");
        assert_collected(&job, test, b"42\n");
        let execution = &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
        let run = job.join(execution["run"].as_str().unwrap());
        assert!(!run.join("old").join("in.txt").exists());
        assert!(!run.join("old").join("out.txt").exists());
        assert_eq!(
            fs::read(run.join("cwd").join("out.txt")).unwrap(),
            b"wrong\n"
        );
        assert!(
            doc["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["kind"] == "io_scratch" && w["detail"] == "not_measured")
        );
    });
}

// F-30, real variant: absence makes the bounded listing independent of order.
#[test]
fn directory_listing_cuts_off_after_65536_entries() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("output = 'out.txt'", "[config.run]\ndeadline_s = 90\n");
        fixture.cases(&[("many", "many", Some("42\n"))]);
        let Some((_, doc)) = fixture.test_report(1) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":null,"output":"out.txt","stdin":"test_input"}),
        );
        assert_test(
            &doc,
            "many",
            "wrong_answer",
            "output_rejected",
            "exited_zero",
            Some("too_many_entries"),
        );
        assert_eq!(
            test_entry(&doc, "many")["check"]["reason"],
            "output_dir_too_large"
        );
    });
}

// F-09 and F-13: a read stream can grow beyond fstat or fail independently of it.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn injected_growth_read_and_extraction_errors_keep_bounded_evidence() {
    run_test(move || async move {
        for (settings, output, verdict, check, status) in [
            (
                vec![("CPF_FILE_IO_READ_SIZE", "8192")],
                "too_large",
                "output_budget",
                "output_rejected",
                1,
            ),
            (
                vec![
                    ("CPF_FILE_IO_FAULTS", "file_io.read=EIO@2"),
                    ("CPF_FILE_IO_READ_CHUNK", "1"),
                ],
                "not_collected",
                "infrastructure",
                "check_error",
                10,
            ),
            (
                vec![
                    ("CPF_FILE_IO_FAULTS", "file_io.write=ENOSPC@2"),
                    ("CPF_FILE_IO_READ_CHUNK", "1"),
                ],
                "not_collected",
                "infrastructure",
                "check_error",
                10,
            ),
        ] {
            let fixture = Fixture::new();
            fixture.executable_problem(
                "input = 'in.txt', output = 'out.txt'",
                "[config.run]\nstdout_cap = 4096\n",
            );
            fixture.cases(&[("one", "size 4000", Some("42\n"))]);
            let Some((job, doc)) = fixture.test_report_with_env(status, &settings) else {
                continue;
            };
            assert_job(
                &doc,
                serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
            );
            assert_test(
                &doc,
                "one",
                verdict,
                check,
                if output == "too_large" {
                    "output_budget"
                } else {
                    "exited_zero"
                },
                Some(output),
            );
            let test = test_entry(&doc, "one");
            let execution =
                &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
            assert_eq!(execution["summary"]["class"]["kind"], "exited_zero");
            assert!(
                !job.join(execution["run"].as_str().unwrap())
                    .join("out")
                    .join("output")
                    .exists()
            );
            if output == "too_large" {
                assert_eq!(test["class"]["stream"], "output_file");
                assert_eq!(test["class"]["streams"], serde_json::json!(["output_file"]));
                assert_eq!(test["output_file"]["bytes"], "4097");
                assert_eq!(test["output_file"]["budget"], "4096");
                assert_eq!(test["check"]["reason"], "output_too_large");
            } else {
                assert_eq!(test["output_file"]["reason"], "io");
                assert!(test["output_file"]["os_error"].is_i64());
                #[cfg(unix)]
                assert_eq!(
                    test["output_file"]["os_error"],
                    if settings[0].1.contains("EIO") {
                        libc::EIO
                    } else {
                        libc::ENOSPC
                    }
                );
                assert_eq!(test["check"]["os_error"], test["output_file"]["os_error"]);
            }
        }
    });
}

// F-20's staging failure: it is setup infrastructure, with no child started.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn staging_enospc_never_starts_the_solution() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "");
        fixture.cases(&[("one", "echo", Some("42\n"))]);
        let Some((job, doc)) = fixture.test_report_with_env(
            10,
            &[("CPF_FILE_IO_FAULTS", "file_io.staging.write=ENOSPC")],
        ) else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        let test = test_entry(&doc, "one");
        assert_eq!(test["verdict"]["kind"], "infrastructure");
        assert_eq!(test["class"]["kind"], "infrastructure");
        assert_eq!(test["check"]["kind"], "not_admitted");
        assert_eq!(test["output_file"]["kind"], "not_attempted");
        let executions = doc["executions"].as_array().unwrap();
        for (index, execution) in executions.iter().enumerate() {
            assert_eq!(
                execution["coverage"]["kind"],
                if index + 1 == executions.len() {
                    "not_started"
                } else {
                    "started"
                }
            );
        }
        let execution = executions.last().unwrap();
        assert_eq!(execution["coverage"]["reason"]["kind"], "setup");
        let run = job.join(execution["run"].as_str().unwrap());
        assert!(!run.join("cwd").join("in.txt").exists());
        assert!(!run.join("cwd").join("out.txt").exists());
    });
}

// F-23: unverified containment is a note and does not prevent comparison.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn unverified_cleanup_still_collects_and_passes() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "");
        fixture.cases(&[("one", "echo", Some("42\n"))]);
        let Some((job, doc)) = fixture.test_report_with_env(0, &[("CPF_FILE_IO_UNVERIFIED", "1")])
        else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        assert_test(
            &doc,
            "one",
            "passed",
            "passed",
            "exited_zero",
            Some("collected"),
        );
        assert_collected(&job, test_entry(&doc, "one"), b"42\n");
        let warning = doc["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["kind"] == "cleanup_unverified")
            .unwrap();
        assert_eq!(warning["runs"], 1);
        assert_eq!(warning["message"], "cleanup not verified");
    });
}

// E §2.1: the checker's `output` is the frozen capture (stdout, or the
// extracted `out/output`), even when that file grows after the freeze.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn checker_output_uses_frozen_length_even_when_the_source_grows() {
    run_test(move || async move {
        for (io, action, judged, target) in [
            (
                "input = 'in.txt'",
                "input-only",
                "stdout",
                serde_json::json!({"input":"in.txt","output":null,"stdin":"null_device"}),
            ),
            (
                "input = 'in.txt', output = 'out.txt'",
                "echo",
                "output",
                serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
            ),
        ] {
            let fixture = Fixture::new();
            fixture.executable_problem(io, "[check]\nkind = 'checker'\nchecker = { sources = ['checker.cpp'], language = 'cpp', profile = 'simple' }\n");
            fixture.write(
                "problems/A/checker.cpp",
                r#"#include <fstream>
#include <iterator>
#include <string>
int main(int, char** argv) {
    std::ifstream file(argv[2]);
    std::string bytes((std::istreambuf_iterator<char>(file)), {});
    return bytes == "42\n" ? 0 : 1;
}
"#,
            );
            fixture.cases(&[("one", action, Some("42\n"))]);
            let Some((job, doc)) = fixture
                .test_report_with_env(0, &[("CPF_FILE_IO_GROW_STDOUT", "appended after freeze")])
            else {
                continue;
            };
            assert_job(&doc, target);
            assert_test(
                &doc,
                "one",
                "passed",
                "passed",
                "exited_zero",
                (judged == "output").then_some("collected"),
            );
            let test = test_entry(&doc, "one");
            let solution =
                &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
            if judged == "output" {
                assert_eq!(test["output_file"]["bytes"], "3");
            } else {
                assert_eq!(solution["summary"]["stdout"]["stored"], "3");
            }
            // The hook grew the source before the checker copy; the checker
            // still saw exactly the frozen three bytes.
            assert_eq!(
                fs::read(
                    job.join(solution["run"].as_str().unwrap())
                        .join("out")
                        .join(judged)
                )
                .unwrap(),
                b"42\nappended after freeze"
            );
        }
    });
}

#[cfg(all(feature = "storage-test-hooks", unix))]
impl Fixture {
    fn paused_report(
        &self,
        point: &str,
        status: i32,
        change: impl FnOnce(&Path),
    ) -> (PathBuf, serde_json::Value, String) {
        let mut change = Some(change);
        self.paused_report_with_env(point, status, &[], |_, cwd, _| {
            change.take().unwrap()(cwd);
        })
    }

    fn paused_report_with_env(
        &self,
        points: &str,
        status: i32,
        settings: &[(&str, &str)],
        mut change: impl FnMut(&str, &Path, &Path),
    ) -> (PathBuf, serde_json::Value, String) {
        use std::io::{BufRead, BufReader, Read};
        use std::time::{Duration, Instant};
        let barriers = self.path("barriers");
        fs::create_dir(&barriers).unwrap();
        let log = self.path("operations");
        let mut settings = settings.to_vec();
        settings.extend([
            ("CPF_STORAGE_PAUSE_AT", points),
            ("CPF_STORAGE_BARRIER_DIR", barriers.to_str().unwrap()),
            ("CPF_FILE_IO_LOG", log.to_str().unwrap()),
        ]);
        let mut child = self
            .command(&["test", "--json"], &settings)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut doc = Vec::new();
            for line in BufReader::new(stdout).split(b'\n') {
                let line = line.unwrap();
                if line.starts_with(b"CPF-PAUSED ") {
                    let point = std::str::from_utf8(&line)
                        .unwrap()
                        .strip_prefix("CPF-PAUSED ")
                        .unwrap()
                        .to_owned();
                    let _ = send.send(point);
                } else {
                    doc.extend(line);
                    doc.push(b'\n');
                }
            }
            doc
        });
        let errors = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            BufReader::new(stderr).read_to_end(&mut bytes).unwrap();
            bytes
        });
        // Kill/wait even if an assertion fails, before the fixture deletes cwd.
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Child(child);
        let first_point = receive
            .recv_timeout(Duration::from_secs(120))
            .expect("file-I/O barrier not reached");
        let jobs: Vec<_> = fs::read_dir(self.path(".cpforge/runs"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(jobs.len(), 1);
        let job = jobs[0].clone();
        let cwd = fs::read_dir(&job)
            .unwrap()
            .map(|e| e.unwrap().path().join("cwd"))
            .find(|cwd| cwd.join("out.txt").is_file())
            .expect("solution cwd at barrier");
        let mut point = first_point;
        loop {
            change(&point, &cwd, &job);
            fs::write(barriers.join(format!("{point}.release")), b"release").unwrap();
            if points.rsplit(',').next() == Some(point.as_str()) {
                break;
            }
            point = receive.recv_timeout(Duration::from_secs(20)).unwrap();
        }
        let until = Instant::now() + Duration::from_secs(45);
        let exit = loop {
            if let Some(exit) = child.0.try_wait().unwrap() {
                break exit;
            }
            assert!(
                Instant::now() < until,
                "cpf did not finish after barrier release"
            );
            // Exit polling, not an ordering or race-timing assumption.
            std::thread::sleep(Duration::from_millis(10));
        };
        let errors = errors.join().unwrap();
        assert_eq!(
            exit.code(),
            Some(status),
            "{}",
            String::from_utf8_lossy(&errors)
        );
        let bytes = reader.join().unwrap();
        cpforge::report::wire::read::read_json(&bytes).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["result"]["pre_delivery_exit"]["code"], status);
        (job, doc, fs::read_to_string(log).unwrap())
    }
}

// F-11's race: pause after the regular type observation, before openat.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn regular_file_replaced_by_socket_between_type_check_and_open_is_not_infrastructure() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("output = 'out.txt'", "");
        fixture.cases(&[("one", "echo", Some("42\n"))]);
        #[cfg(windows)]
        assert!(fixture.test_report(10).is_none());
        #[cfg(unix)]
        {
            fixture.write(
                "socket.cpp",
                r#"#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <cstring>
int main() {
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    sockaddr_un address{}; address.sun_family = AF_UNIX;
    std::strcpy(address.sun_path, "out.txt");
    if (unlink("out.txt")) return 1;
    int result = bind(fd, reinterpret_cast<sockaddr*>(&address), sizeof(address));
    close(fd); return result ? 2 : 0;
}
"#,
            );
            let helper = fixture.path("socket-helper");
            let compiler = if cfg!(target_os = "macos") {
                "g++-14"
            } else {
                "g++"
            };
            let output = Command::new(compiler)
                .arg(fixture.path("socket.cpp"))
                .arg("-o")
                .arg(&helper)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let (_, doc, log) = fixture.paused_report("file_io.open", 1, |cwd| {
                // Keep the old inode alive so replacement cannot reuse it.
                let _original = fs::File::open(cwd.join("out.txt")).unwrap();
                let output = Command::new(&helper).current_dir(cwd).output().unwrap();
                assert!(output.status.success(), "{output:?}");
            });
            assert_job(
                &doc,
                serde_json::json!({"input":null,"output":"out.txt","stdin":"test_input"}),
            );
            assert_test(
                &doc,
                "one",
                "wrong_answer",
                "output_rejected",
                "exited_zero",
                Some("not_regular"),
            );
            assert_eq!(test_entry(&doc, "one")["output_file"]["type"], "other");
            assert_eq!(
                test_entry(&doc, "one")["check"]["reason"],
                "output_not_regular"
            );
            assert!(
                log.contains("file_io.type_check 2 "),
                "replacement must be re-observed: {log}"
            );
        }
    });
}

// F-30, stub listing: the exact name on either side of the fixed cutoff.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn injected_listing_order_observes_the_fixed_entry_boundary() {
    run_test(move || async move {
        for (position, status, outcome, verdict, check) in [
            ("65536", 0, "collected", "passed", "passed"),
            (
                "65537",
                1,
                "too_many_entries",
                "wrong_answer",
                "output_rejected",
            ),
        ] {
            let fixture = Fixture::new();
            fixture.executable_problem("output = 'out.txt'", "");
            fixture.cases(&[("one", "echo", Some("42\n"))]);
            let Some((job, doc)) =
                fixture.test_report_with_env(status, &[("CPF_FILE_IO_LISTING_POSITION", position)])
            else {
                continue;
            };
            assert_job(
                &doc,
                serde_json::json!({"input":null,"output":"out.txt","stdin":"test_input"}),
            );
            assert_test(&doc, "one", verdict, check, "exited_zero", Some(outcome));
            if outcome == "collected" {
                assert_collected(&job, test_entry(&doc, "one"), b"42\n");
            } else {
                assert_eq!(
                    test_entry(&doc, "one")["check"]["reason"],
                    "output_dir_too_large"
                );
            }
        }
    });
}

// F-31/F-33 use engine workers on supported platforms; Windows refuses the
// identical job command and declaration before allocating any job directory.
#[cfg(all(feature = "storage-test-hooks", windows))]
#[test]
fn file_worker_fault_jobs_refuse_before_admission_on_windows() {
    let fixture = Fixture::new();
    for io in ["output = 'out.txt'", "input = 'in.txt', output = 'out.txt'"] {
        fixture.executable_problem(io, "[config.run]\nstdout_cap = 4096\n");
        fixture.cases(&[("one", "echo", Some("42\n")), ("two", "echo", Some("42\n"))]);
        assert!(fixture.test_report(10).is_none());
    }
}

// F-21's command/report assertions complement the engine accounting counters.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn failed_io_scratch_removal_warns_and_excludes_the_staged_input_from_measurement() {
    run_test(move || async move {
        let fixture = Fixture::new();
        fixture.executable_problem("input = 'in.txt', output = 'out.txt'", "");
        fixture.write(
            "cpforge.toml",
            "schema = 1\n[build]\ndeadline_s = 90\n[retention]\ncwd_bytes = 3\n",
        );
        fixture.cases(&[
            ("kept", "echo", Some("42\n")),
            ("deleted", "delete", Some("42\n")),
        ]);
        let Some((job, doc)) =
            fixture.test_report_with_env(0, &[("CPF_FILE_IO_FAULTS", "file_io.unlink=EIO")])
        else {
            return;
        };
        assert_job(
            &doc,
            serde_json::json!({"input":"in.txt","output":"out.txt","stdin":"null_device"}),
        );
        for id in ["kept", "deleted"] {
            assert_test(
                &doc,
                id,
                "passed",
                "passed",
                "exited_zero",
                Some("collected"),
            );
            let test = test_entry(&doc, id);
            assert_collected(&job, test, b"42\n");
            let execution =
                &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
            let run = execution["run"].as_str().unwrap();
            let cwd = job.join(run).join("cwd");
            assert_eq!(cwd.join("in.txt").exists(), id == "kept");
            assert_eq!(fs::read(cwd.join("out.txt")).unwrap(), b"42\n");
            let warnings: Vec<_> = doc["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|w| w["run"] == run && w["kind"] == "io_scratch")
                .map(|w| w["detail"].as_str().unwrap())
                .collect();
            assert_eq!(
                warnings,
                if id == "kept" {
                    vec!["staged_input_kept", "output_kept"]
                } else {
                    vec!["output_kept"]
                }
            );
        }
        assert!(
            !doc["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["kind"] == "retention" && w["detail"] == "cwd_overflow")
        );
    });
}

// The child detaches before the root exits, closes the capture pipes and waits
// on a socket. Every mutation is requested only while the collector is paused.
#[cfg(all(feature = "storage-test-hooks", unix))]
struct Descendant {
    listener: std::net::TcpListener,
    stream: Option<std::net::TcpStream>,
}
#[cfg(all(feature = "storage-test-hooks", unix))]
impl Descendant {
    fn solution(fixture: &Fixture, initial: usize) -> Self {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let source = r#"#include <arpa/inet.h>
#include <sys/socket.h>
#include <unistd.h>
#include <cstdio>
#include <fstream>
#include <string>
static void write(const char* name, const std::string& bytes) {
    std::ofstream f(name, std::ios::binary); f << bytes; f.close();
    if (!f) _exit(31);
}
int main() {
    write("out.txt", std::string(INITIAL, 'x'));
    int ready[2]; if (pipe(ready)) return 32;
    pid_t root = getpid();
    pid_t child = fork(); if (child < 0) return 33;
    if (child) {
        close(ready[1]); char c;
        return read(ready[0], &c, 1) == 1 && c == 'r' ? 0 : 34;
    }
    close(ready[0]);
    if (setsid() < 0) _exit(35);
    close(0); close(1); close(2);
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    timeval timeout{20, 0};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    sockaddr_in address{}; address.sin_family = AF_INET;
    address.sin_port = htons(PORT); address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(fd, reinterpret_cast<sockaddr*>(&address), sizeof(address))) _exit(36);
    if (write(ready[1], "r", 1) != 1) _exit(37);
    close(ready[1]);
    char action;
    while (read(fd, &action, 1) == 1) {
        if (action == 'q') _exit(0);
        // Collection starts only after the root has been reaped.
        if (getppid() == root) _exit(38);
        if (action == 'a' && (rename("out.txt", "swap.tmp") || rename("swap.tmp", "OUT.TXT"))) _exit(39);
        if (action == 'b') {
            if (rename("out.txt", "accepted.txt")) _exit(40);
            write("out.txt", "new\n");
        }
        if (action == 'c' && (rename("OUT.TXT", "swap.tmp") || rename("swap.tmp", "out.txt"))) _exit(41);
        if (action == 'd') {
            if (rename("out.txt", "accepted.txt")) _exit(42);
            write("out.txt", "replacement\n");
            std::ofstream f("accepted.txt", std::ios::app); f << "appended";
            f.close(); if (!f) _exit(43);
        }
        if (action == 'g') {
            std::ofstream f("out.txt", std::ios::app); f << std::string(4192, 'x');
            f.close(); if (!f) _exit(44);
        }
        if (write(fd, "k", 1) != 1) _exit(45);
    }
    _exit(0);
}
"#;
        fixture.write(
            "problems/A/main.cpp",
            &source
                .replace("INITIAL", &initial.to_string())
                .replace("PORT", &port.to_string()),
        );
        Self {
            listener,
            stream: None,
        }
    }

    fn change(&mut self, action: u8) {
        use std::io::{Read, Write};
        let stream = self.stream.get_or_insert_with(|| {
            // A collector barrier proves the root exited after the child
            // connected and acknowledged readiness, so the connection is
            // already queued; the bound only turns a descendant that failed
            // before connecting into a failure instead of a hang.
            self.listener.set_nonblocking(true).unwrap();
            let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
            let stream = loop {
                match self.listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < until,
                            "the descendant never connected"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(20)))
                .unwrap();
            stream
                .set_write_timeout(Some(std::time::Duration::from_secs(20)))
                .unwrap();
            stream
        });
        stream.write_all(&[action]).unwrap();
        let mut ack = [0];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(
            ack,
            [b'k'],
            "descendant survived the root and completed its mutation"
        );
    }
}
#[cfg(all(feature = "storage-test-hooks", unix))]
impl Drop for Descendant {
    fn drop(&mut self) {
        use std::io::Write;
        if let Some(stream) = &mut self.stream {
            let _ = stream.write_all(b"q");
        }
    }
}

// F-29(a)-(d): the mutator is a surviving descendant, not the test driver.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn surviving_descendant_changes_spelling_inode_and_accepted_source_at_barriers() {
    run_test(move || async move {
        for variant in ['a', 'b', 'c', 'd'] {
            let fixture = Fixture::new();
            fixture.executable_problem("output = 'out.txt'", "");
            fixture.cases(&[(
                "one",
                "",
                Some(if variant == 'b' { "new\n" } else { "xxx" }),
            )]);
            #[cfg(windows)]
            assert!(fixture.test_report(10).is_none());
            #[cfg(unix)]
            {
                let mut descendant = Descendant::solution(&fixture, 3);
                let points = if variant == 'd' {
                    "file_io.extracted"
                } else {
                    "file_io.before_listing,file_io.after_listing"
                };
                let misnamed = matches!(variant, 'a' | 'c');
                let (job, doc, log) = fixture.paused_report_with_env(
                    points,
                    i32::from(misnamed),
                    &[("CPF_FILE_IO_UNVERIFIED", "1")],
                    |point, cwd, job| match point {
                        "file_io.before_listing" => {
                            descendant.change(if variant == 'b' { b'b' } else { b'a' })
                        }
                        "file_io.after_listing" if variant == 'c' => descendant.change(b'c'),
                        "file_io.extracted" => {
                            let copy = cwd.parent().unwrap().join("out").join("output");
                            assert!(copy.starts_with(job));
                            let before = fs::read(&copy).unwrap();
                            descendant.change(b'd');
                            assert_eq!(fs::read(copy).unwrap(), before);
                            assert_eq!(fs::read(cwd.join("out.txt")).unwrap(), b"replacement\n");
                            assert_eq!(fs::read(cwd.join("accepted.txt")).unwrap(), b"xxxappended");
                        }
                        _ => {}
                    },
                );
                assert_job(
                    &doc,
                    serde_json::json!({"input":null,"output":"out.txt","stdin":"test_input"}),
                );
                assert_test(
                    &doc,
                    "one",
                    if misnamed { "wrong_answer" } else { "passed" },
                    if misnamed {
                        "output_rejected"
                    } else {
                        "passed"
                    },
                    "exited_zero",
                    Some(if misnamed { "misnamed" } else { "collected" }),
                );
                if misnamed {
                    assert_eq!(
                        test_entry(&doc, "one")["check"]["reason"],
                        "output_misnamed"
                    );
                } else {
                    assert_collected(
                        &job,
                        test_entry(&doc, "one"),
                        if variant == 'b' { b"new\n" } else { b"xxx" },
                    );
                }
                let observations = log
                    .lines()
                    .filter(|l| l.starts_with("file_io.before_listing "))
                    .count();
                assert_eq!(observations, if variant == 'b' { 2 } else { 1 }, "{log}");
                for point in points.split(',') {
                    assert!(log.contains(&format!("{point} 1 ")), "{log}");
                }
                assert!(
                    doc["warnings"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|w| w["kind"] == "cleanup_unverified")
                );
            }
        }
    });
}

// F-32: real growth after accepted fstat, with and without partial unlink failure.
#[cfg(feature = "storage-test-hooks")]
#[test]
fn surviving_descendant_growth_probes_without_writing_beyond_the_reservation() {
    run_test(move || async move {
        for kept in [false, true] {
            let fixture = Fixture::new();
            fixture.executable_problem("output = 'out.txt'", "[config.run]\nstdout_cap = 4096\n");
            let answer = "x".repeat(4096);
            fixture.cases(&[("one", "", Some(answer.as_str()))]);
            #[cfg(windows)]
            {
                let _ = kept;
                assert!(fixture.test_report(10).is_none());
            }
            #[cfg(unix)]
            {
                let mut descendant = Descendant::solution(&fixture, 4000);
                let (job, doc, log) = fixture.paused_report_with_env(
                    "file_io.read",
                    1,
                    &[
                        ("CPF_FILE_IO_UNVERIFIED", "1"),
                        ("CPF_FILE_IO_READ_CHUNK", "1024"),
                        (
                            "CPF_FILE_IO_FAULTS",
                            if kept {
                                "file_io.partial.unlink=EIO"
                            } else {
                                ""
                            },
                        ),
                    ],
                    |_, cwd, _| {
                        assert_eq!(fs::metadata(cwd.join("out.txt")).unwrap().len(), 4000);
                        descendant.change(b'g');
                        assert_eq!(fs::metadata(cwd.join("out.txt")).unwrap().len(), 8192);
                    },
                );
                assert_job(
                    &doc,
                    serde_json::json!({"input":null,"output":"out.txt","stdin":"test_input"}),
                );
                assert_test(
                    &doc,
                    "one",
                    "output_budget",
                    "output_rejected",
                    "output_budget",
                    Some("too_large"),
                );
                let test = test_entry(&doc, "one");
                assert_eq!(test["output_file"]["bytes"], "4097");
                assert_eq!(test["output_file"]["budget"], "4096");
                assert_eq!(test["check"]["reason"], "output_too_large");
                assert_eq!(test["class"]["stream"], "output_file");
                let execution =
                    &doc["executions"][test["executions"]["first"].as_u64().unwrap() as usize];
                assert_eq!(execution["summary"]["class"]["kind"], "exited_zero");
                let copied: Vec<usize> = log
                    .lines()
                    .filter(|l| l.starts_with("file_io.copied "))
                    .map(|l| l.split_whitespace().nth(2).unwrap().parse().unwrap())
                    .collect();
                assert_eq!(copied, [1024, 2048, 3072, 4096]);
                let values = |point: &str| -> Vec<u64> {
                    log.lines()
                        .filter(|line| line.starts_with(point))
                        .map(|line| line.split_whitespace().nth(2).unwrap().parse().unwrap())
                        .collect()
                };
                assert_eq!(values("file_io.copy_length "), [1024, 2048, 3072, 4096]);
                // The charge at the first collection point includes the
                // output-file reservation; copying and probing never add to it.
                let charged = values("file_io.charged ");
                let limits = values("file_io.limit ");
                assert!(!charged.is_empty());
                assert_eq!(charged.len(), limits.len());
                assert!(
                    charged
                        .iter()
                        .zip(limits)
                        .all(|(charged, limit)| *charged <= limit)
                );
                assert!(charged.iter().all(|c| *c <= charged[0]), "{log}");
                let probe = log.find("file_io.probe 1 4096 ").unwrap();
                assert!(log[..probe].contains("file_io.copied 4 4096 "));
                assert!(log[probe..].contains("file_io.read 5 1 "));
                assert!(log.contains("file_io.partial.unlink 1 "));
                let run = execution["run"].as_str().unwrap();
                let copy = job.join(run).join("out").join("output");
                assert_eq!(copy.exists(), kept);
                if kept {
                    assert_eq!(fs::metadata(copy).unwrap().len(), 4096);
                }
                assert_eq!(
                    doc["warnings"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|w| w["kind"] == "io_scratch" && w["detail"] == "partial_copy_kept"),
                    kept
                );
            }
        }
    });
}
