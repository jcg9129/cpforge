use clap::Parser;
use cpforge::cli::Cli;

#[test]
fn test_job_options_and_catalog_subcommands_parse_without_positional_cases() {
    for mode in ["--json", "--events", "--events=output"] {
        Cli::try_parse_from([
            "cpf",
            "--problem",
            "local:A",
            "test",
            "--case",
            "a",
            "--case",
            "b",
            "--profile",
            "debug",
            "--deadline",
            "1s",
            mode,
        ])
        .unwrap();
    }
    for args in [
        vec!["cpf", "test", "custom-1"],
        vec!["cpf", "test", "--json", "--events"],
        vec!["cpf", "test", "--events=status"],
    ] {
        assert_eq!(Cli::try_parse_from(args).unwrap_err().exit_code(), 2);
    }
}

#[test]
fn deadline_uses_config_rounding_and_bounds() {
    for (text, ms) in [
        ("1s", 1000),
        ("2003.5ms", 2004),
        ("0.0005", 1),
        ("1m", 60000),
        ("86400s", 86400000),
    ] {
        assert_eq!(cpforge::config::parse_deadline(text), Ok(ms));
    }
    for text in [
        "0",
        "-1s",
        "nan",
        "inf",
        "1h",
        "0.0004s",
        "86400.001s",
        "1e999s",
    ] {
        assert!(cpforge::config::parse_deadline(text).is_err(), "{text}");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod execution {
    use cpforge::report::wire;
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::Read;
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
                    "cli-test-run-{}",
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
            workspace.write("problems/A/main.cpp", "#include <iostream>\n#include <fstream>\nint main() { if (std::ifstream(\"seen\").good()) return 9; std::ofstream(\"seen\") << 1; int n; std::cin >> n; std::cout << n << std::endl; }\n");
            workspace.write("problems/A/main.py", "print(input())\n");
            workspace.write("problems/A/Main.java", "public class Main { public static void main(String[] args) { System.out.println(new java.util.Scanner(System.in).nextLine()); } }\n");
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
        fn cases(&self, entries: &[(&str, &str, Option<&str>, bool)]) {
            let mut catalog = String::from("schema = 1\n");
            for (id, input, answer, enabled) in entries {
                self.write(&format!("problems/A/tests/{id}.in"), input);
                catalog.push_str(&format!("\n[[test]]\nid = '{id}'\ninput = 'tests/{id}.in'\norigin = 'custom'\nenabled = {enabled}\n"));
                if let Some(answer) = answer {
                    self.write(&format!("problems/A/tests/{id}.ans"), answer);
                    catalog.push_str(&format!("answer = 'tests/{id}.ans'\n"));
                }
            }
            self.write("problems/A/tests.toml", &catalog);
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
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let err = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > Duration::from_secs(150) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("cpf test exceeded 150 seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        }
    }
    fn status(output: &Output, code: i32) {
        assert_eq!(output.status.code(), Some(code), "{output:?}");
    }
    fn document(output: &Output) -> Value {
        assert_eq!(output.stdout.last(), Some(&b'\n'), "{output:?}");
        assert_eq!(
            output.stdout.iter().filter(|b| **b == b'\n').count(),
            1,
            "{output:?}"
        );
        assert_eq!(wire::read::report_schema_prefix(&output.stdout).unwrap(), 1);
        wire::read::read_json(&output.stdout).unwrap()
    }
    fn verdicts(report: &Value) -> Vec<&str> {
        report["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["verdict"]["kind"].as_str().unwrap())
            .collect()
    }
    fn stderr(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
    fn wait_for_marker(w: &Workspace, marker: &Path, child: &mut Child) {
        let start = Instant::now();
        while !marker.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "cpf exited before marker in {:?}",
                w.0
            );
            if start.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("solution did not reach marker");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn cpp_mixed_verdicts_use_frozen_hashes_and_fresh_working_directories() {
        let w = Workspace::new(Some("cpp"));
        w.cases(&[
            ("custom-1", "5\n", Some("5\n"), true),
            ("custom-2", "7\n", Some("5\n"), true),
            ("custom-3", "9\n", None, true),
        ]);
        let output = w.run(&["test", "--json"]);
        status(&output, 1);
        let text = stderr(&output);
        for label in [
            "custom-1  OK",
            "custom-2  WA",
            "custom-3  UNCHECKED",
            "not verified",
            "1/3 passed (tokens comparison)",
        ] {
            assert!(text.contains(label), "{text}");
        }
        assert!(!text.contains("accepted"));
        let (dir, report) = w.report();
        assert_eq!(verdicts(&report), ["passed", "wrong_answer", "unchecked"]);
        assert_eq!(
            report["tests"][0]["input_sha256"],
            format!("{:x}", Sha256::digest(b"5\n"))
        );
        assert_eq!(
            report["tests"][0]["answer_sha256"],
            format!("{:x}", Sha256::digest(b"5\n"))
        );
        assert!(report["tests"][2]["answer_sha256"].is_null());
        for index in 1..=3 {
            assert!(dir.join(format!("r{index:05}/cwd/seen")).is_file());
            assert!(dir.join(format!("r{index:05}/out/stdout")).is_file());
            assert!(dir.join(format!("r{index:05}/out/stderr")).is_file());
        }
        let mut delivered = document(&output);
        delivered.as_object_mut().unwrap().remove("persistence");
        assert_eq!(delivered, report);
    }

    #[test]
    fn cpp_all_passed_and_all_unchecked_have_distinct_statuses_and_summaries() {
        for answer in [Some("5\n"), None] {
            let w = Workspace::new(Some("cpp"));
            w.cases(&[("one", "5\n", answer, true), ("two", "5\n", answer, true)]);
            let output = w.run(&["test"]);
            status(&output, if answer.is_some() { 0 } else { 4 });
            assert!(output.stdout.is_empty());
            assert!(stderr(&output).contains(if answer.is_some() {
                "2/2 passed (tokens comparison)"
            } else {
                "2 unchecked (no answer): not verified"
            }));
        }
    }

    #[test]
    fn deadline_and_crash_are_run_verdicts_with_diagnostic_checks() {
        for (source, expected, label) in [
            (
                "#include <unistd.h>\n#include <iostream>\nint main() { std::cout << 5 << std::endl; sleep(30); }\n",
                "wall_deadline",
                "TLE  wall deadline 1s",
            ),
            (
                "#include <cstdlib>\nint main() { std::abort(); }\n",
                "signal",
                "RE  signal",
            ),
        ] {
            let w = Workspace::new(Some("cpp"));
            w.write("problems/A/main.cpp", source);
            w.cases(&[("one", "5\n", Some("5\n"), true)]);
            let output = w.run(&["test", "--deadline", "1s", "--json"]);
            status(&output, 1);
            assert!(stderr(&output).contains(label), "{}", stderr(&output));
            let (dir, report) = w.report();
            assert_eq!(verdicts(&report), [expected]);
            assert_eq!(report["tests"][0]["diagnostic"], true);
            let build_spec =
                wire::read::read_json(&fs::read(dir.join("r00000/spec.json")).unwrap()).unwrap();
            let run_spec =
                wire::read::read_json(&fs::read(dir.join("r00001/spec.json")).unwrap()).unwrap();
            assert_eq!(build_spec["wall_deadline_us"], "90000000");
            assert_eq!(run_spec["wall_deadline_us"], "1000000");
            if expected == "wall_deadline" {
                assert!(stderr(&output).contains("(output matched)"));
                assert_eq!(report["tests"][0]["check"]["kind"], "passed");
            }
        }
    }

    #[test]
    fn compile_failure_marks_every_test_not_run_and_cannot_reuse_prior_artifacts() {
        let w = Workspace::new(Some("cpp"));
        w.cases(&[
            ("one", "5\n", Some("5\n"), true),
            ("two", "6\n", Some("6\n"), true),
        ]);
        status(&w.run(&["test"]), 0);
        let (first, _) = w.report();
        let original = fs::read(first.join("builds/r00000/main")).unwrap();
        w.write("problems/A/main.cpp", "int main() { syntax error }\n");
        let output = w.run(&["test", "--json"]);
        status(&output, 3);
        let report = document(&output);
        assert_eq!(verdicts(&report), ["not_run", "not_run"]);
        assert_eq!(report["result"]["verdict"], "build_failed");
        assert!(stderr(&output).contains("CE"));
        assert!(stderr(&output).contains("one  NOT RUN  build not succeeded"));
        assert_eq!(report["executions"].as_array().unwrap().len(), 3);
        for execution in report["executions"].as_array().unwrap().iter().skip(1) {
            assert_eq!(execution["coverage"]["kind"], "skipped");
            assert_eq!(execution["coverage"]["reason"], "build_not_succeeded");
            assert!(execution["run"].is_null());
        }
        let second = w.jobs().into_iter().find(|dir| dir != &first).unwrap();
        assert!(!second.join("r00001").exists());
        assert!(!second.join("builds/r00000/main").exists());
        assert_eq!(
            original,
            fs::read(first.join("builds/r00000/main")).unwrap()
        );
    }

    #[test]
    fn explicit_cases_preserve_order_and_include_disabled_entries() {
        let w = Workspace::new(Some("python"));
        w.cases(&[
            ("first", "1\n", Some("1\n"), true),
            ("disabled", "2\n", Some("2\n"), false),
            ("last", "3\n", Some("3\n"), true),
        ]);
        let output = w.run(&["test", "--json"]);
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["tests"][0]["id"], "first");
        assert_eq!(report["tests"][1]["id"], "last");
        assert_eq!(report["tests"].as_array().unwrap().len(), 2);
        let output = w.run(&[
            "test", "--case", "last", "--case", "disabled", "--case", "first", "--json",
        ]);
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["tests"][0]["id"], "last");
        assert_eq!(report["tests"][1]["id"], "disabled");
        assert_eq!(report["tests"][2]["id"], "first");
        assert_eq!(verdicts(&report), ["passed", "passed", "passed"]);
    }

    #[test]
    fn unknown_and_candidate_cases_refuse_before_admission_duplicates_are_usage() {
        let w = Workspace::new(Some("python"));
        w.cases(&[("one", "1\n", Some("1\n"), true)]);
        w.write("problems/A/tests/candidate.in", "1\n");
        let before = w.sequence();
        for name in ["missing", "candidate", "not/an/id"] {
            let output = w.run(&["test", "--case", name, "--json"]);
            status(&output, 8);
            assert!(stderr(&output).contains(name));
            assert_eq!(document(&output)["refusal"]["class"], "workspace");
        }
        let output = w.run(&["test", "--case", "one", "--case", "one", "--json"]);
        status(&output, 2);
        assert!(output.stdout.is_empty());
        assert!(stderr(&output).contains("duplicate --case"));
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);
    }

    #[test]
    fn zero_selected_tests_still_build_and_report_no_tests() {
        for disabled in [false, true] {
            let w = Workspace::new(Some("cpp"));
            if disabled {
                w.cases(&[("off", "5\n", Some("5\n"), false)]);
                fs::remove_file(w.path("problems/A/tests/off.in")).unwrap();
            }
            w.write("problems/A/tests/candidate.in", "5\n");
            let output = w.run(&["test", "--json"]);
            status(&output, 4);
            assert!(stderr(&output).contains("no tests: nothing verified"));
            let (dir, report) = w.report();
            assert_eq!(report["build"]["result"]["kind"], "succeeded");
            assert_eq!(report["result"]["verdict"], "no_tests");
            assert!(report["tests"].as_array().unwrap().is_empty());
            assert!(dir.join("builds/r00000/main").is_file());
        }
    }

    #[test]
    fn python_and_java_pass_and_empty_answers_are_present() {
        for language in ["python", "java"] {
            let w = Workspace::new(Some(language));
            w.cases(&[("one", "5\n", Some("5\n"), true)]);
            let output = w.run(&["test", "--json"]);
            status(&output, 0);
            assert_eq!(verdicts(&document(&output)), ["passed"]);
        }
        let w = Workspace::new(Some("python"));
        w.write("problems/A/main.py", "pass\n");
        w.cases(&[("empty", "", Some(""), true)]);
        let output = w.run(&["test", "--json"]);
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["tests"][0]["answer"], "empty");
        assert_eq!(
            report["tests"][0]["answer_sha256"],
            format!("{:x}", Sha256::digest(b""))
        );
    }

    #[test]
    fn effective_comparator_controls_verdict_and_invalid_answers_exit_five() {
        for (compare, answer, code, verdict) in [
            ("{ exact = true }", "5 ", 1, "wrong_answer"),
            ("{ lines = true }", "5  \n\n", 0, "passed"),
            ("{ float = { abs = 1e-6 } }", "nan\n", 5, "checker_failed"),
        ] {
            let w = Workspace::new(Some("python"));
            w.write("problems/A/problem.toml", &format!("schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'python'\ncompare = {compare}\n"));
            w.cases(&[("one", "5\n", Some(answer), true)]);
            let output = w.run(&["test", "--json"]);
            status(&output, code);
            assert_eq!(verdicts(&document(&output)), [verdict]);
            if code == 5 {
                assert!(stderr(&output).contains("CHECKER-FAILED  invalid answer"));
            }
        }
    }

    #[test]
    fn selected_unhealthy_disabled_case_refuses_without_starting_a_run() {
        let w = Workspace::new(Some("python"));
        w.cases(&[("off", "5\n", Some("5\n"), false)]);
        fs::remove_file(w.path("problems/A/tests/off.ans")).unwrap();
        let output = w.run(&["test", "--case", "off", "--json"]);
        status(&output, 8);
        let report = document(&output);
        assert_eq!(report["outcome"], "refused");
        assert_eq!(report["refusal"]["class"], "workspace");
        assert!(stderr(&output).contains("off.ans"));
        let jobs = w.jobs();
        assert_eq!(jobs.len(), 1);
        assert!(!jobs[0].join("r00000").exists());
        assert!(!jobs[0].join("report.json").exists());
    }

    #[test]
    fn source_and_tests_edited_after_start_are_read_from_capture_for_later_runs() {
        let w = Workspace::new(Some("python"));
        let marker = w.path("started");
        let release = w.path("release");
        w.write("problems/A/main.py", &format!("import pathlib, time\nn = input()\nif n == '1':\n pathlib.Path({marker:?}).write_text('ready')\n while not pathlib.Path({release:?}).exists(): time.sleep(0.01)\nprint(n)\n"));
        w.cases(&[
            ("one", "1\n", Some("1\n"), true),
            ("two", "2\n", Some("2\n"), true),
        ]);
        let mut child = w.command(&["test", "--json"]).spawn().unwrap();
        wait_for_marker(&w, &marker, &mut child);
        w.write(
            "problems/A/main.py",
            "raise RuntimeError('edited source')\n",
        );
        w.write("problems/A/tests/two.in", "99\n");
        w.write("problems/A/tests/two.ans", "100\n");
        w.write("release", "go");
        let output = wait(child);
        status(&output, 0);
        assert_eq!(verdicts(&document(&output)), ["passed", "passed"]);
        let (dir, _) = w.report();
        assert_eq!(fs::read(dir.join("inputs/two.in")).unwrap(), b"2\n");
    }

    #[test]
    fn sigint_during_tests_keeps_finished_verdict_and_delivers_before_reraise() {
        let w = Workspace::new(Some("python"));
        let marker = w.path("sleeping");
        w.write("problems/A/main.py", &format!("import pathlib, time\nn = input()\nif n == '2':\n pathlib.Path({marker:?}).write_text('ready')\n time.sleep(30)\nprint(n)\n"));
        w.cases(&[
            ("one", "1\n", Some("1\n"), true),
            ("two", "2\n", Some("2\n"), true),
            ("three", "3\n", Some("3\n"), true),
        ]);
        let mut child = w.command(&["test", "--json"]).spawn().unwrap();
        wait_for_marker(&w, &marker, &mut child);
        // SAFETY: cpf is still owned and unreaped; signal only that child.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
            0
        );
        let output = wait(child);
        assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
        let delivered = document(&output);
        let (_, report) = w.report();
        assert_eq!(verdicts(&report), ["passed", "cancelled", "cancelled"]);
        assert_eq!(report["result"]["verdict"], "cancelled");
        assert_eq!(report["result"]["pre_delivery_exit"]["reraise"], "sigint");
        assert_eq!(delivered["result"], report["result"]);
        assert!(stderr(&output).contains("one  OK"));
        assert!(stderr(&output).contains("two  CANCELLED"));
    }

    #[test]
    fn profile_global_problem_and_events_use_the_shared_pipeline() {
        let w = Workspace::new(Some("python"));
        w.write("cpforge.toml", "schema = 1\n[languages.python.profiles.marker.run]\nenv = { TEST_PROFILE = 'chosen' }\n");
        w.write(
            "problems/A/main.py",
            "import os\nprint(os.environ['TEST_PROFILE'])\n",
        );
        w.cases(&[("one", "", Some("chosen\n"), true)]);
        for mode in ["--events", "--events=output"] {
            let mut command =
                w.command(&["--problem", "local:A", "test", "--profile", "marker", mode]);
            command.current_dir(&w.0);
            let output = wait(command.spawn().unwrap());
            status(&output, 0);
            let lines: Vec<Value> = output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| wire::read::read_json(line).unwrap())
                .collect();
            assert_eq!(lines[0]["document"], "event_stream");
            assert_eq!(lines[0]["command"], "test");
            assert_eq!(lines[lines.len() - 2]["event"], "job_finished");
            assert_eq!(lines.last().unwrap()["event"], "report");
            assert_eq!(
                lines.iter().any(|v| v["event"] == "output_chunk"),
                mode == "--events=output"
            );
            assert_eq!(
                lines.last().unwrap()["report"]["target"]["profile"],
                "marker"
            );
        }
    }

    #[test]
    fn bad_deadlines_and_run_count_cap_refuse_before_registration() {
        let w = Workspace::new(Some("python"));
        let before = w.sequence();
        for deadline in ["0", "-1s", "nan", "25h", "86401s"] {
            let output = w.run(&["test", "--deadline", deadline, "--json"]);
            status(&output, 7);
            assert_eq!(document(&output)["refusal"]["class"], "config");
        }
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);

        // No data files are needed: the plan cap is checked before freezing.
        // The exact boundary is unit-tested; one end-to-end refusal suffices.
        let w = Workspace::new(Some("cpp"));
        let before = w.sequence();
        let mut catalog = String::from("schema = 1\n");
        for n in 0..10_000 {
            catalog.push_str(&format!(
                "[[test]]\nid='t{n}'\ninput='tests/t{n}.in'\norigin='custom'\nenabled=true\n"
            ));
        }
        w.write("problems/A/tests.toml", &catalog);
        let output = w.run(&["test", "--json"]);
        status(&output, 7); // 10 000 tests plus one build.
        assert_eq!(document(&output)["refusal"]["class"], "config");
        assert!(stderr(&output).contains("10 000"));
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);
    }
}

#[cfg(windows)]
#[test]
fn windows_refuses_test_execution_before_admission() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cpf"))
        .args(["test", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(10), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["refusal"]["class"], "infrastructure");
    assert!(report["job"].is_null());
}
