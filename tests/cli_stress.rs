//! Real search programs; Windows exercises the same command lines at its gate.
use cpforge::report::wire;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "cli-stress-{}",
                cpforge::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&root).unwrap();
        let f = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(root).unwrap(),
        ));
        f.write("cpforge.toml", "schema = 1\n");
        f.write("user.toml", "schema = 1\n");
        #[cfg(target_os = "macos")]
        f.write("user.toml", "schema = 1\n[languages.cpp.build]\nargv = ['g++-14', '-std=gnu++23', '-O2', '-Wall', '{sources}', '-o', '{artifact}']\n");
        f.write(
            ".cpforge/job-seq",
            "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
        );
        f.problem("", "", "");
        f.program("gen.py", "import sys\nprint(sys.argv[-1])\n");
        f.program(
            "brute.py",
            "import sys\nsys.stdout.write(sys.stdin.read())\n",
        );
        f.program(
            "main.py",
            "import sys\nsys.stdout.write(sys.stdin.read())\n",
        );
        f
    }
    fn path(&self, relative: &str) -> PathBuf {
        relative.split('/').fold(self.0.clone(), |p, c| p.join(c))
    }
    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn program(&self, name: &str, text: &str) {
        self.write(&format!("problems/A/{name}"), text);
    }
    fn problem(&self, config: &str, check: &str, stress: &str) {
        self.write("problems/A/problem.toml", &format!("schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'python'\n{config}\n{check}\n[stress]\ngenerator = {{ sources = ['gen.py'], language = 'python' }}\nbrute = {{ sources = ['brute.py'], language = 'python' }}\n{stress}\n"));
    }
    fn run(&self, args: &[&str], code: i32) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(&self.0)
            .args(["--problem", "A", "--config"])
            .arg(self.path("user.toml"))
            .args(args)
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PANIC_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .env_remove("CPF_HOOK_DEPTH")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut b = Vec::new();
            stdout.read_to_end(&mut b).unwrap();
            b
        });
        let err = std::thread::spawn(move || {
            let mut b = Vec::new();
            stderr.read_to_end(&mut b).unwrap();
            b
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() > Duration::from_secs(180) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("stress CLI watchdog expired");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        };
        assert_eq!(output.status.code(), Some(code), "{args:?}: {output:?}");
        output
    }
    fn jobs(&self) -> Vec<PathBuf> {
        fs::read_dir(self.path(".cpforge/runs"))
            .map(|d| d.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    }
    fn unchanged(&self, seq: &[u8]) {
        assert!(self.jobs().is_empty());
        assert_eq!(fs::read(self.path(".cpforge/job-seq")).unwrap(), seq);
    }
    fn search(&self, args: &[&str], code: i32) -> Option<Value> {
        let mut command = vec!["stress", "--json", "--time", "30s"];
        command.extend_from_slice(args);
        let code = if cfg!(windows) { 10 } else { code };
        let seq = fs::read(self.path(".cpforge/job-seq")).unwrap();
        let output = self.run(&command, code);
        let doc = wire::read::read_json(&output.stdout).unwrap();
        if cfg!(windows) {
            self.unchanged(&seq);
            return None;
        }
        assert_eq!(doc["command"], "stress");
        assert_eq!(doc["tests"], serde_json::json!([]));
        assert_eq!(doc["aggregates"], serde_json::json!([]));
        assert!(output.stdout.len() <= 1024 * 1024);
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!(
            "stress start seed: {}",
            doc["stress"]["seeds"]["start"].as_str().unwrap()
        )));
        Some(doc)
    }
    fn trace(&self) {
        self.program("gen.py", &format!("import sys\nwith open({:?}, 'a') as f: f.write(' '.join(sys.argv[1:]) + '\\n')\nprint(sys.argv[-1])\n", self.path("trace").to_str().unwrap()));
    }
    fn class(doc: &Value, class: &str, reason: &str) {
        assert_eq!(doc["result"]["primary_verdict"], class);
        let row = doc["stress"]["classes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["class"] == class)
            .unwrap();
        assert_eq!(row["diagnostics"][0]["reason"], reason);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn st_r01_generator_seed_is_last_argument_and_increases() {
    let f = Fixture::new();
    f.trace();
    f.write(
        "cpforge.toml",
        "schema = 1\n[languages.python.run]\nargv = ['python3', '-B', '{source}', 'prefix']\n",
    );
    if let Some(doc) = f.search(&["--seed", "5", "--iterations", "4"], 0) {
        assert_eq!(doc["stress"]["seeds"]["start"], "5");
        assert_eq!(
            fs::read_to_string(f.path("trace")).unwrap(),
            "prefix 5\nprefix 6\nprefix 7\nprefix 8\n"
        );
        assert_eq!(doc["stress"]["runs"], "12");
    }
}

#[test]
fn st_r02_seed_boundary_and_wrapping_refusal_before_admission() {
    let f = Fixture::new();
    f.trace();
    if let Some(doc) = f.search(&["--seed", "18446744073709551614", "--iterations", "2"], 0) {
        assert_eq!(doc["stress"]["seeds"]["last"], "18446744073709551615");
        assert_eq!(
            fs::read_to_string(f.path("trace")).unwrap(),
            "18446744073709551614\n18446744073709551615\n"
        );
    }
    let f = Fixture::new();
    let seq = fs::read(f.path(".cpforge/job-seq")).unwrap();
    let output = f.run(
        &[
            "stress",
            "--seed",
            "18446744073709551614",
            "--iterations",
            "3",
        ],
        if cfg!(windows) { 10 } else { 7 },
    );
    if !cfg!(windows) {
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains("18446744073709551614") && err.contains("3") && err.contains("wrap"));
    }
    f.unchanged(&seq);
}

#[test]
fn st_r03_os_seed_is_reported_printed_and_repeatable() {
    let f = Fixture::new();
    f.trace();
    if let Some(doc) = f.search(&["--iterations", "2"], 0) {
        let seed = doc["stress"]["seeds"]["start"].as_str().unwrap();
        assert!(seed.parse::<u64>().unwrap() <= i64::MAX as u64);
        let original = fs::read(f.path("trace")).unwrap();
        fs::remove_file(f.path("trace")).unwrap();
        f.search(&["--seed", seed, "--iterations", "2"], 0).unwrap();
        assert_eq!(fs::read(f.path("trace")).unwrap(), original);
    }
}

#[test]
fn st_r04_all_applicable_roles_build_once_in_contract_order() {
    let f = Fixture::new();
    f.write("problems/A/problem.toml", "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'cpp'\n[check]\nkind = 'checker'\nchecker = { sources = ['check.cpp'], language = 'cpp', profile = 'simple' }\nvalidator = { sources = ['val.cpp'], language = 'cpp', profile = 'simple' }\n[stress]\ngenerator = { sources = ['gen.cpp'], language = 'cpp' }\nbrute = { sources = ['brute.cpp'], language = 'cpp' }\n");
    f.program(
        "gen.cpp",
        "#include <iostream>\nint main(int n,char**v){std::cout<<v[n-1]<<'\\n';}\n",
    );
    for name in ["brute.cpp", "main.cpp"] {
        f.program(
            name,
            "#include <iostream>\nint main(){std::cout<<std::cin.rdbuf();}\n",
        );
    }
    for name in ["check.cpp", "val.cpp"] {
        f.program(name, "int main(){return 0;}\n");
    }
    if let Some(doc) = f.search(&["--seed", "0", "--iterations", "3"], 0) {
        // Batch has five roles; the sixth build in §3.1 is the parked interactor.
        assert_eq!(doc["executions"].as_array().unwrap().len(), 5);
        let roles: Vec<_> = doc["roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["checker", "validator", "generator", "brute"]);
        let indices: Vec<_> = doc["roles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["build"]["execution"].clone())
            .collect();
        assert_eq!(
            indices,
            serde_json::json!([1, 2, 3, 4]).as_array().unwrap().clone()
        );
        assert_eq!(doc["stress"]["runs"], "15");
        wire::read::read_roles(&doc["roles"], true).unwrap();
        assert_eq!(doc["fingerprints"]["judge"].as_str().unwrap().len(), 64);
    }
}

#[test]
fn st_r05_generator_build_failure_admits_no_iteration() {
    let f = Fixture::new();
    f.program("gen.cpp", "this is a syntax error\n");
    f.problem("", "", "");
    let problem = fs::read_to_string(f.path("problems/A/problem.toml"))
        .unwrap()
        .replace(
            "sources = ['gen.py'], language = 'python'",
            "sources = ['gen.cpp'], language = 'cpp'",
        );
    f.write("problems/A/problem.toml", &problem);
    if let Some(doc) = f.search(&["--seed", "1", "--iterations", "2"], 3) {
        assert_eq!(doc["result"]["primary_verdict"], "build_failed");
        assert_eq!(doc["stress"]["iterations"]["admitted"], "0");
        assert_eq!(doc["stress"]["stop"]["reason"], "build_failed");
    }
}

#[test]
fn st_r06_generator_exit_signal_cap_and_deadline() {
    for (source, reason, limits) in [
        ("raise SystemExit(1)\n", "nonzero_exit", ""),
        (
            "import os,signal\nos.kill(os.getpid(),signal.SIGTERM)\n",
            "signal",
            "",
        ),
        (
            "import sys\nsys.stdout.write('x'*4097)\n",
            "output_budget",
            ", stdout_cap = 4096",
        ),
        (
            "import time\ntime.sleep(10)\n",
            "wall_deadline",
            ", deadline_s = '20ms'",
        ),
    ] {
        let f = Fixture::new();
        f.program("gen.py", source);
        let p = fs::read_to_string(f.path("problems/A/problem.toml"))
            .unwrap()
            .replace(
                "sources = ['gen.py'], language = 'python'",
                &format!("sources = ['gen.py'], language = 'python'{limits}"),
            );
        f.write("problems/A/problem.toml", &p);
        if let Some(doc) = f.search(&["--seed", "1", "--iterations", "1"], 5) {
            Fixture::class(&doc, "generator_failure", reason);
            assert_eq!(doc["stress"]["runs"], "1");
        }
    }
}

#[test]
fn st_r07_validator_profiles_keep_invalid_data_separate() {
    for (profile, code, reason) in [
        ("simple", 1, "invalid_input"),
        ("testlib", 3, "validation_failed"),
    ] {
        let f = Fixture::new();
        f.program(
            "val.py",
            // A wrongly delivered input exits 0 (valid), so the job would pass.
            &format!(
                "import sys\nok = sys.stdin.read() == '5\\n'\nif {simple}:\n    ok = ok and len(sys.argv) == 2 and open(sys.argv[1]).read() == '5\\n'\nelse:\n    ok = ok and len(sys.argv) == 1\nraise SystemExit({code} if ok else 0)\n",
                simple = if profile == "simple" { "True" } else { "False" }
            ),
        );
        f.problem("", &format!("[check]\nvalidator = {{ sources = ['val.py'], language = 'python', profile = '{profile}' }}"), "");
        if let Some(doc) = f.search(&["--seed", "5", "--iterations", "1"], 5) {
            Fixture::class(&doc, "validator_failure", reason);
            assert_eq!(doc["stress"]["runs"], "2");
        }
    }
}

#[test]
fn st_r08_brute_exit_and_invalid_float_answer_skip_candidate() {
    for (source, config, reason) in [
        ("raise SystemExit(1)\n", "", "nonzero_exit"),
        (
            // Compare spec §5.5: a `special` token is an invalid float answer.
            "print('nan')\n",
            "compare = { float = { abs = 0.0001, rel = 0.0001 } }",
            "invalid_answer",
        ),
    ] {
        let f = Fixture::new();
        f.program("brute.py", source);
        f.problem(config, "", "");
        if let Some(doc) = f.search(&["--seed", "0", "--iterations", "1"], 5) {
            Fixture::class(&doc, "oracle_failure", reason);
            assert_eq!(doc["stress"]["runs"], "2");
        }
    }
}

fn testlib(f: &Fixture, outcome: &str, code: i32) {
    f.program("check.py", &format!("import sys\nassert len(sys.argv) == 6 and sys.argv[-1] == '-appes'\nopen(sys.argv[4], 'w').write('<result outcome=\"{outcome}\"></result>')\nraise SystemExit({code})\n"));
    f.problem("", "[check]\nkind = 'checker'\nchecker = { sources = ['check.py'], language = 'python', profile = 'testlib' }", "");
}

#[test]
fn st_r09_checker_declared_fail_and_signal() {
    for signal in [false, true] {
        let f = Fixture::new();
        testlib(&f, "fail", 3);
        if signal {
            f.program(
                "check.py",
                "import os,signal\nos.kill(os.getpid(),signal.SIGTERM)\n",
            );
        }
        if let Some(doc) = f.search(&["--seed", "0", "--iterations", "1"], 5) {
            Fixture::class(
                &doc,
                "judge_failure",
                if signal { "signal" } else { "declared_fail" },
            );
        }
    }
}

#[test]
fn st_r10_candidate_mismatch_deadline_wa_pe_and_score() {
    let f = Fixture::new();
    f.program("main.py", "s=input()\nprint('wrong' if s == '3' else s)\n");
    if let Some(doc) = f.search(&["--seed", "0", "--iterations", "6"], 1) {
        Fixture::class(&doc, "candidate_failure", "token_mismatch");
        assert_eq!(doc["stress"]["iterations"]["retired"], "4");
        assert_eq!(doc["stress"]["classes"][1]["first"]["seed"], "3");
        assert_eq!(doc["stress"]["stop"]["reason"], "failure");
    }
    let f = Fixture::new();
    f.program("main.py", "import time\ntime.sleep(10)\n");
    if let Some(doc) = f.search(
        &["--seed", "0", "--iterations", "1", "--deadline", "20ms"],
        1,
    ) {
        Fixture::class(&doc, "candidate_failure", "wall_deadline");
    }
    for (outcome, code, reason) in [
        ("wrong-answer", 1, "wrong_answer"),
        ("presentation-error", 2, "presentation_error"),
        ("points\" points=\"50", 7, "score_below_threshold"),
    ] {
        let f = Fixture::new();
        testlib(&f, outcome, code);
        if code == 7 {
            let p = fs::read_to_string(f.path("problems/A/problem.toml"))
                .unwrap()
                .replace(
                    "kind = 'checker'",
                    "kind = 'checker'\nscoring = { points = true, scale = 100 }",
                );
            f.write("problems/A/problem.toml", &(p + "min_score = '60'\n"));
        }
        if let Some(doc) = f.search(&["--seed", "0", "--iterations", "1"], 1) {
            Fixture::class(&doc, "candidate_failure", reason);
        }
    }
}

#[test]
fn st_r11_constructive_checker_gets_empty_answer() {
    for fail in [false, true] {
        let f = Fixture::new();
        f.program(
            "check.py",
            &format!(
                // A non-empty answer inverts the outcome, so either case fails.
                "import sys\nok = len(sys.argv) == 4 and open(sys.argv[3], 'rb').read() == b''\nraise SystemExit({0} if ok else 1 - {0})\n",
                i32::from(fail)
            ),
        );
        f.problem("", "[check]\nkind = 'checker'\nanswer = 'none'\nchecker = { sources = ['check.py'], language = 'python', profile = 'simple' }", "");
        let p = fs::read_to_string(f.path("problems/A/problem.toml"))
            .unwrap()
            .replace(
                "brute = { sources = ['brute.py'], language = 'python' }\n",
                "",
            );
        f.write("problems/A/problem.toml", &p);
        if let Some(doc) = f.search(&["--seed", "0", "--iterations", "1"], i32::from(fail)) {
            assert_eq!(doc["stress"]["runs"], "3");
            assert_eq!(
                doc["result"]["primary_verdict"],
                if fail { "candidate_failure" } else { "passed" }
            );
        }
    }
}

#[test]
fn st_r12_validation_gates_brute_and_answer_bytes_are_preserved() {
    for valid in [false, true] {
        let f = Fixture::new();
        f.program(
            "val.py",
            &format!("raise SystemExit({})\n", i32::from(!valid)),
        );
        f.program(
            "brute.py",
            "import sys\nsys.stdout.buffer.write(b'00\\n\\x00\\xff')\n",
        );
        f.program(
            "check.py",
            "import sys\nassert open(sys.argv[3], 'rb').read() == b'00\\n\\x00\\xff'\n",
        );
        f.problem("", "[check]\nkind = 'checker'\nvalidator = { sources = ['val.py'], language = 'python', profile = 'simple' }\nchecker = { sources = ['check.py'], language = 'python', profile = 'simple' }", "");
        if let Some(doc) = f.search(
            &["--seed", "0", "--iterations", "1"],
            if valid { 0 } else { 5 },
        ) {
            assert_eq!(doc["stress"]["runs"], if valid { "5" } else { "2" });
        }
    }
}

#[test]
fn st_r13_stop_on_failure_and_keep_going_exact_counts() {
    for keep in [false, true] {
        let f = Fixture::new();
        f.program(
            "main.py",
            "s=input()\nprint('wrong' if s in ['4','9'] else s)\n",
        );
        let mut args = vec!["--seed", "0", "--iterations", "30"];
        if keep {
            args.push("--keep-going");
        }
        if let Some(doc) = f.search(&args, 1) {
            let s = &doc["stress"];
            assert_eq!(s["iterations"]["retired"], if keep { "30" } else { "5" });
            assert_eq!(s["iterations"]["saved"], 0);
            assert_eq!(s["iterations"]["not_saved"], if keep { "2" } else { "1" });
            assert_eq!(s["classes"][0]["class"], "passed");
            assert_eq!(s["classes"][0]["count"], if keep { "28" } else { "4" });
            assert_eq!(s["classes"][1]["class"], "candidate_failure");
            assert_eq!(s["classes"][1]["count"], if keep { "2" } else { "1" });
            assert_eq!(
                s["stop"]["reason"],
                if keep { "iterations" } else { "failure" }
            );
            let kept: &[u64] = if keep { &[4, 9] } else { &[4] };
            for k in kept {
                assert!(
                    f.jobs()[0]
                        .join("iterations")
                        .join(format!("{k}.candidate"))
                        .join("out")
                        .join("stdout")
                        .exists()
                );
            }
            // Saving adds manifests, promoted contents and failure refs in the next layer.
            assert!(!f.path("problems/A/stress/failures").exists());
        }
    }
}

#[test]
fn st_r14_two_hundred_passes_leave_no_iteration_entry() {
    let f = Fixture::new();
    if let Some(doc) = f.search(&["--seed", "0", "--iterations", "200"], 0) {
        assert_eq!(doc["result"]["primary_verdict"], "passed");
        assert_eq!(doc["stress"]["iterations"]["retired"], "200");
        assert_eq!(
            fs::read_dir(f.jobs()[0].join("iterations"))
                .unwrap()
                .count(),
            0
        );
        assert!(!f.path("problems/A/stress/failures").exists());
    }
}

#[test]
fn st_r15_time_budget_finishes_the_live_iteration() {
    let f = Fixture::new();
    f.program(
        "brute.py",
        "import sys,time\ntime.sleep(.3)\nsys.stdout.write(sys.stdin.read())\n",
    );
    let before = Instant::now();
    let args = [
        "stress",
        "--json",
        "--seed",
        "0",
        "--iterations",
        "100",
        "--time",
        "1s",
    ];
    let out = f.run(&args, if cfg!(windows) { 10 } else { 0 });
    if cfg!(windows) {
        assert!(f.jobs().is_empty());
        return;
    }
    let doc = wire::read::read_json(&out.stdout).unwrap();
    assert_eq!(doc["stress"]["stop"]["reason"], "time_budget");
    assert!(
        doc["stress"]["iterations"]["retired"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            >= 1
    );
    assert!(before.elapsed() < Duration::from_secs(7));
}

#[test]
fn st_r18_file_io_and_interactive_refuse_before_admission() {
    let f = Fixture::new();
    f.problem("io = { input = 'input.txt' }", "", "");
    let seq = fs::read(f.path(".cpforge/job-seq")).unwrap();
    let out = f.run(&["stress"], 7);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("stress with file-based I/O is unsupported in this version")
    );
    f.unchanged(&seq);
    f.problem("", "[check]\nkind = 'interactive'\nanswer = 'none'\n[check.interactive]\nchecking = 'interactor'\ninteractor = { sources = ['int.py'], language = 'python', profile = 'testlib' }", "");
    let out = f.run(&["stress"], 7);
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsupported in this version"));
    f.unchanged(&seq);
}

#[test]
fn st_r19_stress_diagnostics_and_inspection_are_consumer_specific() {
    for (change, key) in [
        ("missing", "generator"),
        ("profile", "generator.profile"),
        ("seed", "brute.seed"),
        ("iterations = 0", "iterations"),
        ("time_s = 0", "time_s"),
        ("unknown = true", "unknown"),
        ("no-brute", "brute"),
        ("min_score = '60'", "min_score"),
        ("over-scale", "min_score"),
    ] {
        let f = Fixture::new();
        f.problem("", "", if change.contains('=') { change } else { "" });
        let mut p = fs::read_to_string(f.path("problems/A/problem.toml")).unwrap();
        p = match change {
            "missing" => p.replace("generator = { sources = ['gen.py'], language = 'python' }\n", ""),
            "profile" => p.replace("sources = ['gen.py'], language = 'python'", "sources = ['gen.py'], language = 'python', profile = 'simple'"),
            "seed" => p.replace("sources = ['brute.py'], language = 'python'", "sources = ['brute.py'], language = 'python', seed = 'env'"),
            "no-brute" => p.replace("brute = { sources = ['brute.py'], language = 'python' }\n", ""),
            "over-scale" => p.replace("[stress]", "[check]\nkind = 'checker'\nscoring = { points = true, scale = 100 }\nchecker = { sources = ['check.py'], language = 'python', profile = 'simple' }\n[stress]") + "min_score = '101'\n",
            _ => p,
        };
        f.write("problems/A/problem.toml", &p);
        let seq = fs::read(f.path(".cpforge/job-seq")).unwrap();
        for args in [
            vec!["stress"],
            vec!["config", "check"],
            vec!["config", "show", "--json"],
        ] {
            let output = f.run(&args, 7);
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(&format!("stress.{key}")),
                "{change}: {output:?}"
            );
            f.unchanged(&seq);
        }
        f.program("check.py", "raise SystemExit(0)\n");
        f.program("tests/a.in", "7\n");
        f.program("tests/a.ans", "7\n");
        f.program("tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n");
        f.run(&["test"], if cfg!(windows) { 10 } else { 0 });
    }
    let f = Fixture::new();
    let out = f.run(&["config", "show", "--origin"], 0);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("stress")
            && text.contains("10000")
            && text.contains("gen.py")
            && text.contains("problem.toml")
    );
    assert!(f.jobs().is_empty());
}

#[test]
fn st_r20_default_candidate_cap_and_exact_generator_cap() {
    let f = Fixture::new();
    f.program(
        "main.py",
        "import sys\nsys.stdout.buffer.write(b'x' * (64*1024*1024+1))\n",
    );
    if let Some(doc) = f.search(&["--seed", "0", "--iterations", "1"], 1) {
        Fixture::class(&doc, "candidate_failure", "output_budget");
    }
    let f = Fixture::new();
    f.program(
        "gen.py",
        "import sys\nsys.stdout.buffer.write(b'x' * (64*1024*1024))\n",
    );
    if let Some(doc) = f.search(&["--seed", "0", "--iterations", "1"], 0) {
        assert_eq!(
            doc["stress"]["captured_bytes"],
            (3u64 * 64 * 1024 * 1024).to_string()
        );
    }
}

#[test]
fn stress_usage_errors_allocate_nothing_and_replay_stays_unavailable() {
    let f = Fixture::new();
    let id = "f-0123456789abcdef0123456789abcdef-0-0";
    let seq = fs::read(f.path(".cpforge/job-seq")).unwrap();
    for args in [
        vec!["--iterations", "0"],
        vec!["--iterations", "x"],
        vec!["--seed", "18446744073709551616"],
        vec!["--time", "0"],
        vec!["--json", "--events"],
        vec!["--replay", id, "--seed", "1"],
        vec!["--replay", id, "--iterations", "5"],
        vec!["--replay-binaries", id, "--keep-going"],
        vec!["--replay", id, "--replay-binaries", id],
        vec!["--replay", "f-ABC"],
        vec!["--replay-binaries", "nope"],
    ] {
        let mut command = vec!["stress"];
        command.extend(args);
        f.run(&command, 2);
        f.unchanged(&seq);
    }
    for option in ["--replay", "--replay-binaries"] {
        let out = f.run(&["stress", option, id], 2);
        assert!(String::from_utf8_lossy(&out.stderr).contains("not available yet"));
        f.unchanged(&seq);
    }
}

#[test]
fn stress_events_use_iteration_identities_and_end_with_the_report() {
    let f = Fixture::new();
    let out = f.run(
        &[
            "stress",
            "--seed",
            "5",
            "--iterations",
            "2",
            "--events=output",
        ],
        if cfg!(windows) { 10 } else { 0 },
    );
    let lines: Vec<Value> = out
        .stdout
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| wire::read::read_json(l).unwrap())
        .collect();
    assert_eq!(lines[0]["document"], "event_stream");
    assert_eq!(lines.last().unwrap()["event"], "report");
    let mut previous = None;
    for line in &lines {
        if let Some(seq) = line["seq"].as_str() {
            let seq = seq.parse::<u64>().unwrap();
            assert!(previous.is_none_or(|p| p < seq));
            previous = Some(seq);
        }
    }
    if cfg!(windows) {
        assert!(f.jobs().is_empty());
        return;
    }
    assert!(
        lines
            .iter()
            .any(|l| l["event"] == "iteration_run_started" && l["run"] == "1.candidate")
    );
    assert!(
        lines
            .iter()
            .filter(|l| l["event"] == "iteration_run_started")
            .all(|l| !l["run"].as_str().unwrap().starts_with('r'))
    );
    assert_eq!(
        lines.last().unwrap()["report"]["stress"]["seeds"]["start"],
        "5"
    );
}

