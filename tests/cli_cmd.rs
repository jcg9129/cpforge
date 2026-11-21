use clap::Parser;
use cpforge::cli::{Cli, Command};

#[test]
fn command_options_and_literal_extra_arguments() {
    for mode in ["--json", "--events", "--events=output"] {
        let cli = Cli::try_parse_from([
            "cpf",
            "--problem",
            "local:A",
            "cmd",
            "bench",
            "--profile",
            "debug",
            mode,
            "--",
            "{sources}",
            "--json",
            "a b",
            "",
        ])
        .unwrap();
        let Some(Command::Cmd { extra, .. }) = cli.command else {
            panic!("cmd expected")
        };
        assert_eq!(
            extra,
            ["{sources}", "--json", "a b", ""].map(std::ffi::OsString::from)
        );
    }
    for args in [
        vec!["cpf", "cmd"],
        vec!["cpf", "cmd", "x", "extra"],
        vec!["cpf", "cmd", "x", "--json", "--events"],
        vec!["cpf", "cmd", "x", "--events=status"],
    ] {
        assert_eq!(Cli::try_parse_from(args).unwrap_err().exit_code(), 2);
    }
}

#[cfg(windows)]
#[test]
fn windows_refuses_execution_before_workspace_discovery() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cpf"))
        .args(["cmd", "anything", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(10), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["job"].is_null());
    assert_eq!(report["refusal"]["class"], "infrastructure");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod execution {
    use cpforge::report::wire;
    use serde_json::Value;
    use std::fs;
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Output, Stdio};
    use std::time::{Duration, Instant};

    struct Workspace(PathBuf);
    impl Workspace {
        fn new(command: &str) -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!(
                    "cli-cmd-{}",
                    cpforge::build::BuildId::generate().unwrap()
                ));
            fs::create_dir_all(&path).unwrap();
            let w = Self(cpforge::storage::paths::plain(
                &fs::canonicalize(path).unwrap(),
            ));
            w.write(
                "cpforge.toml",
                &format!("schema = 1\n[commands.x]\n{command}\n"),
            );
            w.write(
                ".cpforge/job-seq",
                "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
            );
            w.write("user/cpforge/config.toml", "schema = 1\n");
            w.write(
                "problems/A/problem.toml",
                "schema = 1\n[problem]\nid = 'local:A'\n",
            );
            w
        }
        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }
        fn write(&self, relative: &str, text: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
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
        fn tests(&self, entries: &[(&str, &str, Option<&str>, bool)]) {
            let mut catalog = "schema = 1\n".to_owned();
            for (id, input, answer, enabled) in entries {
                self.write(&format!("problems/A/tests/{id}.in"), input);
                catalog.push_str(&format!("[[test]]\nid = '{id}'\ninput = 'tests/{id}.in'\norigin = 'custom'\nenabled = {enabled}\n"));
                if let Some(answer) = answer {
                    self.write(&format!("problems/A/tests/{id}.ans"), answer);
                    catalog.push_str(&format!("answer = 'tests/{id}.ans'\n"));
                }
            }
            self.write("problems/A/tests.toml", &catalog);
        }
        fn job(&self) -> PathBuf {
            let jobs: Vec<_> = fs::read_dir(self.path(".cpforge/runs"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            assert_eq!(jobs.len(), 1, "{jobs:?}");
            jobs.into_iter().next().unwrap()
        }
        fn stdout(&self, report: &Value, index: usize) -> Vec<u8> {
            let run = report["executions"][index]["run"].as_str().unwrap();
            fs::read(self.job().join(run).join("out/stdout")).unwrap()
        }
        fn refused(&self, output: &Output, code: i32) -> Value {
            status(output, code);
            let report = document(output);
            assert!(report["job"].is_null(), "{report}");
            assert!(!self.path(".cpforge/runs").exists());
            assert_eq!(
                fs::read_to_string(self.path(".cpforge/job-seq")).unwrap(),
                "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n"
            );
            report
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn wait(mut child: Child) -> Output {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let drain = |pipe: Option<std::process::ChildStdout>| {
            pipe.map(|mut pipe| {
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    pipe.read_to_end(&mut bytes).unwrap();
                    bytes
                })
            })
        };
        let out = drain(stdout);
        let err = stderr.map(|mut pipe| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                pipe.read_to_end(&mut bytes).unwrap();
                bytes
            })
        });
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > Duration::from_secs(90) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("command timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Output {
            status,
            stdout: out.map_or_else(Vec::new, |t| t.join().unwrap()),
            stderr: err.map_or_else(Vec::new, |t| t.join().unwrap()),
        }
    }
    fn status(output: &Output, expected: i32) {
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
    }
    fn document(output: &Output) -> Value {
        assert_eq!(wire::read::report_schema_prefix(&output.stdout).unwrap(), 1);
        wire::read::read_json(&output.stdout).unwrap()
    }

    #[test]
    fn plain_command_has_no_target_or_problem_and_runs_outside_a_problem_directory() {
        let w = Workspace::new("argv = ['sh', '-c', 'echo hi']");
        // No selection and no language; also ignore an unusable saved selection.
        w.write(
            ".cpforge/state.toml",
            "schema = 1\ncurrent_problem = 'local:missing'\n",
        );
        let output = wait(
            w.command(&["cmd", "x", "--json"])
                .current_dir(&w.0)
                .spawn()
                .unwrap(),
        );
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["command"], "cmd");
        assert!(report["problem"].is_null());
        assert!(report["target"].is_null());
        assert!(report["capture"].is_null());
        assert!(report["build"].is_null());
        assert_eq!(w.stdout(&report, 0), b"hi\n");
        assert_eq!(report["aggregates"][0]["iterations"], 1);
        assert_eq!(report["aggregates"][0]["succeeded"], 1);
        let persisted = fs::read(w.job().join("report.json")).unwrap();
        assert_eq!(wire::read::report_schema_prefix(&persisted).unwrap(), 1);
    }

    #[test]
    fn command_execution_releases_the_workspace_lock() {
        let w = Workspace::new("argv = ['sh', '-c', '\"$1\" switch A', 'command']");
        let output = w.run(&["cmd", "x", "--json", "--", env!("CARGO_BIN_EXE_cpf")]);
        status(&output, 0);
        assert_eq!(w.stdout(&document(&output), 0), b"local:A\n");
        assert!(
            fs::read_to_string(w.path(".cpforge/state.toml"))
                .unwrap()
                .contains("local:A")
        );
    }

    #[test]
    fn extra_arguments_are_verbatim_for_argv_and_shell() {
        for command in [
            "argv = ['sh', '-c', 'printf \"<%s>\\n\" \"$@\"', 'command', 'configured']",
            "shell = { interpreter = ['sh', '-c'], script = 'printf \"<%s>\\n\" \"$@\"', args = ['configured'] }",
        ] {
            let w = Workspace::new(command);
            let extra = [
                "{sources}",
                "a b",
                "\"quote\"",
                "back\\slash",
                "$HOME",
                "line\nbreak",
                "",
                "--json",
            ];
            let mut args = vec!["cmd", "x", "--json", "--"];
            args.extend(extra);
            let output = w.run(&args);
            status(&output, 0);
            let report = document(&output);
            let expected = std::iter::once("configured")
                .chain(extra)
                .map(|s| format!("<{s}>\n"))
                .collect::<String>();
            assert_eq!(w.stdout(&report, 0), expected.as_bytes());
        }
    }

    #[test]
    fn frozen_tests_stdin_paths_and_test_major_iteration_order() {
        let w = Workspace::new(
            "for_each_test = true\nrepeat = 3\nshell = { interpreter = ['sh', '-c'], script = 'test ! -e seen || exit 9; touch seen; printf \"%s:\" \"$3\"; cat; cat \"$1\"; cat \"$2\"', args = ['{test_input}', '{test_answer}', '{iteration}'] }",
        );
        w.tests(&[
            ("z", "z-input\n", Some("z-answer\n"), true),
            ("disabled", "bad", None, false),
            ("a", "a-input\n", Some("a-answer\n"), true),
        ]);
        let output = w.run(&["cmd", "x", "--json"]);
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["executions"].as_array().unwrap().len(), 6);
        assert_eq!(report["tests"].as_array().unwrap().len(), 2);
        for (index, id) in ["z", "a"].into_iter().enumerate() {
            let test = &report["tests"][index];
            assert_eq!(test["id"], id);
            assert!(test["check"].is_null());
            assert!(test["verdict"].is_null());
            assert!(test["class"].is_null());
            for iteration in 1..=3 {
                let execution = index * 3 + iteration - 1;
                assert_eq!(report["executions"][execution]["role"]["test"], id);
                assert_eq!(
                    report["executions"][execution]["role"]["iteration"],
                    iteration
                );
                assert_eq!(
                    w.stdout(&report, execution),
                    format!("{iteration}:{id}-input\n{id}-input\n{id}-answer\n").as_bytes()
                );
                let spec: Value = serde_json::from_slice(
                    &fs::read(w.job().join(format!("r{execution:05}/spec.json"))).unwrap(),
                )
                .unwrap();
                assert!(
                    spec["stdin"]["path"]["utf8"]
                        .as_str()
                        .unwrap()
                        .contains("/inputs/")
                );
            }
            assert_eq!(report["aggregates"][index]["test"], id);
            assert_eq!(report["aggregates"][index]["succeeded"], 3);
        }
    }

    #[test]
    fn null_stdin_overrides_test_input() {
        let w = Workspace::new(
            "for_each_test = true\nstdin = 'null'\nargv = ['sh', '-c', 'test -z \"$(cat)\"']",
        );
        w.tests(&[("one", "nonempty\n", None, true)]);
        status(&w.run(&["cmd", "x"]), 0);
    }

    #[test]
    fn first_failure_stops_the_product_unless_keep_going() {
        for keep_going in [false, true] {
            let w = Workspace::new(&format!(
                "for_each_test = true\nrepeat = 3\nkeep_going = {keep_going}\nshell = {{ interpreter = ['sh', '-c'], script = 'test \"$1\" != 2', args = ['{{iteration}}'] }}"
            ));
            w.tests(&[("one", "", None, true), ("two", "", None, true)]);
            let output = w.run(&["cmd", "x", "--json"]);
            status(&output, 1);
            let report = document(&output);
            assert_eq!(
                report["executions"][1]["summary"]["class"]["kind"],
                "nonzero_exit"
            );
            for execution in report["executions"].as_array().unwrap().iter().skip(2) {
                assert_eq!(
                    execution["coverage"]["kind"],
                    if keep_going { "started" } else { "skipped" }
                );
                if !keep_going {
                    assert_eq!(execution["coverage"]["reason"], "stop_policy");
                    assert!(execution["run"].is_null());
                }
            }
            assert_eq!(report["aggregates"][0]["iterations"], 3);
            assert_eq!(
                report["aggregates"][0]["succeeded"],
                if keep_going { 2 } else { 1 }
            );
            assert_eq!(
                report["aggregates"][1]["succeeded"],
                if keep_going { 2 } else { 0 }
            );
            if !keep_going {
                assert!(report["aggregates"][1]["wall_us"].is_null());
            }
        }
    }

    #[test]
    fn retention_refusal_is_infrastructure_and_closes_later_admission() {
        let w = Workspace::new("repeat = 3\nargv = ['sh', '-c', 'exit 0']");
        w.write("cpforge.toml", "schema = 1\n[retention]\njob_bytes = '1MiB'\n[commands.x]\nrepeat = 3\nargv = ['sh', '-c', 'exit 0']\n");
        let output = w.run(&["cmd", "x", "--json"]);
        status(&output, 10);
        let report = document(&output);
        assert_eq!(report["result"]["verdict"], "infrastructure");
        assert_eq!(report["executions"][0]["coverage"]["kind"], "not_started");
        for execution in report["executions"].as_array().unwrap().iter().skip(1) {
            assert_eq!(execution["coverage"]["reason"], "admission_closed");
        }
    }

    #[test]
    fn zero_enabled_tests_and_unavailable_answer_are_plan_refusals() {
        for (tests, command) in [
            (false, "for_each_test = true\nargv = ['sh', '-c', 'exit 0']"),
            (
                true,
                "for_each_test = true\nargv = ['sh', '-c', 'exit 0', '{test_answer}']",
            ),
        ] {
            let w = Workspace::new(command);
            w.tests(&[("one", "input", None, tests)]);
            w.refused(&w.run(&["cmd", "x", "--json"]), 7);
        }
    }

    #[test]
    fn unsupported_command_limits_are_refused_before_admission() {
        let w = Workspace::new("argv = ['sh', '-c', 'exit 0']\nlimits = { job_commit = 4096 }");
        w.refused(&w.run(&["cmd", "x", "--json"]), 7);
    }

    #[test]
    fn compile_error_skips_every_iteration_and_builds_only_once() {
        let w = Workspace::new(
            "needs_build = true\nfor_each_test = true\nrepeat = 3\nargv = ['sh', '-c', 'echo should-not-run']",
        );
        // A build stage that reports a compile error and exits non-zero.
        w.write(
            "user/cpforge/config.toml",
            "schema = 1\n[languages.cpp.build]\nargv = ['sh', '-c', 'echo \"main.cpp:1: error\" >&2; exit 1']\n",
        );
        w.write(
            "problems/A/problem.toml",
            "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\n",
        );
        w.write("problems/A/main.cpp", "this is a compile error\n");
        w.tests(&[("one", "", None, true), ("two", "", None, true)]);
        let output = w.run(&["cmd", "x", "--json"]);
        status(&output, 3);
        let report = document(&output);
        assert_eq!(report["executions"].as_array().unwrap().len(), 7);
        assert_eq!(report["executions"][0]["stage"], "build");
        for execution in report["executions"].as_array().unwrap().iter().skip(1) {
            assert_eq!(execution["coverage"]["reason"], "build_not_succeeded");
            assert!(execution["run"].is_null());
        }
    }

    #[test]
    fn successful_build_is_outside_the_product_and_binds_generated_program_and_sources() {
        let w = Workspace::new(
            "needs_build = true\nrepeat = 3\nargv = ['{artifact}', '{iteration}', '{source}']",
        );
        w.write(
            "problems/A/problem.toml",
            "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\n",
        );
        w.write(
            "problems/A/main.cpp",
            "#!/bin/sh\ntest -r \"$2\" || exit 9\nprintf '%s\\n' \"$1\"\n",
        );
        w.write("user/cpforge/config.toml", "schema = 1\n[languages.cpp.profiles.custom.build]\nshell = { interpreter = ['sh', '-c'], script = 'cp \"$1\" \"$2\"; chmod +x \"$2\"; printf compiled', args = ['{source}', '{artifact}'] }\n");
        let output = w.run(&["cmd", "x", "--profile", "custom", "--json"]);
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["executions"].as_array().unwrap().len(), 4);
        assert_eq!(report["build"]["result"]["kind"], "succeeded");
        assert_eq!(w.stdout(&report, 0), b"compiled");
        assert_eq!(report["target"]["profile"], "custom");
        for iteration in 1..=3 {
            assert_eq!(
                w.stdout(&report, iteration),
                format!("{iteration}\n").as_bytes()
            );
        }
        assert_eq!(report["aggregates"][0]["iterations"], 3);
        assert_eq!(report["aggregates"][0]["succeeded"], 3);
    }

    #[test]
    fn edits_after_freeze_do_not_change_any_iteration_input_or_answer() {
        let w = Workspace::new(
            "for_each_test = true\nrepeat = 2\nshell = { interpreter = ['sh', '-c'], script = 'printf changed > \"$3/tests/one.in\"; printf changed > \"$3/tests/one.ans\"; cat; cat \"$1\"; cat \"$2\"', args = ['{test_input}', '{test_answer}', '{problem_dir}'] }",
        );
        w.tests(&[("one", "input\n", Some("answer\n"), true)]);
        let output = w.run(&["cmd", "x", "--json"]);
        status(&output, 0);
        let report = document(&output);
        for index in 0..2 {
            assert_eq!(w.stdout(&report, index), b"input\ninput\nanswer\n");
        }
        assert_eq!(
            fs::read(w.path("problems/A/tests/one.in")).unwrap(),
            b"changed"
        );
    }

    #[test]
    fn cancelling_an_iteration_skips_remaining_iterations_as_cancelled() {
        for keep_going in [false, true] {
            let w = Workspace::new(&format!(
                "repeat = 3\nkeep_going = {keep_going}\nargv = ['sh', '-c', 'kill -INT \"$PPID\"; sleep 10']"
            ));
            let output = w.run(&["cmd", "x", "--json"]);
            assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
            let report = document(&output);
            assert_eq!(report["result"]["verdict"], "cancelled");
            for execution in report["executions"].as_array().unwrap().iter().skip(1) {
                assert_eq!(execution["coverage"]["reason"], "cancelled");
                assert!(execution["run"].is_null());
            }
        }
    }

    #[test]
    fn planned_run_cap_accepts_ten_thousand_and_rejects_ten_thousand_and_one_before_admission() {
        // The legal product reaches answer validation; the larger product fails
        // the cap first. Neither case executes or creates a job directory.
        for (count, repeat, build, capped) in [
            (101, 99, false, false),
            (10, 1000, false, false),
            (137, 73, false, true),
            (101, 99, true, false),
            (10, 1000, true, true),
        ] {
            let w = Workspace::new(&format!(
                "needs_build = {build}\nfor_each_test = true\nrepeat = {repeat}\nargv = ['sh', '-c', 'exit 0', '{{test_answer}}']"
            ));
            if build {
                w.write(
                    "problems/A/problem.toml",
                    "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\n",
                );
                w.write("problems/A/main.cpp", "int main() {}\n");
                w.write(
                    "user/cpforge/config.toml",
                    "schema = 1\n[languages.cpp.build]\nargv = ['sh', '-c', 'exit 0']\n",
                );
            }
            let ids: Vec<_> = (0..count).map(|n| format!("t{n}")).collect();
            let entries: Vec<_> = ids.iter().map(|id| (id.as_str(), "", None, true)).collect();
            w.tests(&entries);
            let output = w.run(&["cmd", "x", "--json"]);
            w.refused(&output, 7);
            let error = String::from_utf8_lossy(&output.stderr);
            assert_eq!(error.contains("10 000 execution cap"), capped, "{error}");
            if !capped {
                assert!(error.contains("test_answer"), "{error}");
            }
        }
    }

    #[test]
    fn json_aggregates_use_the_lower_middle_successful_wall_time() {
        let w = Workspace::new(
            "repeat = 4\nshell = { interpreter = ['sh', '-c'], script = 'case \"$1\" in 1) sleep 0.02;; 2) sleep 0.08;; 3) sleep 0.04;; 4) sleep 0.06;; esac', args = ['{iteration}'] }",
        );
        let output = w.run(&["cmd", "x", "--json"]);
        status(&output, 0);
        let report = document(&output);
        let mut walls: Vec<_> = report["executions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                e["summary"]["timing"]["wall_us"]
                    .as_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
            })
            .collect();
        walls.sort_unstable();
        let aggregate = &report["aggregates"][0];
        assert_eq!(aggregate["iterations"], 4);
        assert_eq!(aggregate["succeeded"], 4);
        for (field, value) in [("min", walls[0]), ("median", walls[1]), ("max", walls[3])] {
            assert_eq!(aggregate["wall_us"][field], value.to_string());
        }
    }

    #[test]
    fn cwd_overflow_is_a_recorded_warning_and_preserves_success() {
        let w = Workspace::new(
            "repeat = 3\nshell = { interpreter = ['sh', '-c'], script = 'if test \"$1\" = 2; then printf 123456789 > large; fi', args = ['{iteration}'] }",
        );
        w.write("cpforge.toml", "schema = 1\n[retention]\ncwd_bytes = 4\n[commands.x]\nrepeat = 3\nshell = { interpreter = ['sh', '-c'], script = 'if test \"$1\" = 2; then printf 123456789 > large; fi', args = ['{iteration}'] }\n");
        let output = w.run(&["cmd", "x", "--json"]);
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["result"]["verdict"], "succeeded");
        assert_eq!(report["aggregates"][0]["succeeded"], 3);
        assert_eq!(
            report["executions"][1]["summary"]["cwd_overflow_bytes"],
            "9"
        );
        assert!(
            report["executions"][1]["summary"]["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n == "cwd_overflow")
        );
        assert!(
            report["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["detail"] == "cwd_overflow")
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("CwdOverflow"));
        assert!(!w.job().join("r00001/cwd/large").exists());
    }

    #[test]
    fn problem_identity_and_cwd_require_context_without_requiring_a_language() {
        for command in [
            "argv = ['sh', '-c', 'exit 0', '{problem_id}']",
            "argv = ['sh', '-c', 'exit 0']\ncwd = { selector = 'problem' }",
        ] {
            let w = Workspace::new(command);
            let output = wait(
                w.command(&["cmd", "x", "--json"])
                    .current_dir(&w.0)
                    .spawn()
                    .unwrap(),
            );
            // Missing context is a plan error, not a selection error.
            let report = w.refused(&output, 7);
            assert_eq!(report["refusal"]["class"], "config", "{report}");
        }
        let w =
            Workspace::new("argv = ['sh', '-c', 'printf %s \"$1\"', 'command', '{problem_id}']");
        w.write(
            "problems/A/problem.toml",
            "schema = 1\n[problem]\nid = 'local:A'\nkind = 'interactive'\n",
        );
        let output = wait(
            w.command(&["--problem", "A", "cmd", "x", "--json"])
                .current_dir(&w.0)
                .spawn()
                .unwrap(),
        );
        status(&output, 0);
        assert_eq!(w.stdout(&document(&output), 0), b"local:A");
    }

    #[test]
    fn cwd_path_and_workspace_selector_derive_their_context_requirements() {
        let w = Workspace::new("argv = ['sh', '-c', 'pwd']\ncwd = { path = '{problem_dir}' }");
        let output = w.run(&["cmd", "x", "--json"]);
        status(&output, 0);
        assert_eq!(
            String::from_utf8(w.stdout(&document(&output), 0))
                .unwrap()
                .trim(),
            w.path("problems/A").to_str().unwrap()
        );
        let w = Workspace::new("argv = ['sh', '-c', 'pwd']\ncwd = { selector = 'workspace' }");
        let output = wait(
            w.command(&["cmd", "x", "--json"])
                .current_dir(&w.0)
                .spawn()
                .unwrap(),
        );
        status(&output, 0);
        let report = document(&output);
        assert!(report["problem"].is_null());
        assert_eq!(
            String::from_utf8(w.stdout(&report, 0)).unwrap().trim(),
            w.0.to_str().unwrap()
        );
    }

    #[test]
    fn outside_a_workspace_refuses_without_creating_job_storage() {
        let w = Workspace::new("argv = ['sh', '-c', 'exit 0']");
        fs::remove_file(w.path("cpforge.toml")).unwrap();
        let output = wait(
            w.command(&["cmd", "x", "--json"])
                .current_dir(&w.0)
                .spawn()
                .unwrap(),
        );
        w.refused(&output, 8);
    }

    #[test]
    fn events_use_the_shared_command_transport() {
        for mode in ["--events", "--events=output"] {
            let w = Workspace::new("argv = ['sh', '-c', 'echo hi']");
            let output = w.run(&["cmd", "x", mode]);
            status(&output, 0);
            let lines: Vec<Value> = output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|b| !b.is_empty())
                .map(|b| wire::read::read_json(b).unwrap())
                .collect();
            assert!(lines.iter().any(|v| v["event"] == "run_started"));
            assert_eq!(
                lines.iter().any(|v| v["event"] == "output_chunk"),
                mode == "--events=output"
            );
        }
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn sigint_between_iterations_delivers_cancelled_coverage_before_signal_death() {
        use std::io::{BufRead, BufReader, Write};
        for keep_going in [false, true] {
            let w = Workspace::new(&format!(
                "repeat = 3\nkeep_going = {keep_going}\nargv = ['sh', '-c', 'exit 0']"
            ));
            let mut child = w
                .command(&["cmd", "x", "--json"])
                .env("CPF_STORAGE_PAUSE_AT", "command.between_iterations")
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap());
            let mut marker = String::new();
            stdout.read_line(&mut marker).unwrap();
            assert_eq!(marker.trim(), "CPF-PAUSED command.between_iterations");
            // SAFETY: signal the live child process owned by this test.
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
            child.stdin.take().unwrap().write_all(b"x").unwrap();
            let reader = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).unwrap();
                bytes
            });
            let mut output = wait(child);
            output.stdout = reader.join().unwrap();
            assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
            let report = document(&output);
            assert_eq!(report["result"]["verdict"], "cancelled");
            assert_eq!(report["executions"][0]["coverage"]["kind"], "started");
            for execution in report["executions"].as_array().unwrap().iter().skip(1) {
                assert_eq!(execution["coverage"]["reason"], "cancelled");
                assert!(execution["run"].is_null());
            }
            let persisted: Value =
                serde_json::from_slice(&fs::read(w.job().join("report.json")).unwrap()).unwrap();
            assert_eq!(persisted["executions"], report["executions"]);
        }
    }
}
