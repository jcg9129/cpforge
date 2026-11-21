use clap::Parser;
use cpforge::cli::Cli;

#[test]
fn build_flags_are_exclusive_and_deadline_is_not_a_build_option() {
    for args in [
        vec!["cpf", "build", "--json", "--events"],
        vec!["cpf", "build", "--deadline", "1s"],
        vec!["cpf", "build", "--events=status"],
    ] {
        assert_eq!(Cli::try_parse_from(args).unwrap_err().exit_code(), 2);
    }
    for mode in ["--json", "--events", "--events=output"] {
        Cli::try_parse_from([
            "cpf",
            "--problem",
            "local:A",
            "build",
            "--profile",
            "debug",
            mode,
        ])
        .unwrap();
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod execution {
    use cpforge::report::wire;
    use serde_json::Value;
    use std::fs;
    use std::io::Read;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Output, Stdio};
    use std::time::{Duration, Instant};

    struct Workspace(PathBuf);
    impl Workspace {
        fn new(language: Option<&str>) -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!(
                    "cli-build-{}",
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
                "raise RuntimeError('build must not run Python')\n",
            );
            workspace.write("problems/A/Main.java", "public class Main { public static void main(String[] args) { throw new RuntimeException(\"build must not run Java\"); } }\n");
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
                panic!("cpf build exceeded 150 seconds");
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
    fn refused(output: &Output, code: i32, class: &str) -> Value {
        status(output, code);
        let value = document(output);
        assert_eq!(value["outcome"], "refused");
        assert_eq!(value["refusal"]["class"], class);
        assert_eq!(value["persistence"]["report"]["kind"], "not_attempted");
        value
    }

    #[test]
    fn cpp_success_persists_schema_one_and_captures_support_without_tests() {
        let w = Workspace::new(Some("cpp"));
        w.write("problems/A/problem.toml", "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\nsupport_files = ['value.h']\n");
        w.write("problems/A/value.h", "#define VALUE 0\n");
        w.write(
            "problems/A/main.cpp",
            "#include \"value.h\"\nint main() { return VALUE; }\n",
        );
        // A build neither reads nor freezes the test catalog.
        w.write("problems/A/tests.toml", "deliberately malformed [\n");
        let output = w.run(&["build"]);
        status(&output, 0);
        assert!(output.stdout.is_empty(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("OK") && stderr.contains("artifacts: main"),
            "{stderr}"
        );
        let (dir, report) = w.report();
        assert_eq!(report["schema"], 1);
        assert_eq!(report["command"], "build");
        assert_eq!(report["result"]["verdict"], "succeeded");
        assert!(dir.join("builds/r00000/main").is_file());
        assert!(dir.join("inputs/src/value.h").is_file());
        assert_eq!(
            fs::read(dir.join("inputs/src/main.cpp")).unwrap(),
            fs::read(w.path("problems/A/main.cpp")).unwrap()
        );
        assert_eq!(report["tests"].as_array().unwrap().len(), 0);
        assert_eq!(report["executions"].as_array().unwrap().len(), 1);
        assert_eq!(report["capture"]["manifest"], "inputs/capture.json");
    }

    #[test]
    fn syntax_error_is_ce_and_never_exposes_a_runnable_artifact() {
        let w = Workspace::new(Some("cpp"));
        w.write(
            "problems/A/main.cpp",
            "int main() { deliberate syntax error }\n",
        );
        let output = w.run(&["build", "--json"]);
        status(&output, 3);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("CE") && stderr.contains("error:"),
            "{stderr}"
        );
        let (dir, report) = w.report();
        assert_eq!(report["result"]["verdict"], "build_failed");
        assert!(report["build"]["artifacts"].as_array().unwrap().is_empty());
        assert!(!dir.join("builds/r00000/main").exists());
        assert_eq!(report["executions"].as_array().unwrap().len(), 1);
        assert_eq!(document(&output)["result"], report["result"]);
    }

    #[test]
    fn python_build_free_succeeds_without_running_the_script() {
        let w = Workspace::new(Some("python"));
        let output = w.run(&["build", "--json"]);
        status(&output, 0);
        assert!(String::from_utf8_lossy(&output.stderr).contains("nothing was compiled"));
        let (dir, report) = w.report();
        assert_eq!(report["result"]["verdict"], "succeeded");
        assert_eq!(report["target"]["build_free"], true);
        assert!(report["build"].is_null());
        assert!(report["executions"].as_array().unwrap().is_empty());
        assert!(dir.join("inputs/src/main.py").is_file());
    }

    #[test]
    fn java_build_validates_class_output_without_running_it() {
        let w = Workspace::new(Some("java"));
        let output = w.run(&["build"]);
        status(&output, 0);
        let (dir, report) = w.report();
        assert_eq!(report["result"]["verdict"], "succeeded");
        assert!(dir.join("builds/r00000/Main.class").is_file());
    }

    #[test]
    fn json_is_exactly_the_persisted_report_plus_persistence() {
        let w = Workspace::new(Some("cpp"));
        let output = w.run(&["build", "--json"]);
        status(&output, 0);
        let mut stdout = document(&output);
        let (_, report) = w.report();
        let persistence = stdout
            .as_object_mut()
            .unwrap()
            .remove("persistence")
            .unwrap();
        assert_eq!(persistence["report"]["kind"], "published");
        assert_eq!(stdout, report);
    }

    #[test]
    fn events_have_header_ordered_sequences_and_the_two_terminal_lines() {
        for mode in ["--events", "--events=output"] {
            let w = Workspace::new(Some("cpp"));
            // Guaranteed compiler output, independent of compiler warnings.
            w.write("problems/A/problem.toml", "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\n[config.build]\nshell = { interpreter = ['sh', '-c'], script = 'printf chunk; cp \"$1\" \"$2\"; chmod +x \"$2\"', args = ['/bin/sh', '{artifact}'] }\n");
            let output = w.run(&["build", mode]);
            status(&output, 0);
            let lines: Vec<Value> = output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| wire::read::read_json(line).unwrap())
                .collect();
            assert_eq!(output.stdout.last(), Some(&b'\n'));
            assert_eq!(lines[0]["document"], "event_stream");
            assert_eq!(lines[0]["output"], mode == "--events=output");
            assert_eq!(lines[1]["event"], "job_started");
            assert_eq!(lines[lines.len() - 2]["event"], "job_finished");
            assert_eq!(lines.last().unwrap()["event"], "report");
            let sequence: Vec<u64> = lines[1..lines.len() - 1]
                .iter()
                .map(|v| v["seq"].as_str().unwrap().parse().unwrap())
                .collect();
            assert_eq!(sequence[0], 0);
            assert!(sequence.windows(2).all(|pair| pair[0] < pair[1]));
            assert_eq!(
                lines.iter().any(|v| v["event"] == "output_chunk"),
                mode == "--events=output"
            );
            let (_, report) = w.report();
            let mut delivered = lines.last().unwrap()["report"].clone();
            delivered.as_object_mut().unwrap().remove("persistence");
            assert_eq!(delivered, report);
        }
    }

    #[test]
    fn config_and_plan_errors_refuse_before_admission() {
        for (language, extra) in [
            (None, ""),
            (
                Some("cpp"),
                "[config.build]\nargv = ['cpf-compiler-that-does-not-exist', '{sources}', '-o', '{artifact}']\n",
            ),
            (
                Some("cpp"),
                "[config.build]\nargv = ['g++', '{problem_dir}', '-o', '{artifact}']\n",
            ),
            (Some("cpp"), "[config.build]\ndeadline_s = 0\n"),
        ] {
            let w = Workspace::new(language);
            if !extra.is_empty() {
                let path = w.path("problems/A/problem.toml");
                let mut text = fs::read_to_string(&path).unwrap();
                text.push_str(extra);
                fs::write(path, text).unwrap();
            }
            let before = w.sequence();
            refused(&w.run(&["build", "--json"]), 7, "config");
            assert!(w.jobs().is_empty());
            assert_eq!(w.sequence(), before);
        }
    }

    #[test]
    fn events_refusal_and_admission_failure_keep_machine_framing() {
        let w = Workspace::new(None);
        let output = w.run(&["build", "--events"]);
        status(&output, 7);
        let lines: Vec<Value> = output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| wire::read::read_json(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["document"], "event_stream");
        assert_eq!(lines[1]["event"], "report");
        assert_eq!(lines[1]["report"]["refusal"]["class"], "config");
        assert!(w.jobs().is_empty());

        let w = Workspace::new(Some("cpp"));
        w.write(".cpforge/job-seq", "schema = 1\nnext = 'invalid'\n");
        let before = w.sequence();
        refused(&w.run(&["build", "--json"]), 8, "workspace");
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);
    }

    #[test]
    fn unsupported_problem_kinds_refuse_before_admission() {
        for kind in ["answer", "interactive", "multi-file", "unknown"] {
            let w = Workspace::new(Some("cpp"));
            w.write("problems/A/problem.toml", &format!("schema = 1\n[problem]\nid = 'local:A'\nkind = '{kind}'\n[config]\nlanguage = 'cpp'\n"));
            let before = w.sequence();
            let output = w.run(&["build", "--json"]);
            refused(&output, 7, "config");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("unsupported in this version")
            );
            assert_eq!(w.sequence(), before);
            assert!(w.jobs().is_empty());
        }
    }

    #[test]
    fn profile_selection_and_global_problem_are_applied_before_admission() {
        let w = Workspace::new(Some("cpp"));
        let before = w.sequence();
        refused(
            &w.run(&["build", "--profile", "missing", "--json"]),
            7,
            "config",
        );
        assert_eq!(w.sequence(), before);
        w.write("cpforge.toml", "schema = 1\n[languages.cpp.profiles.marker.build]\nenv = { CLI_BUILD_PROFILE = 'chosen' }\n");
        let mut command = w.command(&[
            "--problem",
            "local:A",
            "build",
            "--profile",
            "marker",
            "--json",
        ]);
        command.current_dir(&w.0);
        let output = wait(command.spawn().unwrap());
        status(&output, 0);
        let (dir, report) = w.report();
        assert_eq!(report["target"]["profile"], "marker");
        let spec = fs::read_to_string(dir.join("r00000/spec.json")).unwrap();
        assert!(
            spec.contains("CLI_BUILD_PROFILE") && spec.contains("chosen"),
            "{spec}"
        );
    }

    #[test]
    fn selection_and_workspace_errors_have_status_eight_and_no_job() {
        let w = Workspace::new(Some("cpp"));
        let before = w.sequence();
        let mut command = w.command(&["build", "--json"]);
        command.current_dir(&w.0);
        let value = refused(&wait(command.spawn().unwrap()), 8, "workspace");
        assert!(
            value["refusal"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("NoSelection")
        );
        refused(
            &w.run(&["--problem", "missing", "build", "--json"]),
            8,
            "workspace",
        );
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);
        fs::remove_file(w.path("cpforge.toml")).unwrap();
        let value = refused(&w.run(&["build", "--json"]), 8, "workspace");
        assert!(
            value["refusal"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("outside a workspace")
        );
        assert!(w.jobs().is_empty());
    }

    #[test]
    fn closed_stdout_fails_before_registration_and_conflicts_are_usage_errors() {
        let w = Workspace::new(Some("cpp"));
        let before = w.sequence();
        let mut command = w.command(&["build", "--json"]);
        // SAFETY: only close the child stdout; cpf repairs it at startup.
        unsafe {
            command.pre_exec(|| {
                libc::close(1);
                Ok(())
            });
        }
        let output = wait(command.spawn().unwrap());
        status(&output, 11);
        assert!(output.stdout.is_empty());
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);
        let output = w.run(&["build", "--json", "--events"]);
        status(&output, 2);
        assert!(output.stdout.is_empty());
        assert_eq!(w.sequence(), before);
    }

    #[test]
    fn capture_failure_has_registered_identity_but_no_report() {
        let w = Workspace::new(Some("cpp"));
        fs::remove_file(w.path("problems/A/main.cpp")).unwrap();
        let output = w.run(&["build", "--json"]);
        let refusal = refused(&output, 8, "workspace");
        assert!(!refusal["job"].is_null());
        let jobs = w.jobs();
        assert_eq!(jobs.len(), 1);
        assert!(!jobs[0].join("report.json").exists());
        assert!(!jobs[0].join("r00000").exists());
    }

    #[test]
    fn consecutive_builds_are_fresh_and_a_later_failure_cannot_reuse_outputs() {
        let w = Workspace::new(Some("cpp"));
        status(&w.run(&["build"]), 0);
        let (first, _) = w.report();
        let original = fs::read(first.join("builds/r00000/main")).unwrap();
        w.write("problems/A/main.cpp", "int main() { return 42; }\n");
        status(&w.run(&["build"]), 0);
        let second = w.jobs().into_iter().find(|dir| dir != &first).unwrap();
        assert_ne!(first, second);
        assert_ne!(
            original,
            fs::read(second.join("builds/r00000/main")).unwrap()
        );
        assert_eq!(
            original,
            fs::read(first.join("builds/r00000/main")).unwrap()
        );
        let second_report =
            wire::read::read_json(&fs::read(second.join("report.json")).unwrap()).unwrap();
        let first_report =
            wire::read::read_json(&fs::read(first.join("report.json")).unwrap()).unwrap();
        assert_ne!(
            first_report["job"]["sequence"],
            second_report["job"]["sequence"]
        );
        w.write("problems/A/main.cpp", "syntax error\n");
        let output = w.run(&["build", "--json"]);
        status(&output, 3);
        assert_eq!(w.jobs().len(), 3);
        assert!(
            document(&output)["build"]["artifacts"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sigint_cancels_long_build_persists_and_delivers_before_reraise() {
        let w = Workspace::new(Some("cpp"));
        // An observed producer side effect is the barrier: no signal is sent
        // until the shell stage is running and the command's listener is active.
        let marker = w.path("producer-started");
        let script = format!("printf ready > '{}'; sleep 30", marker.display());
        w.write("problems/A/problem.toml", &format!("schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\n[config.build]\nshell = {{ interpreter = ['sh', '-c'], script = '''{script}''' }}\n"));
        let mut child = w.command(&["build", "--json"]).spawn().unwrap();
        let start = Instant::now();
        while !marker.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "build exited before marker"
            );
            if start.elapsed() > Duration::from_secs(20) {
                let _ = child.kill();
                panic!("build producer did not reach marker: {:?}", wait(child));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: child is still owned and unreaped; send SIGINT only to cpf.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
            0
        );
        let output = wait(child);
        assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
        let stdout = document(&output);
        let (_, report) = w.report();
        assert_eq!(report["result"]["verdict"], "cancelled");
        assert_eq!(report["result"]["cancellation"]["source"], "console");
        assert_eq!(report["result"]["pre_delivery_exit"]["reraise"], "sigint");
        assert_eq!(stdout["result"], report["result"]);
        assert!(String::from_utf8_lossy(&output.stderr).contains("CANCELLED"));
    }
}

#[cfg(windows)]
#[test]
fn unsupported_platform_is_infrastructure_before_admission() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cpf"))
        .arg("build")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(10), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported platform"));
    // With --json the refusal document carries the infrastructure class.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cpf"))
        .args(["build", "--json"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(10), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("{\"schema\":1,\"document\":\"job_report\","),
        "{stdout}"
    );
    assert!(stdout.contains("\"class\":\"infrastructure\""), "{stdout}");
    assert!(stdout.ends_with("}\n"), "{stdout}");
}
