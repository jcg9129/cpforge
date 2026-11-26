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
        if cfg!(windows) {
            let output = self.run(&["test", "--json"], 10);
            let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(doc["outcome"], "refused");
            assert_eq!(doc["refusal"]["class"], "infrastructure");
            return None;
        }
        let output = self.invoke(&["test", "--json"], status);
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