#[test]
fn stress_shell_role_assets_prebuilt_brute_and_config_budgets() {
    let f = Fixture::new();
    f.write("cpforge.toml", r#"schema = 1
[languages.generator]
sources = ["gen.py"]
build = false
[languages.generator.run]
shell = { interpreter = ["sh", "-c"], script = 'test "$0" = cpf || exit 9; cat words; printf "%s\n" "$2"', args = ["{source}"] }
"#);
    f.program("words", "prefix\n");
    f.program("brute.sh", "#!/bin/sh\ncat\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            f.path("problems/A/brute.sh"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    f.write("problems/A/problem.toml", "schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = 'python'\n[stress]\ngenerator = { sources = ['gen.py'], language = 'generator', assets = ['words'] }\nbrute = { program = 'brute.sh' }\niterations = 2\ntime_s = '5s'\n");
    if let Some(doc) = f.search(&["--seed", "9"], 0) {
        assert_eq!(doc["stress"]["iterations"]["retired"], "2");
        assert_eq!(doc["stress"]["budget"]["iterations"], "2");
        assert!(doc["fingerprints"]["judge"].is_null());
    }
}

#[cfg(windows)]
#[test]
fn st_r16_real_batch_plateau_is_refused_before_admission() {
    let f = Fixture::new();
    f.search(&["--iterations", "2000", "--seed", "0"], 0);
}

#[cfg(windows)]
#[test]
fn st_r17_blocked_delivery_search_is_refused_before_admission() {
    let f = Fixture::new();
    f.search(&["--iterations", "2000", "--seed", "19", "--keep-going"], 0);
}

#[test]
fn stress_role_plan_errors_precede_admission_but_follow_the_platform_gate() {
    for case in ["missing", "guard", "limit"] {
        let f = Fixture::new();
        if case == "missing" {
            fs::remove_file(f.path("problems/A/gen.py")).unwrap();
        }
        if case == "guard" {
            f.write("cpforge.toml", "schema = 1\n[languages.generator]\nsources = ['gen.py']\nbuild = false\n[languages.generator.run]\nargv = ['python3', '-B', '{source}', '../../../../../../outside']\n");
        }
        if case == "limit" {
            f.write("cpforge.toml", "schema = 1\n[languages.generator]\nsources = ['gen.py']\nbuild = false\n[languages.generator.run]\nargv = ['python3', '-B', '{source}']\nlimits = { job_commit = '1GiB' }\n");
        }
        if case != "missing" {
            let p = fs::read_to_string(f.path("problems/A/problem.toml"))
                .unwrap()
                .replace(
                    "sources = ['gen.py'], language = 'python'",
                    "sources = ['gen.py'], language = 'generator'",
                );
            f.write("problems/A/problem.toml", &p);
        }
        let seq = fs::read(f.path(".cpforge/job-seq")).unwrap();
        f.run(
            &["stress", "--seed", "0", "--iterations", "1"],
            if cfg!(windows) { 10 } else { 7 },
        );
        f.unchanged(&seq);
    }
}
