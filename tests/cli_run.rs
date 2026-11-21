use clap::Parser;
use cpforge::cli::Cli;

#[test]
fn raw_options_and_machine_refusals() {
    Cli::try_parse_from([
        "cpf",
        "--problem",
        "local:A",
        "run",
        "--profile",
        "debug",
        "--deadline",
        "1s",
    ])
    .unwrap();
    // Config §8.1 grants extra argv only to cpf cmd; raw mode rejects
    // machine output (execution contract §8.1). Clap refuses both with 2.
    for args in [
        vec!["cpf", "run", "extra"],
        vec!["cpf", "run", "--", "--child-arg"],
        vec!["cpf", "run", "--json"],
        vec!["cpf", "run", "--events"],
        vec!["cpf", "run", "--events=output"],
    ] {
        assert_eq!(Cli::try_parse_from(args).unwrap_err().exit_code(), 2);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod execution {
    use cpforge::report::wire;
    use serde_json::Value;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Output, Stdio};
    use std::time::{Duration, Instant};

    struct Workspace(PathBuf);
    impl Workspace {
        fn new(language: Option<&str>) -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!(
                    "cli-run-{}",
                    cpforge::build::BuildId::generate().unwrap()
                ));
            fs::create_dir_all(&path).unwrap();
            let workspace = Self(cpforge::storage::paths::plain(
                &fs::canonicalize(path).unwrap(),
            ));
            workspace.write("cpforge.toml", "schema = 1\n[build]\ndeadline_s = 90\n");
            workspace.write(
                ".cpforge/job-seq",
                "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
            );
            workspace.write("user/cpforge/config.toml", "schema = 1\n");
            #[cfg(target_os = "macos")]
            workspace.write("user/cpforge/config.toml", "schema = 1\n[languages.cpp.build]\nargv = ['g++-14', '-std=gnu++23', '-O2', '-Wall', '{sources}', '-o', '{artifact}']\n");
            let config = language.map_or(String::new(), |language| {
                format!("[config]\nlanguage = '{language}'\n")
            });
            workspace.write(
                "problems/A/problem.toml",
                &format!("schema = 1\n[problem]\nid = 'local:A'\n{config}"),
            );
            workspace.write("problems/A/main.cpp", "int main() { return 0; }\n");
            workspace.write(
                "problems/A/main.py",
                "import sys\nsys.stdout.buffer.write(sys.stdin.buffer.read())\n",
            );
            workspace
        }
        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }
        fn write(&self, relative: &str, bytes: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        fn command(&self, args: &[&str]) -> Command {
            let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
            command
                .current_dir(self.path("problems/A"))
                .env("HOME", self.path("user"))
                .env("XDG_CONFIG_HOME", self.path("user"))
                .env_remove("CPF_STORAGE_CRASH_AT")
                .env_remove("CPF_STORAGE_PAUSE_AT")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .args(args);
            command
        }
        fn run(&self, args: &[&str]) -> Output {
            wait(self.command(args).spawn().unwrap())
        }
        fn jobs(&self) -> Vec<PathBuf> {
            let runs = self.path(".cpforge/runs");
            if !runs.exists() {
                return Vec::new();
            }
            let mut jobs: Vec<_> = fs::read_dir(runs)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            jobs.sort();
            jobs
        }
        fn report(&self) -> (PathBuf, Value) {
            let jobs = self.jobs();
            assert_eq!(jobs.len(), 1, "{jobs:?}");
            let bytes = fs::read(jobs[0].join("report.json")).unwrap();
            assert_eq!(wire::read::report_schema_prefix(&bytes).unwrap(), 1);
            (jobs[0].clone(), wire::read::read_json(&bytes).unwrap())
        }
        fn sequence(&self) -> Vec<u8> {
            fs::read(self.path(".cpforge/job-seq")).unwrap()
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // Drain both pipes concurrently; bound a stuck supervisor/compiler without
    // leaving a child behind or filling a pipe while waiting for process exit.
    fn wait(mut child: Child) -> Output {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out = stdout.map(|mut stdout| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).unwrap();
                bytes
            })
        });
        let err = stderr.map(|mut stderr| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).unwrap();
                bytes
            })
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > Duration::from_secs(150) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("cpf run exceeded 150 seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Output {
            status,
            stdout: out.map_or_else(Vec::new, |out| out.join().unwrap()),
            stderr: err.map_or_else(Vec::new, |err| err.join().unwrap()),
        }
    }
    fn status(output: &Output, code: i32) {
        assert_eq!(output.status.code(), Some(code), "{output:?}");
    }

    fn raw(report: &Value) -> &Value {
        report["executions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|execution| execution["role"]["kind"] == "raw")
            .unwrap()
    }
    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
    fn input(w: &Workspace, bytes: Vec<u8>) -> Output {
        let mut child = w.command(&["run"]).stdin(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || stdin.write_all(&bytes).unwrap());
        let output = wait(child);
        writer.join().unwrap();
        output
    }
    fn marker(w: &Workspace, path: &Path, child: &mut Child) {
        let start = Instant::now();
        while !path.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "cpf exited before readiness in {:?}",
                w.0
            );
            if start.elapsed() > Duration::from_secs(120) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("program never became ready");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn signal(child: &Child) {
        // SAFETY: the harness owns this unreaped cpf process.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
            0
        );
    }

    #[test]
    fn fresh_build_echoes_binary_stdin_exactly_and_persists_raw_spec() {
        let w = Workspace::new(Some("cpp"));
        w.write(
            "problems/A/main.cpp",
            "#include <iostream>\nint main() { std::cout << std::cin.rdbuf(); }\n",
        );
        // Raw mode never opens the test catalog.
        w.write("problems/A/tests.toml", "invalid test catalog");
        let mut bytes: Vec<u8> = (0..=255).cycle().take(256 * 1024).collect();
        bytes.extend_from_slice(b"no trailing newline");
        let output = input(&w, bytes.clone());
        status(&output, 0);
        assert_eq!(output.stdout, bytes);
        assert!(output.stderr.is_empty(), "{output:?}");
        let (dir, report) = w.report();
        assert_eq!(report["command"], "run");
        assert_eq!(report["job"]["kind"], "run");
        assert_eq!(report["build"]["result"]["kind"], "succeeded");
        assert!(report["tests"].as_array().unwrap().is_empty());
        assert!(report["target"]["compare"].is_null());
        let execution = raw(&report);
        assert_eq!(execution["summary"]["class"]["kind"], "exited_zero");
        assert_eq!(
            execution["summary"]["stdout"]["forwarded"],
            bytes.len().to_string()
        );
        assert_eq!(execution["summary"]["stdout"]["stored"], "0");
        assert!(execution["summary"]["stdout"]["file"].is_null());
        let spec: Value = wire::read::read_json(
            &fs::read(dir.join(execution["spec"]["path"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        assert_eq!(spec["stdin"]["kind"], "forward");
        assert_eq!(spec["stdout"]["sink"]["kind"], "forward");
        assert_eq!(spec["stderr"]["sink"]["stream"], "stderr");
        assert_eq!(spec["stdout"]["budget"], "67108864");
        assert_eq!(spec["stderr"]["budget"], "67108864");
        assert!(!dir.join("r00001/out/stdout").exists());
        // A second invocation builds new source in a separate job.
        w.write(
            "problems/A/main.cpp",
            "#include <iostream>\nint main() { std::cout << \"fresh\"; }\n",
        );
        let output = w.run(&["run"]);
        status(&output, 0);
        assert_eq!(output.stdout, b"fresh");
        assert_eq!(w.jobs().len(), 2);
    }

    #[test]
    fn stdout_and_stderr_are_forwarded_without_added_newlines() {
        let w = Workspace::new(Some("python"));
        w.write(
            "problems/A/main.py",
            "import os\nos.write(1, b'out\\x00\\xff')\nos.write(2, b'err\\x00\\xfe')\n",
        );
        let output = w.run(&["run"]);
        status(&output, 0);
        assert_eq!(output.stdout, b"out\x00\xff");
        assert_eq!(output.stderr, b"err\x00\xfe");
        let (_, report) = w.report();
        assert!(report["build"].is_null());
        assert_eq!(raw(&report)["summary"]["stderr"]["forwarded"], "5");
    }

    #[test]
    fn child_exit_three_uses_fixed_status_and_reports_actual_termination() {
        let w = Workspace::new(Some("cpp"));
        w.write(
            "problems/A/main.cpp",
            "#include <iostream>\nint main() { std::cerr << \"child stderr\\n\"; return 3; }\n",
        );
        let output = w.run(&["run"]);
        status(&output, 1);
        assert!(output.stdout.is_empty());
        let lines: Vec<_> = output
            .stderr
            .split(|b| *b == b'\n')
            .filter(|b| !b.is_empty())
            .collect();
        assert_eq!(lines.len(), 2, "{output:?}");
        assert_eq!(lines[0], b"child stderr");
        assert!(stderr(&output).contains("RE"));
        assert!(stderr(&output).contains("child exited with code 3"));
        let (_, report) = w.report();
        assert_eq!(report["result"]["verdict"], "nonzero_exit");
        assert_eq!(report["result"]["pre_delivery_exit"]["code"], 1);
        assert_eq!(raw(&report)["summary"]["execution"]["status"]["code"], 3);
        assert_eq!(raw(&report)["summary"]["class"]["code"], 3);
    }

    #[test]
    fn deadline_applies_to_run_and_leaves_build_deadline_alone() {
        let w = Workspace::new(Some("cpp"));
        w.write("problems/A/main.cpp", "#include <chrono>\n#include <thread>\nint main() { std::this_thread::sleep_for(std::chrono::seconds(30)); }\n");
        let output = w.run(&["run", "--deadline", "1s"]);
        status(&output, 1);
        assert!(stderr(&output).contains("TLE"));
        assert!(stderr(&output).contains("child killed by signal 9"));
        let (dir, report) = w.report();
        assert_eq!(report["result"]["verdict"], "wall_deadline");
        assert_eq!(raw(&report)["summary"]["class"]["kind"], "wall_deadline");
        let build: Value =
            wire::read::read_json(&fs::read(dir.join("r00000/spec.json")).unwrap()).unwrap();
        let run: Value =
            wire::read::read_json(&fs::read(dir.join("r00001/spec.json")).unwrap()).unwrap();
        assert_eq!(build["wall_deadline_us"], "90000000");
        assert_eq!(run["wall_deadline_us"], "1000000");
    }

    #[test]
    fn sigint_while_waiting_on_stdin_persists_cancellation_before_reraise() {
        let w = Workspace::new(Some("python"));
        let ready = w.path("ready");
        w.write("problems/A/main.py", &format!("import pathlib, sys\npathlib.Path({ready:?}).write_text('ready')\nsys.stdin.buffer.read()\n"));
        let mut child = w.command(&["run"]).stdin(Stdio::piped()).spawn().unwrap();
        marker(&w, &ready, &mut child);
        signal(&child);
        // Keep cpf's stdin open: its helper is allowed to stay blocked in read.
        let output = wait(child);
        assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
        assert!(stderr(&output).contains("CANCELLED"));
        assert!(stderr(&output).contains("child killed by signal 9"));
        let (_, report) = w.report();
        assert_eq!(report["result"]["verdict"], "cancelled");
        assert_eq!(report["result"]["pre_delivery_exit"]["reraise"], "sigint");
        assert_eq!(raw(&report)["summary"]["class"]["kind"], "cancelled");
    }

    #[test]
    fn several_mib_on_each_stream_are_forwarded_completely() {
        let w = Workspace::new(Some("python"));
        w.write("problems/A/main.py", "import os\nfor _ in range(96):\n os.write(1, b'o' * 65536)\n os.write(2, b'e' * 65536)\n");
        let output = w.run(&["run"]);
        status(&output, 0);
        assert_eq!(output.stdout, vec![b'o'; 6 * 1024 * 1024]);
        assert_eq!(output.stderr, vec![b'e'; 6 * 1024 * 1024]);
        let (_, report) = w.report();
        for stream in ["stdout", "stderr"] {
            assert_eq!(raw(&report)["summary"][stream]["forwarded"], "6291456");
            assert!(
                raw(&report)["summary"][stream]["losses"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn consumer_gone_records_forward_incomplete_and_does_not_fail_delivery() {
        let w = Workspace::new(Some("python"));
        w.write(
            "problems/A/main.py",
            "import os\nfor _ in range(96): os.write(1, b'x' * 65536)\n",
        );
        let mut child = w.command(&["run"]).spawn().unwrap();
        drop(child.stdout.take());
        let output = wait(child);
        status(&output, 10);
        assert!(stderr(&output).contains("INFRA"));
        assert!(stderr(&output).contains("child exited with code 0"));
        let (_, report) = w.report();
        assert_eq!(report["result"]["verdict"], "infrastructure");
        let losses = raw(&report)["summary"]["stdout"]["losses"]
            .as_array()
            .unwrap();
        assert!(
            losses
                .iter()
                .any(|loss| loss["kind"] == "forward_incomplete")
        );
        assert_eq!(raw(&report)["summary"]["execution"]["status"]["code"], 0);
    }

    #[test]
    fn stalled_console_has_bounded_flush_and_deadline_still_fires() {
        let w = Workspace::new(Some("python"));
        let ready = w.path("ready");
        w.write("problems/A/main.py", &format!("import os, pathlib\npathlib.Path({ready:?}).write_text('ready')\nwhile True: os.write(1, b'x' * 65536)\n"));
        let mut child = w.command(&["run", "--deadline", "1s"]).spawn().unwrap();
        // Keep the pipe's consumer alive without reading it, through process exit.
        let blocked = child.stdout.take().unwrap();
        marker(&w, &ready, &mut child);
        let start = Instant::now();
        let output = wait(child);
        assert!(start.elapsed() < Duration::from_secs(15), "{output:?}");
        status(&output, 10); // stdout loss outranks the recorded deadline.
        let (_, report) = w.report();
        let summary = &raw(&report)["summary"];
        assert_eq!(
            summary["execution"]["stop_trigger"]["kind"],
            "wall_deadline"
        );
        assert!(
            summary["stdout"]["losses"]
                .as_array()
                .unwrap()
                .iter()
                .any(|loss| loss["kind"] == "forward_incomplete")
        );
        drop(blocked);
    }

    #[test]
    fn stalled_stderr_cannot_block_the_final_summary_or_report() {
        let w = Workspace::new(Some("python"));
        let ready = w.path("ready");
        w.write("problems/A/main.py", &format!("import os, pathlib\npathlib.Path({ready:?}).write_text('ready')\nwhile True: os.write(2, b'x' * 65536)\n"));
        let mut child = w.command(&["run", "--deadline", "1s"]).spawn().unwrap();
        let blocked = child.stderr.take().unwrap();
        marker(&w, &ready, &mut child);
        let start = Instant::now();
        let output = wait(child);
        assert!(start.elapsed() < Duration::from_secs(15), "{output:?}");
        status(&output, 1); // stderr loss is evidence, not stdout infrastructure.
        let (_, report) = w.report();
        assert_eq!(report["result"]["verdict"], "wall_deadline");
        assert!(
            raw(&report)["summary"]["stderr"]["losses"]
                .as_array()
                .unwrap()
                .iter()
                .any(|loss| loss["kind"] == "forward_incomplete")
        );
        drop(blocked);
    }

    #[test]
    fn child_closing_stdin_can_finish_with_cpf_stdin_still_open() {
        let w = Workspace::new(Some("python"));
        w.write(
            "problems/A/main.py",
            "import os\nos.close(0)\nprint('done', end='')\n",
        );
        let start = Instant::now();
        let output = wait(w.command(&["run"]).stdin(Stdio::piped()).spawn().unwrap());
        assert!(start.elapsed() < Duration::from_secs(15), "{output:?}");
        status(&output, 0);
        assert_eq!(output.stdout, b"done");
        assert!(output.stderr.is_empty());
        let (_, report) = w.report();
        assert_eq!(raw(&report)["summary"]["class"]["kind"], "exited_zero");
    }

    #[test]
    fn second_sigint_forces_exit_during_console_drain() {
        let w = Workspace::new(Some("python"));
        let ready = w.path("ready");
        w.write("problems/A/main.py", &format!("import os, pathlib\npathlib.Path({ready:?}).write_text('ready')\nwhile True: os.write(1, b'x' * 65536)\n"));
        let mut child = w.command(&["run"]).spawn().unwrap();
        let blocked = child.stdout.take().unwrap();
        marker(&w, &ready, &mut child);
        // Fill the console pipe and forwarding queue before cancellation.
        std::thread::sleep(Duration::from_millis(200));
        signal(&child);
        std::thread::sleep(Duration::from_millis(150));
        signal(&child);
        let output = wait(child);
        assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
        assert!(!stderr(&output).contains("CANCELLED"));
        drop(blocked);
    }

    #[test]
    fn machine_options_and_bad_configuration_refuse_before_admission() {
        let w = Workspace::new(Some("python"));
        let sequence = w.sequence();
        for mode in ["--json", "--events", "--events=output"] {
            let output = w.run(&["run", mode]);
            status(&output, 2);
            assert!(output.stdout.is_empty());
            assert!(w.jobs().is_empty());
            assert_eq!(w.sequence(), sequence);
        }
        for deadline in ["0", "-1s", "nan", "86401s"] {
            let output = w.run(&["run", "--deadline", deadline]);
            status(&output, 7);
            assert!(output.stdout.is_empty());
            assert!(w.jobs().is_empty());
            assert_eq!(w.sequence(), sequence);
        }
    }

    #[test]
    fn compile_error_skips_raw_run_and_keeps_compiler_output_in_capture() {
        let w = Workspace::new(Some("cpp"));
        let ran = w.path("ran");
        w.write(
            "problems/A/main.cpp",
            &format!(
                "#include <fstream>\nint main() {{ std::ofstream({:?}) << 1; syntax error }}\n",
                ran.to_str().unwrap()
            ),
        );
        let output = w.run(&["run"]);
        status(&output, 3);
        assert!(output.stdout.is_empty());
        assert!(stderr(&output).contains("CE"));
        assert!(!ran.exists());
        let (dir, report) = w.report();
        assert_eq!(report["result"]["verdict"], "build_failed");
        assert_eq!(raw(&report)["coverage"]["kind"], "skipped");
        assert_eq!(raw(&report)["coverage"]["reason"], "build_not_succeeded");
        assert!(raw(&report)["run"].is_null());
        assert!(fs::metadata(dir.join("r00000/out/stderr")).unwrap().len() > 0);
    }

    #[test]
    fn python_echo_profile_and_global_problem_work() {
        let w = Workspace::new(Some("python"));
        let bytes = b"Python raw echo without newline\x00\xff".to_vec();
        let output = input(&w, bytes.clone());
        status(&output, 0);
        assert_eq!(output.stdout, bytes);
        assert!(output.stderr.is_empty());
        w.write("cpforge.toml", "schema = 1\n[languages.python.profiles.marker.run]\nenv = { CPF_TEST_PROFILE = 'chosen' }\n");
        w.write(
            "problems/A/main.py",
            "import os, sys\nsys.stdout.write(os.environ['CPF_TEST_PROFILE'])\n",
        );
        let output = wait(
            w.command(&["--problem", "local:A", "run", "--profile", "marker"])
                .current_dir(&w.0)
                .spawn()
                .unwrap(),
        );
        status(&output, 0);
        assert_eq!(output.stdout, b"chosen");
    }
}

#[cfg(windows)]
#[test]
fn windows_refuses_raw_run_before_workspace_admission() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cpf"))
        .args(["run"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(10), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported platform"));
}
