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
                .env_remove("CPF_STORAGE_PANIC_AT")
                .env_remove("CPF_STORAGE_PAUSE_AT")
                .env_remove("CPF_STORAGE_PERSISTENCE_STATS")
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
        let stdout = child.stdout.take();
        let mut stderr = child.stderr.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stdout) = stdout {
                stdout.read_to_end(&mut bytes).unwrap();
            }
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

    /// Own the paused child and its stdout reader even on assertion failure.
    /// Every wait for a barrier or report has a hard timeout; stdin releases
    /// the barrier without relying on timing or filesystem polling.
    #[cfg(feature = "storage-test-hooks")]
    struct PausedJob {
        child: Option<Child>,
        lines: std::sync::mpsc::Receiver<Vec<u8>>,
        reader: Option<std::thread::JoinHandle<()>>,
    }
    #[cfg(feature = "storage-test-hooks")]
    impl PausedJob {
        fn start(w: &Workspace, point: &str) -> Self {
            Self::start_args(w, point, &["test", "--json"])
        }
        fn start_args(w: &Workspace, point: &str, args: &[&str]) -> Self {
            use std::io::{BufRead, BufReader};
            let mut child = w
                .command(args)
                .env("CPF_STORAGE_PAUSE_AT", point)
                .env("CPF_STORAGE_STDERR_QUEUED", w.path("stderr-queued"))
                .env(
                    "CPF_STORAGE_PERSISTENCE_STATS",
                    w.path("persistence-stats.json"),
                )
                .env("CPF_STORAGE_SIGNAL_READY", w.path("signal-ready"))
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap());
            let (send, lines) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                loop {
                    let mut line = Vec::new();
                    if stdout.read_until(b'\n', &mut line).unwrap() == 0 {
                        break;
                    }
                    if send.send(line).is_err() {
                        break;
                    }
                }
            });
            let paused = Self {
                child: Some(child),
                lines,
                reader: Some(reader),
            };
            assert_eq!(paused.line(), format!("CPF-PAUSED {point}\n").as_bytes());
            paused
        }
        fn line(&self) -> Vec<u8> {
            self.lines
                .recv_timeout(Duration::from_secs(300))
                .expect("cpf did not reach the barrier or deliver its report")
        }
        fn queued_at(&mut self, w: &Workspace) -> std::time::SystemTime {
            wait_for_marker(w, &w.path("stderr-queued"), self.child.as_mut().unwrap());
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(nanos) = fs::read_to_string(w.path("stderr-queued"))
                    .unwrap()
                    .parse::<u64>()
                {
                    return std::time::UNIX_EPOCH + Duration::from_nanos(nanos);
                }
                assert!(Instant::now() < deadline, "incomplete t_q milestone");
                std::thread::yield_now();
            }
        }
        fn release(&mut self) {
            use std::io::Write;
            self.child
                .as_mut()
                .unwrap()
                .stdin
                .take()
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }
        fn finish(mut self, delivered: Vec<u8>) -> Output {
            let mut output = wait(self.child.take().unwrap());
            self.reader.take().unwrap().join().unwrap();
            output.stdout = delivered;
            for line in self.lines.try_iter() {
                output.stdout.extend(line);
            }
            output
        }
    }
    #[cfg(feature = "storage-test-hooks")]
    impl Drop for PausedJob {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    // Reconstruct E §2's independent binary digest from the published spec,
    // including native bytes, rather than hashing its JSON representation.
    #[cfg(feature = "storage-test-hooks")]
    fn published_spec_digest(spec: &Value) -> String {
        fn bytes(hash: &mut Sha256, value: &[u8]) {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
        fn native(hash: &mut Sha256, value: &Value) {
            let value = wire::decode_native(&serde_json::to_vec(value).unwrap()).unwrap();
            bytes(hash, &wire::native_bytes(&value));
        }
        let mut hash = Sha256::new();
        bytes(&mut hash, spec["stage"].as_str().unwrap().as_bytes());
        native(&mut hash, &spec["program"]);
        let args = spec["args"].as_array().unwrap();
        hash.update((args.len() as u64).to_le_bytes());
        for arg in args {
            native(&mut hash, arg);
        }
        native(&mut hash, &spec["cwd"]);
        let environment = spec["env"].as_array().unwrap();
        hash.update((environment.len() as u64).to_le_bytes());
        for pair in environment {
            native(&mut hash, &pair["name"]);
            native(&mut hash, &pair["value"]);
        }
        let stdin = &spec["stdin"];
        let kind = stdin["kind"].as_str().unwrap();
        bytes(&mut hash, kind.as_bytes());
        if kind == "file" {
            native(&mut hash, &stdin["path"]);
        }
        for name in ["stdout", "stderr"] {
            let stream = &spec[name];
            let sink = &stream["sink"];
            let kind = sink["kind"].as_str().unwrap();
            bytes(&mut hash, kind.as_bytes());
            match kind {
                "file" => native(&mut hash, &sink["path"]),
                "forward" => bytes(&mut hash, sink["stream"].as_str().unwrap().as_bytes()),
                "discard" => {}
                other => panic!("invalid persisted sink {other}"),
            }
            hash.update(
                wire::read::read_u64(&stream["budget"])
                    .unwrap()
                    .to_le_bytes(),
            );
            bytes(&mut hash, stream["on_exceed"].as_str().unwrap().as_bytes());
            hash.update([u8::from(stream["events"].as_bool().unwrap())]);
        }
        hash.update(
            wire::read::read_u64(&spec["wall_deadline_us"])
                .unwrap()
                .to_le_bytes(),
        );
        assert!(spec["deadline_group"].is_null());
        bytes(&mut hash, b"none");
        let limits = spec["limits"].as_array().unwrap();
        hash.update((limits.len() as u64).to_le_bytes());
        for limit in limits {
            bytes(&mut hash, limit["kind"].as_str().unwrap().as_bytes());
            hash.update(wire::read::read_u64(&limit["bytes"]).unwrap().to_le_bytes());
        }
        let measures = spec["measure"].as_array().unwrap();
        hash.update((measures.len() as u64).to_le_bytes());
        for metric in measures {
            bytes(&mut hash, metric.as_str().unwrap().as_bytes());
        }
        format!("{:x}", hash.finalize())
    }

    #[cfg(feature = "storage-test-hooks")]
    fn assert_all_published(w: &Workspace, report: &Value, count: usize) {
        let (directory, saved) = w.report();
        let mut delivered = report.clone();
        delivered.as_object_mut().unwrap().remove("persistence");
        assert_eq!(saved, delivered);
        assert_eq!(report["persistence"]["report"]["kind"], "published");
        assert!(
            report["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|w| w["kind"] != "persist")
        );
        assert_eq!(report["executions"].as_array().unwrap().len(), count);
        for execution in report["executions"].as_array().unwrap() {
            let spec_ref = &execution["spec"];
            let record_ref = &execution["record"];
            assert!(spec_ref["error"].is_null() && record_ref["error"].is_null());
            let spec_path = directory.join(spec_ref["path"].as_str().unwrap());
            let record_path = directory.join(record_ref["path"].as_str().unwrap());
            let spec = wire::read::read_json(&fs::read(spec_path).unwrap()).unwrap();
            assert_eq!(
                published_spec_digest(&spec),
                spec_ref["digest"].as_str().unwrap()
            );
            let record = wire::read::read_json(&fs::read(&record_path).unwrap()).unwrap();
            assert_eq!(record["spec"]["digest"], spec_ref["digest"]);
            assert_eq!(record["spec"]["path"], spec_ref["path"]);
            assert!(
                fs::read(record_path.with_file_name("cleanup.log"))
                    .unwrap()
                    .starts_with(b"record finalized")
            );
        }
        for name in ["spec.json", "record.json", "cleanup.log"] {
            let actual = fs::read_dir(&directory)
                .unwrap()
                .map(Result::unwrap)
                .filter(|e| {
                    e.file_type().unwrap().is_dir()
                        && e.file_name().to_string_lossy().starts_with('r')
                })
                .filter(|e| e.path().join(name).is_file())
                .count();
            assert_eq!(actual, count, "{name}");
        }
    }

    // Real-host outcome of E §6.2 without the platform evidence claim: the
    // report is published, every published spec and record is readable and
    // linked, and every refusal is credit backpressure (`queue_full`), never
    // a timeout, an unavailable worker or an I/O error.
    #[cfg(feature = "storage-test-hooks")]
    fn assert_published_or_queue_full(w: &Workspace, report: &Value, count: usize) {
        let (directory, saved) = w.report();
        let mut delivered = report.clone();
        delivered.as_object_mut().unwrap().remove("persistence");
        assert_eq!(saved, delivered);
        assert_eq!(report["persistence"]["report"]["kind"], "published");
        for warning in report["warnings"].as_array().unwrap() {
            if warning["kind"] == "persist" {
                assert_eq!(warning["error"]["kind"], "queue_full", "{warning}");
            }
        }
        let executions = report["executions"].as_array().unwrap();
        assert_eq!(executions.len(), count);
        for execution in executions {
            let spec_ref = &execution["spec"];
            let record_ref = &execution["record"];
            for reference in [spec_ref, record_ref] {
                assert!(
                    reference["path"].is_string() || reference["error"]["kind"] == "queue_full",
                    "{reference}"
                );
            }
            if let Some(path) = spec_ref["path"].as_str() {
                let spec = wire::read::read_json(&fs::read(directory.join(path)).unwrap()).unwrap();
                assert_eq!(
                    published_spec_digest(&spec),
                    spec_ref["digest"].as_str().unwrap()
                );
            }
            if let Some(path) = record_ref["path"].as_str() {
                let record_path = directory.join(path);
                let record = wire::read::read_json(&fs::read(&record_path).unwrap()).unwrap();
                assert_eq!(record["spec"]["digest"], spec_ref["digest"]);
                assert_eq!(record["spec"]["path"], spec_ref["path"]);
                if let Ok(log) = fs::read(record_path.with_file_name("cleanup.log")) {
                    assert!(log.starts_with(b"record finalized"));
                }
            }
        }
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn persistence_throughput_three_thousand_tests_platform_evidence() {
        let w = Workspace::new(Some("fixture"));
        let program = serde_json::to_string(&cpf_fixture::harness::fixture_path()).unwrap();
        w.write("cpforge.toml", &format!("schema = 1\n[languages.fixture]\nsources = ['main.sh']\nbuild = false\n[languages.fixture.run]\nargv = [{program}, 'exit:0']\n"));
        w.write("problems/A/main.sh", "fixture\n");
        let ids: Vec<_> = (0..3000).map(|n| format!("case{n:04}")).collect();
        let entries: Vec<_> = ids
            .iter()
            .map(|id| (id.as_str(), "", Some(""), true))
            .collect();
        w.cases(&entries);
        let output = wait(
            w.command(&["test", "--json"])
                .env(
                    "CPF_STORAGE_PERSISTENCE_STATS",
                    w.path("persistence-stats.json"),
                )
                .spawn()
                .unwrap(),
        );
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["result"]["verdict"], "passed");
        let stats: Value =
            serde_json::from_slice(&fs::read(w.path("persistence-stats.json")).unwrap()).unwrap();
        let waited = wire::read::read_u64(&stats["credit_wait_us"]).unwrap();
        let mut cycles: Vec<_> = stats["service_cycles_us"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| wire::read::read_u64(v).unwrap())
            .collect();
        cycles.sort_unstable();
        let max = cycles.last().copied().unwrap_or(0);
        let p99 = if cycles.is_empty() {
            0
        } else {
            cycles[(cycles.len() - 1) * 99 / 100]
        };
        let evidence = format!(
            "persistence platform evidence: max cycle {max} us, p99 {p99} us, total credit wait {waited} us ({} samples)",
            cycles.len()
        );
        eprintln!("{evidence}");
        // Report contract §9.2: a failure here blocks CI until it is
        // classified as a defect or as a recorded platform finding; the
        // evidence above is what that classification needs.
        let refused: Vec<_> = report["executions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| [&e["spec"]["error"]["kind"], &e["record"]["error"]["kind"]])
            .filter(|kind| !kind.is_null())
            .collect();
        assert!(
            refused.is_empty(),
            "{} spec/record refusals, first {:?}; {evidence}; classify as a defect or a platform finding",
            refused.len(),
            refused.first()
        );
        assert_all_published(&w, &report, 3000);
        assert_eq!(stats["last_write"], "report.json", "{evidence}");
        assert!(stats["peak_credits"].as_u64().unwrap() <= 2 * 1024 * 1024);
        assert!(waited <= 46_000_000, "{evidence}");
        let elapsed = wire::read::read_u64(&stats["settlement_to_finished_us"]).unwrap();
        assert!(
            elapsed <= 10_000_000 + 300_000 * stderr_allowance_scale(),
            "start of W to job_finished {elapsed} us; {evidence}"
        );
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn permanently_stalled_persistence_honestly_settles_one_hundred_real_tests() {
        let w = Workspace::new(Some("fixture"));
        let program = serde_json::to_string(&cpf_fixture::harness::fixture_path()).unwrap();
        w.write("cpforge.toml", &format!("schema = 1\n[languages.fixture]\nsources = ['main.sh']\nbuild = false\n[languages.fixture.run]\nargv = [{program}, 'exit:0']\n"));
        w.write("problems/A/main.sh", "fixture\n");
        let ids: Vec<_> = (0..100).map(|n| format!("case{n:03}")).collect();
        let entries: Vec<_> = ids
            .iter()
            .map(|id| (id.as_str(), "", Some(""), true))
            .collect();
        w.cases(&entries);
        let paused = PausedJob::start(&w, "persist.write");
        let delivered = paused.line();
        let report: Value = serde_json::from_slice(&delivered).unwrap();
        assert_eq!(report["result"]["verdict"], "passed");
        let executions = report["executions"].as_array().unwrap();
        assert_eq!(executions.len(), 100);
        let mut timeouts = 0;
        for execution in executions {
            for name in ["spec", "record"] {
                let reference = &execution[name];
                assert!(reference["path"].is_null());
                let kind = reference["error"]["kind"].as_str().unwrap();
                assert!(
                    matches!(kind, "timeout" | "queue_full" | "worker_unavailable"),
                    "{reference}"
                );
                timeouts += usize::from(kind == "timeout");
            }
        }
        assert!(timeouts > 0);
        assert_eq!(
            report["persistence"]["report"]["error"]["kind"],
            "worker_unavailable"
        );
        assert!(!w.jobs()[0].join("report.json").exists());
        let stats: Value =
            serde_json::from_slice(&fs::read(w.path("persistence-stats.json")).unwrap()).unwrap();
        let waited = wire::read::read_u64(&stats["credit_wait_us"]).unwrap();
        assert!(waited <= 1_000_000 + 100_000 * stderr_allowance_scale());
        let elapsed = wire::read::read_u64(&stats["settlement_to_finished_us"]).unwrap();
        assert!(elapsed <= 5_000_000 + 300_000 * stderr_allowance_scale());
        // Never release persist.write. Process exit must remain bounded while
        // cleanup owns the syscall, its lease and outstanding reservations.
        status(&paused.finish(delivered), 0);
    }

    #[test]
    fn unresolved_language_checks_successfully_but_test_refuses_before_admission() {
        let w = Workspace::new(None);
        let before = w.sequence();
        status(&w.run(&["config", "check"]), 0);
        let shown = w.run(&["config", "show", "--json"]);
        status(&shown, 0);
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert!(shown["config"].get("target").is_none());
        let output = w.run(&["test", "--json"]);
        status(&output, 7);
        let report = document(&output);
        assert_eq!(report["outcome"], "refused");
        assert_eq!(report["refusal"]["class"], "config");
        assert_eq!(report["persistence"]["report"]["kind"], "not_attempted");
        assert!(report["job"].is_null());
        assert!(stderr(&output).contains("language"));
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);
    }

    #[test]
    fn phase_one_config_refusal_never_attempts_report_persistence() {
        let w = Workspace::new(Some("cpp"));
        w.write("cpforge.toml", "schema = 1\n[run]\nargv = ['']\n");
        let before = w.sequence();
        let output = w.run(&["test", "--json"]);
        status(&output, 7);
        let report = document(&output);
        assert_eq!(report["outcome"], "refused");
        assert_eq!(report["refusal"]["class"], "config");
        assert_eq!(report["persistence"]["report"]["kind"], "not_attempted");
        assert!(report["job"].is_null());
        assert!(stderr(&output).contains("run.argv[0]"));
        assert!(stderr(&output).contains("must not be empty"));
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);
    }

    #[test]
    fn literal_workspace_include_operand_refuses_before_launch() {
        let w = Workspace::new(Some("cpp"));
        // The script could leave a marker if it ran. Its positional include
        // operand must instead be rejected during planning.
        let script = format!("printf launched > '{}'", w.path("launched").display());
        let include = format!("-I{}", w.path("lib").display());
        let argv: toml_edit::Array = [
            "sh",
            "-c",
            script.as_str(),
            "build",
            "{sources}",
            include.as_str(),
        ]
        .into_iter()
        .collect();
        w.write(
            "cpforge.toml",
            &format!("schema = 1\n[languages.cpp.build]\nargv = {argv}\n"),
        );
        let before = w.sequence();
        let output = w.run(&["test", "--json"]);
        status(&output, 7);
        let report = document(&output);
        assert_eq!(report["refusal"]["class"], "config");
        assert!(stderr(&output).contains("phase 3"));
        assert!(stderr(&output).contains(&include));
        assert!(!w.path("launched").exists());
        assert!(w.jobs().is_empty());
        assert_eq!(w.sequence(), before);
    }

    #[test]
    fn shell_build_with_captured_sources_records_unchecked_shell_stage() {
        let w = Workspace::new(Some("cpp"));
        w.write("problems/A/main.cpp", "#!/bin/sh\ncat\n");
        w.write("cpforge.toml", "schema = 1\n[languages.cpp.build]\ncwd = { selector = 'run' }\nshell = { interpreter = ['sh', '-c'], script = 'cp \"$1\" \"$2\"; chmod +x \"$2\"', args = ['{sources}', '{artifact}'] }\n");
        w.cases(&[("one", "7\n", Some("7\n"), true)]);
        let output = w.run(&["test", "--json"]);
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["result"]["verdict"], "passed");
        assert_eq!(report["build"]["result"]["kind"], "succeeded");
        assert_eq!(report["capture"]["check"]["kind"], "unchecked");
        assert_eq!(
            report["capture"]["check"]["reasons"],
            serde_json::json!(["shell_stage"])
        );
        let (job, persisted) = w.report();
        assert_eq!(persisted["capture"]["check"], report["capture"]["check"]);
        let manifest: Value =
            serde_json::from_slice(&fs::read(job.join("inputs").join("capture.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["check"], report["capture"]["check"]);
        assert_eq!(
            fs::read(job.join("inputs").join("src").join("main.cpp")).unwrap(),
            b"#!/bin/sh\ncat\n"
        );
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn source_collision_after_registration_refuses_without_report_or_launch() {
        let w = Workspace::new(Some("cpp"));
        w.write(
            "cpforge.toml",
            "schema = 1\n[languages.cpp.build]\nargv = ['sh', '-c', 'exit 0']\n",
        );
        w.write("problems/A/problem.toml", "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\nsources = ['main.cpp', 'second.cpp']\n");
        w.write("problems/A/second.cpp", "second source\n");
        let mut paused = PausedJob::start(&w, "inputs.source_copied");
        let jobs = w.jobs();
        assert_eq!(jobs.len(), 1);
        let collision = jobs[0].join("inputs").join("src").join("second.cpp");
        fs::write(&collision, b"keep existing destination").unwrap();
        paused.release();
        let output = paused.finish(Vec::new());
        status(&output, 8);
        let report = document(&output);
        assert_eq!(report["outcome"], "refused");
        assert!(!report["job"].is_null());
        assert_eq!(report["refusal"]["class"], "workspace");
        assert_eq!(report["persistence"]["report"]["kind"], "not_attempted");
        assert!(stderr(&output).contains("source destination already exists"));
        assert_eq!(fs::read(collision).unwrap(), b"keep existing destination");
        assert!(!jobs[0].join("report.json").exists());
        assert!(!jobs[0].join("r00000").exists());
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn manifest_publication_failure_is_a_workspace_refusal() {
        let w = Workspace::new(Some("python"));
        let mut paused = PausedJob::start(&w, "manifest.publish");
        let jobs = w.jobs();
        assert_eq!(jobs.len(), 1);
        let destination = jobs[0].join("inputs").join("capture.json");
        // A competing destination deterministically fails no-clobber publish
        // on every supported filesystem, including when tests run as root.
        fs::create_dir(&destination).unwrap();
        paused.release();
        let output = paused.finish(Vec::new());
        status(&output, 8);
        let report = document(&output);
        assert_eq!(report["outcome"], "refused");
        assert_eq!(report["refusal"]["class"], "workspace");
        assert!(!report["job"].is_null());
        assert_eq!(report["persistence"]["report"]["kind"], "not_attempted");
        assert!(stderr(&output).contains("capture.json"));
        assert!(destination.is_dir());
        assert!(!jobs[0].join("report.json").exists());
        assert!(!jobs[0].join("r00000").exists());
    }

    #[cfg(feature = "storage-test-hooks")]
    fn wait_until(at: std::time::SystemTime) {
        while let Ok(remaining) = at.duration_since(std::time::SystemTime::now()) {
            std::thread::park_timeout(remaining);
        }
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn three_thousand_tests_finish_while_stderr_is_blocked_then_deliver_serial_summary() {
        let w = Workspace::new(Some("fixture"));
        w.write("cpforge.toml", "schema = 1\n[languages.fixture]\nsources = ['main.sh']\nbuild = false\n[languages.fixture.run]\nargv = ['sh', '{source}']\n");
        w.write("problems/A/main.sh", "exit 0\n");
        let ids: Vec<_> = (0..3000)
            .map(|n| format!("case{n:04}{}", "x".repeat(40)))
            .collect();
        let entries: Vec<_> = ids
            .iter()
            .map(|id| (id.as_str(), "", Some(""), true))
            .collect();
        w.cases(&entries);
        let mut paused = PausedJob::start(&w, "stderr.write");
        // Phase D must deliver a complete report while the stderr writer is
        // still blocked. No stderr bytes or final summary have been written.
        let delivered = paused.line();
        let report: Value = serde_json::from_slice(&delivered).unwrap();
        assert_eq!(report["outcome"], "finished");
        assert_eq!(report["result"]["verdict"], "passed");
        assert_eq!(report["tests"].as_array().unwrap().len(), 3000);
        assert_eq!(report["executions"].as_array().unwrap().len(), 3000);
        assert!(
            report["executions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["coverage"]["kind"] == "started")
        );
        assert!(
            report["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|warning| warning["kind"] != "progress_dropped")
        );
        assert!(paused.child.as_mut().unwrap().try_wait().unwrap().is_none());
        let (_, persisted) = w.report();
        assert_eq!(persisted["result"], report["result"]);
        use std::os::fd::AsRawFd;
        let mut stderr_ready = libc::pollfd {
            fd: paused
                .child
                .as_ref()
                .unwrap()
                .stderr
                .as_ref()
                .unwrap()
                .as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll only inspects the live child's owned pipe descriptor.
        assert_eq!(
            unsafe { libc::poll(&mut stderr_ready, 1, 0) },
            0,
            "stderr produced bytes before releasing its write barrier"
        );
        let queued_at = paused.queued_at(&w);
        // This is the specified service schedule, after the enqueue milestone,
        // rather than a delay used to guess whether enqueue has happened.
        wait_until(queued_at + Duration::from_secs(1));
        paused.release();
        let output = paused.finish(delivered);
        status(&output, 0);
        assert_eq!(document(&output), report);
        let stderr = stderr(&output);
        let lines: Vec<_> = stderr.lines().collect();
        assert_eq!(
            lines.last().copied(),
            Some("3000/3000 passed (tokens comparison)")
        );
        let mut progress = 0;
        let mut warnings = false;
        for line in &lines[..lines.len() - 1] {
            assert!(line.len() < cpforge::progress::LINE_BYTES);
            if line.starts_with("cpf: warning:") {
                warnings = true;
                // Real hosts give no service bound (E §6.2): backpressure can
                // end in `queue_full`, which the platform evidence run, not
                // this stderr fixture, classifies. A loaded host can also cut
                // a cleanup scan at its deadline (E §4.4), which is a warning.
                assert!(
                    line.contains("progress lines dropped")
                        || line.contains("artefact publication failed")
                        || line.contains("cleanup not verified")
                        || line.contains("further warnings dropped"),
                    "{line}"
                );
            } else {
                assert!(
                    !warnings,
                    "progress interleaved with final warnings: {line}"
                );
                let (id, label) = line.split_once("  ").unwrap();
                assert_eq!(id, ids[progress], "result order");
                assert!(label.starts_with("OK"), "{line}");
                assert!(line.len() >= 40);
                progress += 1;
            }
        }
        assert_eq!(progress, 3000);
        assert_published_or_queue_full(&w, &report, 3000);
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn stalled_stderr_exits_five_seconds_after_final_enqueue_with_verdict_status() {
        let w = Workspace::new(Some("fixture"));
        w.write("cpforge.toml", "schema = 1\n[languages.fixture]\nsources = ['main.sh']\nbuild = false\n[languages.fixture.run]\nargv = ['sh', '{source}']\n");
        w.write("problems/A/main.sh", "exit 0\n");
        w.cases(&[("one", "", Some(""), true)]);
        let mut paused = PausedJob::start(&w, "stderr.write");
        let delivered = paused.line();
        let report: Value = serde_json::from_slice(&delivered).unwrap();
        assert_eq!(report["result"]["verdict"], "passed");
        let tq = paused.queued_at(&w);
        // The write barrier is never released. Child process exit must abandon
        // it, with stdout and report.json already complete.
        while paused.child.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(
                std::time::SystemTime::now().duration_since(tq).unwrap()
                    < Duration::from_secs(5)
                        + Duration::from_millis(100 * stderr_allowance_scale()),
                "stalled final drain"
            );
            std::thread::yield_now();
        }
        let elapsed = std::time::SystemTime::now().duration_since(tq).unwrap();
        assert!(elapsed >= Duration::from_secs(5));
        let output = paused.finish(delivered);
        status(&output, 0);
        assert!(output.stderr.is_empty());
        assert_eq!(document(&output), report);
        assert_eq!(w.report().1["result"], report["result"]);
    }

    #[cfg(feature = "storage-test-hooks")]
    fn stderr_allowance_scale() -> u64 {
        if cfg!(target_os = "macos") {
            4
        } else if std::env::var_os("GITHUB_ACTIONS").is_some() {
            2
        } else {
            1
        }
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn force_exit_never_waits_for_stderr_lock_or_final_drain() {
        for point in ["stderr.lock", "stderr.write"] {
            let w = Workspace::new(Some("fixture"));
            w.write("cpforge.toml", "schema = 1\n[languages.fixture]\nsources = ['main.sh']\nbuild = false\n[languages.fixture.run]\nargv = ['sh', '{source}']\n");
            w.write("problems/A/main.sh", "exit 0\n");
            w.cases(&[("one", "", Some(""), true)]);
            let mut paused = PausedJob::start(&w, point);
            let delivered = if point == "stderr.write" {
                let delivered = paused.line();
                let tq = paused.queued_at(&w);
                wait_until(tq + Duration::from_secs(1));
                delivered
            } else {
                Vec::new()
            };
            let pid = paused.child.as_ref().unwrap().id() as i32;
            // SAFETY: the subprocess is owned and remains alive at the barrier.
            assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
            wait_for_marker(&w, &w.path("signal-ready"), paused.child.as_mut().unwrap());
            // Deliberately satisfy the signal debounce, after acknowledgement.
            wait_until(std::time::SystemTime::now() + Duration::from_millis(110));
            let forced_at = Instant::now();
            assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
            while paused.child.as_mut().unwrap().try_wait().unwrap().is_none() {
                assert!(
                    forced_at.elapsed() < Duration::from_millis(100 * stderr_allowance_scale()),
                    "force exit waited for stderr"
                );
                std::thread::yield_now();
            }
            let output = paused.finish(delivered);
            assert_eq!(output.status.signal(), Some(libc::SIGINT));
            assert!(output.stderr.is_empty());
        }
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn orderly_cancellation_drains_results_before_reraising_with_blocked_stderr() {
        let w = Workspace::new(Some("fixture"));
        w.write("cpforge.toml", "schema = 1\n[languages.fixture]\nsources = ['main.sh']\nbuild = false\n[languages.fixture.run]\nargv = ['sh', '{source}']\n");
        let ready = w.path("second-ready");
        w.write("problems/A/main.sh", &format!("read action\nif [ \"$action\" = wait ]; then\n  printf ready > {ready:?}\n  exec sleep 30\nfi\nexit 0\n"));
        w.cases(&[
            ("one", "pass\n", Some(""), true),
            ("two", "wait\n", Some(""), true),
        ]);
        let mut paused = PausedJob::start(&w, "stderr.write");
        wait_for_marker(&w, &ready, paused.child.as_mut().unwrap());
        let pid = paused.child.as_ref().unwrap().id() as i32;
        // SAFETY: the owned process is alive, with its second run at the gate.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
        let delivered = paused.line();
        let report: Value = serde_json::from_slice(&delivered).unwrap();
        assert_eq!(report["result"]["verdict"], "cancelled");
        let tq = paused.queued_at(&w);
        wait_until(tq + Duration::from_secs(1));
        paused.release();
        let output = paused.finish(delivered);
        assert_eq!(output.status.signal(), Some(libc::SIGINT));
        let text = stderr(&output);
        let lines: Vec<_> = text.lines().collect();
        assert!(lines[0].starts_with("one  OK"));
        assert!(lines[1].starts_with("two  CANCELLED"));
        assert!(
            lines[2..lines.len() - 1]
                .iter()
                .any(|line| line.contains("CANCELLED"))
        );
        assert_eq!(lines.last(), Some(&"1/2 passed (tokens comparison)"));
        assert_eq!(document(&output), report);
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn ten_thousand_command_executions_keep_ten_test_lines_with_blocked_stderr() {
        let w = Workspace::new(None);
        let fixture = cpf_fixture::harness::fixture_path();
        let argv = serde_json::to_string(&[fixture.to_str().unwrap(), "exit:0"]).unwrap();
        w.write(
            "cpforge.toml",
            &format!(
                "schema = 1\n[commands.flood]\nargv = {argv}\nfor_each_test = true\nrepeat = 1000\n"
            ),
        );
        let ids: Vec<_> = (0..10).map(|n| format!("case{n:02}")).collect();
        let entries: Vec<_> = ids.iter().map(|id| (id.as_str(), "", None, true)).collect();
        w.cases(&entries);
        let mut paused = PausedJob::start_args(&w, "stderr.write", &["cmd", "flood", "--json"]);
        let delivered = paused.line();
        let report: Value = serde_json::from_slice(&delivered).unwrap();
        assert_eq!(report["result"]["verdict"], "succeeded");
        assert_eq!(report["executions"].as_array().unwrap().len(), 10_000);
        assert_eq!(report["tests"].as_array().unwrap().len(), 10);
        let warnings = report["warnings"].as_array().unwrap();
        // The writer took exactly one progress line before blocking. All
        // result lines survive, and E finalizes the other progress as dropped.
        // Persistence warnings precede it in the list; on a host slower than
        // the E §6.2 service assumption, `queue_full` refusals can push it
        // among the counted, unlisted warnings.
        if let Some(warning) = warnings
            .iter()
            .find(|warning| warning["kind"] == "progress_dropped")
        {
            assert_eq!(warning["lines"], 9999);
        } else {
            assert_eq!(warnings.len(), 24);
            assert!(report["warnings_dropped"].as_u64().unwrap() > 0);
        }
        let tq = paused.queued_at(&w);
        wait_until(tq + Duration::from_secs(1));
        paused.release();
        let output = paused.finish(delivered);
        status(&output, 0);
        let text = stderr(&output);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0], "1/10000 executions finished");
        for (n, line) in lines[1..11].iter().enumerate() {
            assert!(
                line.starts_with(&format!("{}  OK  1000/1000 succeeded", ids[n])),
                "{line}"
            );
        }
        assert!(
            lines[11..lines.len() - 1]
                .iter()
                .all(|line| line.starts_with("cpf: warning:"))
        );
        assert_eq!(lines.last(), Some(&"OK"));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("executions finished"))
                .count(),
            1
        );
        assert_eq!(w.report().1["warnings"], report["warnings"]);
        assert_published_or_queue_full(&w, &report, 10_000);
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
    fn one_output_budget_failure_among_two_runs_exits_one() {
        let w = Workspace::new(Some("python"));
        w.write(
            "cpforge.toml",
            "schema = 1\n[build]\ndeadline_s = 90\n[run]\nstdout_cap = 4\n",
        );
        w.cases(&[
            ("short", "1\n", Some("1\n"), true),
            ("long", "123456789\n", Some("123456789\n"), true),
        ]);
        let output = w.run(&["test", "--json"]);
        status(&output, 1);
        let report = document(&output);
        assert_eq!(verdicts(&report), ["passed", "output_budget"]);
        assert_eq!(report["result"]["verdict"], "output_budget");
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
            let report = document(&output);
            assert_eq!(report["fact"]["outcome"]["kind"], "published");
            assert!(report["fingerprints"]["source"].as_str().is_some());
            assert!(report["fingerprints"]["tests"].as_str().is_some());
            let fact = cpforge::storage::facts::read_fact(w.path(".cpforge/facts/A/solution.toml"));
            assert!(matches!(fact, cpforge::storage::Record::Valid(record)
                if record.verdict == cpforge::report::JobVerdict::Passed));
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
        assert_eq!(report["fact"]["outcome"]["kind"], "not_attempted");
        assert_eq!(report["fact"]["outcome"]["reason"], "not_completed");
        assert!(!w.path(".cpforge/facts/A/solution.toml").exists());
        assert!(report["fingerprints"]["source"].as_str().is_some());
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

    #[test]
    fn other_job_kinds_leave_fact_slots_untouched() {
        // Raw `run` refuses --json, so every kind is read from its report.json.
        for args in [vec!["build"], vec!["run"], vec!["cmd", "noop"]] {
            let w = Workspace::new(Some("python"));
            w.write("problems/A/main.py", "pass\n");
            w.write(
                "cpforge.toml",
                "schema = 1\n[commands.noop]\nargv = ['python3', '-c', 'pass']\n",
            );
            let output = w.run(&args);
            status(&output, 0);
            let (_, report) = w.report();
            assert!(report["fact"].is_null());
            assert!(report["fingerprints"].is_null());
            assert!(!w.path(".cpforge/facts/A/solution.toml").exists());
        }
    }

    #[test]
    fn unavailable_sequence_at_completion_warns_without_changing_passed_verdict() {
        let w = Workspace::new(Some("python"));
        let marker = w.path("started");
        let release = w.path("release");
        w.write("problems/A/main.py", &format!("import pathlib, time\npathlib.Path({marker:?}).write_text('ready')\nwhile not pathlib.Path({release:?}).exists(): time.sleep(0.01)\nprint(input())\n"));
        w.cases(&[("one", "5\n", Some("5\n"), true)]);
        let mut child = w.command(&["test", "--json"]).spawn().unwrap();
        wait_for_marker(&w, &marker, &mut child);
        w.write(".cpforge/job-seq", "schema = 99\n");
        w.write("release", "go");
        let output = wait(child);
        status(&output, 0);
        let report = document(&output);
        assert_eq!(report["result"]["verdict"], "passed");
        assert_eq!(report["fact"]["outcome"]["kind"], "not_published");
        assert_eq!(report["fact"]["outcome"]["reason"], "sequence_unavailable");
        assert!(
            report["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["kind"] == "fact")
        );
        assert!(!w.path(".cpforge/facts/A/solution.toml").exists());
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn signal_on_either_side_of_fact_cutoff_preserves_frozen_primary_and_commit_rule() {
        use std::io::{BufRead, BufReader, Write};
        for (barrier, expected) in [
            ("fact.cutoff", "not_attempted"),
            ("fact.after_cutoff", "published"),
            ("fact.after_replace", "published"),
        ] {
            let w = Workspace::new(Some("python"));
            w.cases(&[("one", "5\n", Some("5\n"), true)]);
            status(&w.run(&["test", "--json"]), 0);
            let path = w.path(".cpforge/facts/A/solution.toml");
            let old = fs::read(&path).unwrap();
            let mut child = w
                .command(&["test", "--json"])
                .env("CPF_STORAGE_PAUSE_AT", barrier)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap());
            let mut marker = String::new();
            stdout.read_line(&mut marker).unwrap();
            assert_eq!(marker.trim(), format!("CPF-PAUSED {barrier}"));
            // SAFETY: signal the live child process owned by this test. The
            // verdict lock incorporates the handler notification itself.
            assert_eq!(
                unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
                0
            );
            child.stdin.take().unwrap().write_all(b"x").unwrap();
            assert!(stdout.buffer().is_empty());
            child.stdout = Some(stdout.into_inner());
            let output = wait(child);
            assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
            let report = document(&output);
            assert_eq!(report["result"]["primary_verdict"], "passed");
            assert_eq!(report["result"]["cancellation"]["phase"], "fact");
            assert_eq!(report["fact"]["outcome"]["kind"], expected);
            if expected == "not_attempted" {
                assert_eq!(report["fact"]["outcome"]["reason"], "cancelled");
                assert_eq!(fs::read(&path).unwrap(), old);
            } else {
                assert_ne!(fs::read(&path).unwrap(), old);
                assert!(
                    report["warnings"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|w| w["kind"] == "late_cancellation")
                );
            }
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_refuses_stderr_fixture_commands_before_admission() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!(
            "windows-stderr-{}",
            cpforge::build::BuildId::generate().unwrap()
        ));
    std::fs::create_dir_all(&directory).unwrap();
    for args in [
        vec!["test", "--json"],
        vec!["cmd", "flood", "--json"],
        vec!["build", "--json"],
        vec!["run"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_cpf"))
            .args(&args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(10), "{args:?}: {output:?}");
        if args == ["run"] {
            assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported platform"));
        } else {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["refusal"]["class"], "infrastructure");
            assert!(report["job"].is_null());
        }
        assert!(!directory.join(".cpforge/runs").exists());
    }
    std::fs::remove_dir_all(directory).unwrap();
}
