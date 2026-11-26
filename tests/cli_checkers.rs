//! Judging gates, refusal precedence and CLI report delivery.

use cpforge::report::wire;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "cli-checkers-{}",
                cpforge::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&root).unwrap();
        let fixture = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(root).unwrap(),
        ));
        // These command plans need no executable probing before the refusal.
        fixture.write(
            "cpforge.toml",
            "schema = 1\n\
             [commands.building]\nneeds_build = true\nargv = ['unused']\n\
             [commands.testing]\nfor_each_test = true\nargv = ['unused']\n\
             [commands.independent]\nargv = ['sh', '-c', 'printf ok']\n\
             [commands.problem]\nargv = ['sh', '-c', 'printf ok', '{problem_id}']\n",
        );
        fixture.write("user/cpforge/config.toml", "schema = 1\n");
        fixture.write(
            ".cpforge/job-seq",
            "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
        );
        fixture.problem("");
        fixture.write("problems/A/main.py", "# build-free target\n");
        fixture.write("problems/A/tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n");
        fixture.write("problems/A/tests/a.in", "input\n");
        fixture.write("problems/A/tests/a.ans", "ok\n");
        fixture
    }

    fn path(&self, relative: &str) -> PathBuf {
        relative
            .split('/')
            .fold(self.0.clone(), |path, part| path.join(part))
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn problem(&self, declarations: &str) {
        self.write(
            "problems/A/problem.toml",
            &format!(
                "schema = 1\n{declarations}[problem]\nid = 'local:A'\n\
                 [config]\nlanguage = 'python'\n\
                 [config.run]\nargv = ['sh', '-c', 'printf ok']\n"
            ),
        );
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cpf"))
            .current_dir(&self.0)
            .env("HOME", self.path("user"))
            .env("USERPROFILE", self.path("user"))
            .env("XDG_CONFIG_HOME", self.path("user"))
            .env("APPDATA", self.path("user"))
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .env_remove("CPF_HOOK_DEPTH")
            .stdin(Stdio::null())
            .args(args)
            .output()
            .unwrap()
    }

    fn no_admission(&self, sequence: &[u8]) {
        assert_eq!(fs::read(self.path(".cpforge/job-seq")).unwrap(), sequence);
        assert!(!self.path(".cpforge/runs").exists());
    }

    fn inspectable(&self) {
        for args in [
            vec!["--problem", "A", "config", "show", "--json"],
            vec!["--problem", "A", "config", "check"],
        ] {
            let output = self.run(&args);
            assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        }
        let output = self.run(&["--problem", "A", "config", "show", "--origin"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("check.kind"), "{text}");
        assert!(text.contains("problem.toml"), "{text}");
    }

    fn refuses(&self, key: &str) {
        let sequence = fs::read(self.path(".cpforge/job-seq")).unwrap();
        let original = fs::read(self.path("problems/A/problem.toml")).unwrap();
        for args in [
            vec!["--problem", "A", "build", "--json"],
            vec!["--problem", "A", "test", "--json"],
            vec!["--problem", "A", "run"],
            vec!["--problem", "A", "cmd", "building", "--json"],
            vec!["--problem", "A", "cmd", "testing", "--json"],
        ] {
            let output = self.run(&args);
            // Report §5.2: config (7) precedes platform infrastructure (10),
            // including on Windows, before any tools or test data are read.
            assert_eq!(output.status.code(), Some(7), "{args:?}: {output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("unsupported in this version"), "{stderr}");
            assert!(stderr.contains(key), "{stderr}");
            assert!(!stderr.contains("unsupported platform"), "{stderr}");
            if args.last() == Some(&"--json") {
                let doc = document(&output);
                assert_eq!(doc["command"], args[2]);
                assert_eq!(doc["outcome"], "refused");
                assert_eq!(doc["problem"]["identity"], "local:A");
                assert_eq!(doc["refusal"]["class"], "config");
                assert_eq!(doc["refusal"]["error"]["kind"], "config");
                assert!(
                    doc["refusal"]["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains(key)
                );
                assert!(doc["refusal"]["diagnostics"].is_array());
                assert_eq!(doc["result"]["verdict"], "refused");
                assert_eq!(doc["result"]["primary_verdict"], "refused");
                assert_eq!(doc["result"]["pre_delivery_exit"]["code"], 7);
                assert!(doc["result"]["pre_delivery_exit"]["reraise"].is_null());
                assert_eq!(doc["persistence"]["report"]["kind"], "not_attempted");
                assert_eq!(doc["persistence"]["report"]["reason"], "refusal");
                for member in ["job", "target", "capture", "build", "fingerprints", "fact"] {
                    assert!(doc[member].is_null(), "{member}: {doc}");
                }
                for member in ["executions", "tests", "aggregates", "hooks"] {
                    assert_eq!(doc[member], serde_json::json!([]), "{member}: {doc}");
                }
            } else {
                assert!(output.stdout.is_empty(), "{output:?}");
            }
            self.no_admission(&sequence);
            assert!(!self.path(".cpforge/lock").exists());
            assert_eq!(
                fs::read(self.path("problems/A/problem.toml")).unwrap(),
                original
            );
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn document(output: &Output) -> Value {
    assert_eq!(output.stdout.last(), Some(&b'\n'), "{output:?}");
    assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    assert_eq!(wire::read::report_schema_prefix(&output.stdout).unwrap(), 1);
    let doc = wire::read::read_json(&output.stdout).unwrap();
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["document"], "job_report");
    doc
}

#[test]
fn checker_kind_refuses_before_admission() {
    let fixture = Fixture::new();
    fixture.problem(
        "[check]\nkind = 'checker'\nchecker = { program = 'checker', profile = 'testlib' }\n",
    );
    fixture.inspectable();
    fixture.refuses("check.kind");
    for kind in ["checker", "multi-file", "answer", "unknown"] {
        fixture.write("problems/A/problem.toml", &format!(
            "schema = 1\n[problem]\nid = 'local:A'\nkind = '{kind}'\n[config]\nlanguage = 'python'\n"
        ));
        fixture.refuses("problem kind");
    }
    // An interactive problem kind without `[check] kind = "interactive"`: a
    // phase-2 error for `cpf test` (config §9), the kind refusal otherwise.
    fixture.write(
        "problems/A/problem.toml",
        "schema = 1\n[problem]\nid = 'local:A'\nkind = 'interactive'\n[config]\nlanguage = 'python'\n",
    );
    let sequence = fs::read(fixture.path(".cpforge/job-seq")).unwrap();
    let output = fixture.run(&["--problem", "A", "test", "--json"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("interactive problem needs"),
        "{output:?}"
    );
    fixture.no_admission(&sequence);
    for args in [
        vec!["--problem", "A", "build", "--json"],
        vec!["--problem", "A", "run"],
        vec!["--problem", "A", "cmd", "building", "--json"],
        vec!["--problem", "A", "cmd", "testing", "--json"],
    ] {
        let output = fixture.run(&args);
        assert_eq!(output.status.code(), Some(7), "{args:?}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("problem kind"), "{stderr}");
        assert!(stderr.contains("unsupported in this version"), "{stderr}");
        fixture.no_admission(&sequence);
    }
    // Interactive declarations are inspectable while their execution stays gated.
    fixture.problem("[check]\nkind = 'interactive'\n[check.interactive]\nchecking = 'interactor'\ninteractor = { program = 'interactor', profile = 'testlib' }\n");
    fixture.inspectable();
    fixture.refuses("check.kind");
    let document = fs::read_to_string(fixture.path("problems/A/problem.toml")).unwrap();
    fixture.write(
        "problems/A/problem.toml",
        &document.replace("[problem]", "[problem]\nkind = 'interactive'"),
    );
    fixture.refuses("problem kind");
}

#[test]
fn checker_spec_refuses_before_admission() {
    let fixture = Fixture::new();
    for spec in [
        "sources = ['checker.cpp'], support_files = ['testlib.h'], assets = ['data/dict'], language = 'cpp', entry = 'checker', profile = 'testlib', deadline_s = '2s', stdout_cap = '1MiB', stderr_cap = 1024",
        "program = 'bin/checker', profile = 'testlib'",
    ] {
        fixture.problem(&format!(
            "[check]\nkind = 'checker'\nchecker = {{ {spec} }}\n"
        ));
        fixture.inspectable();
        fixture.refuses("check.kind");
    }
}

#[test]
fn validator_spec_refuses_before_admission() {
    let fixture = Fixture::new();
    fixture.problem("[check]\nvalidator = { sources = ['validator.cpp'], language = 'cpp', profile = 'testlib' }\n");
    fixture.inspectable();
    fixture.refuses("check.validator");
}

#[test]
fn interactor_settings_refuse_before_admission() {
    let fixture = Fixture::new();
    for checking in ["interactor", "checker", "interactor+checker"] {
        let checker = if checking == "interactor" {
            ""
        } else {
            "checker = { program = 'checker', profile = 'simple' }\n"
        };
        fixture.problem(&format!("[check]\nkind = 'interactive'\n{checker}answer = 'none'\n[check.interactive]\nchecking = '{checking}'\ninteractor = {{ program = 'interactor', profile = 'testlib', seed = 'env' }}\nmode = 'direct'\nidle_s = 0\nearly_reader_close = false\ninteractor_allowance_s = 1.0\npeer_allowance_s = 1.0\nsolution_seed = 'env'\n[check.interactor_test]\ngood = [{{ sources = ['ok.cpp'], language = 'cpp' }}]\nbad = []\n"));
        fixture.inspectable();
        fixture.refuses("check.kind");
    }
}

#[test]
fn answer_policy_refuses_before_admission() {
    let fixture = Fixture::new();
    for answer in ["none", "file"] {
        fixture.problem(&format!("[check]\nkind = 'checker'\nchecker = {{ program = 'checker', profile = 'testlib' }}\nanswer = '{answer}'\n"));
        fixture.inspectable();
        fixture.refuses("check.kind");
    }
}

#[test]
fn scoring_policy_refuses_before_admission() {
    let fixture = Fixture::new();
    for scoring in [
        "'pass_fail'",
        "{ points = true, scale = 100, aggregation = 'sum' }",
        "{ points = true, scale = 100, aggregation = 'min' }",
    ] {
        fixture.problem(&format!("[check]\nkind = 'checker'\nchecker = {{ program = 'checker', profile = 'testlib' }}\nscoring = {scoring}\ndisplay = 'judge-cf'\n"));
        fixture.inspectable();
        fixture.refuses("check.kind");
    }
}

#[test]
fn invalid_judging_is_a_schema_error_for_inspection_and_execution() {
    let fixture = Fixture::new();
    for (declaration, key) in [
        ("[check]\nkind = 'future-kind'\n", "check.kind"),
        (
            "[check]\nchecker = { program = 'checker', profile = 'simple' }\n",
            "check.checker",
        ),
        (
            "[check]\nkind = 'checker'\nchecker = { path = 'checker' }\n",
            "check.checker.path",
        ),
        ("[check]\ninteractor = {}\n", "check.interactor"),
        ("[check]\nnotes = 'x'\n", "check.notes"),
        ("[check]\nanswer = 'none'\n", "check.answer"),
        ("[check]\nscoring = {}\n", "check.scoring"),
        (
            "[check.interactive]\nmode = 'direct'\n",
            "check.interactive",
        ),
    ] {
        fixture.problem(declaration);
        let sequence = fs::read(fixture.path(".cpforge/job-seq")).unwrap();
        for args in [
            vec!["--problem", "A", "config", "show", "--json"],
            vec!["--problem", "A", "test", "--json"],
        ] {
            let output = fixture.run(&args);
            assert_eq!(output.status.code(), Some(7), "{args:?}: {output:?}");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains(key),
                "{output:?}"
            );
            fixture.no_admission(&sequence);
        }
    }
}

#[test]
fn inspection_and_commands_without_dependencies_remain_available() {
    let fixture = Fixture::new();
    fixture.problem(
        "[check]\nkind = 'checker'\nchecker = { program = 'checker', profile = 'simple' }\n",
    );
    let original = fs::read(fixture.path("problems/A/problem.toml")).unwrap();
    let sequence = fs::read(fixture.path(".cpforge/job-seq")).unwrap();
    for args in [
        vec!["--problem", "A", "config", "show", "--json"],
        vec!["switch", "A"],
        vec!["current"],
    ] {
        let output = fixture.run(&args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
    }
    fixture.no_admission(&sequence);
    for args in [
        vec!["cmd", "independent", "--json"],
        vec!["--problem", "A", "cmd", "independent", "--json"],
        vec!["--problem", "A", "cmd", "problem", "--json"],
    ] {
        let output = fixture.run(&args);
        let doc = document(&output);
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
            assert_eq!(doc["outcome"], "finished");
            assert_eq!(doc["result"]["verdict"], "succeeded");
        } else {
            assert_eq!(output.status.code(), Some(10), "{args:?}: {output:?}");
            assert_eq!(doc["refusal"]["class"], "infrastructure");
            fixture.no_admission(&sequence);
        }
    }
    assert_eq!(
        fs::read(fixture.path("problems/A/problem.toml")).unwrap(),
        original
    );
}

#[test]
fn non_execution_keys_do_not_change_jobs() {
    for declaration in [
        "[metadata]\nnotes = 'preserved'\n",
        "[check]\nkind = 'compare'\nanswer = 'file'\nscoring = 'pass_fail'\ndisplay = 'judge-cf'\n",
        "[check]\n",
    ] {
        let fixture = Fixture::new();
        fixture.problem(declaration);
        let original = fs::read(fixture.path("problems/A/problem.toml")).unwrap();
        let sequence = fs::read(fixture.path(".cpforge/job-seq")).unwrap();
        for command in ["build", "test", "run"] {
            let mut args = vec!["--problem", "A", command];
            if command != "run" {
                args.push("--json");
            }
            let output = fixture.run(&args);
            if cfg!(any(target_os = "linux", target_os = "macos")) {
                assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
                assert!(fixture.path(".cpforge/runs").is_dir());
                if command == "test" {
                    let doc = document(&output);
                    assert_eq!(doc["tests"][0]["verdict"]["kind"], "passed");
                    assert_eq!(doc["tests"][0]["check"]["kind"], "passed");
                }
            } else {
                assert_eq!(output.status.code(), Some(10), "{args:?}: {output:?}");
                fixture.no_admission(&sequence);
            }
        }
        assert_eq!(
            fs::read(fixture.path("problems/A/problem.toml")).unwrap(),
            original
        );
    }
}

#[test]
fn per_test_check_refuses_only_selected_entries_before_admission() {
    for value in ["{ kind = 'checker' }", "'x'", "false", "42"] {
        let fixture = Fixture::new();
        fixture.write("cpforge.toml", "schema = 1\n[commands.testing]\nfor_each_test = true\nargv = ['sh', '-c', 'printf ok']\n");
        let catalog = format!(
            "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\ncheck = {value} # keep this declaration\n\n[[test]]\nid = 'b'\ninput = 'tests/b.in'\nanswer = 'tests/b.ans'\norigin = 'custom'\nenabled = true\n"
        );
        fixture.write("problems/A/tests.toml", &catalog);
        fixture.write("problems/A/tests/b.in", "input\n");
        fixture.write("problems/A/tests/b.ans", "ok\n");
        let sequence = fs::read(fixture.path(".cpforge/job-seq")).unwrap();
        for args in [
            vec!["--problem", "A", "test", "--json"],
            vec!["--problem", "A", "test", "--case", "a", "--json"],
        ] {
            let output = fixture.run(&args);
            assert_eq!(output.status.code(), Some(7), "{args:?}: {output:?}");
            let message = String::from_utf8_lossy(&output.stderr);
            assert!(message.contains("unsupported in this version"), "{message}");
            assert!(message.contains("tests.toml"), "{message}");
            assert!(message.contains("test[0].check"), "{message}");
            fixture.no_admission(&sequence);
        }
        let expected = if cfg!(any(target_os = "linux", target_os = "macos")) {
            0
        } else {
            10
        };
        // Storage §8.1 refuses the reserved key for judged jobs only; a
        // command run over the tests does not judge and leaves it inert.
        let output = fixture.run(&["--problem", "A", "cmd", "testing", "--json"]);
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
        let output = fixture.run(&["--problem", "A", "test", "ls"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(
            fs::read_to_string(fixture.path("problems/A/tests.toml")).unwrap(),
            catalog
        );
        let output = fixture.run(&["--problem", "A", "test", "disable", "a"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let edited = fs::read_to_string(fixture.path("problems/A/tests.toml")).unwrap();
        assert!(
            edited.contains(&format!("check = {value} # keep this declaration")),
            "{edited}"
        );
        for args in [
            vec!["--problem", "A", "test", "--case", "b", "--json"],
            vec!["--problem", "A", "test", "--json"],
            vec!["--problem", "A", "cmd", "testing", "--json"],
        ] {
            let output = fixture.run(&args);
            assert_eq!(output.status.code(), Some(expected), "{args:?}: {output:?}");
        }
        // Explicit selection of a disabled entry still selects its reserved key.
        let output = fixture.run(&["--problem", "A", "test", "--case", "a", "--json"]);
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported in this version"));
    }
}

#[test]
fn inspection_exposes_judging_values_and_each_origin() {
    let fixture = Fixture::new();
    fixture.problem("[check]\nkind = 'checker'\nchecker = { sources = ['c.py'], language = 'python', profile = 'simple', assets = ['data/dict'] }\nvalidator = { program = 'v', profile = 'testlib' }\nanswer = 'none'\nscoring = { points = true, scale = 100 }\ndisplay = 'judge-cf'\n");
    let output = fixture.run(&["--problem", "A", "config", "show", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let doc: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["config"]["check"]["kind"], "checker");
    assert_eq!(doc["config"]["check"]["answer"], "none");
    assert_eq!(doc["config"]["check"]["scoring"]["scale"], "100");
    assert_eq!(doc["config"]["check"]["scoring"]["aggregation"], "sum");
    assert_eq!(
        doc["config"]["check"]["checker"]["sources"][0]["utf8"],
        "c.py"
    );
    assert_eq!(
        doc["config"]["check"]["checker"]["assets"][0]["utf8"],
        "data/dict"
    );
    assert_eq!(doc["config"]["check"]["validator"]["program"]["utf8"], "v");
    for path in [
        vec!["check", "kind"],
        vec!["check", "checker", "sources", "0"],
        vec!["check", "checker", "assets", "0"],
        vec!["check", "validator", "program"],
    ] {
        let origin = doc["origins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|origin| origin["path"] == serde_json::json!(path))
            .unwrap();
        assert_eq!(origin["origin"]["kind"], "file");
        assert!(
            origin["origin"]["path"]["utf8"]
                .as_str()
                .unwrap()
                .ends_with("problem.toml")
        );
        assert!(origin["origin"]["line"].is_number() && origin["origin"]["col"].is_number());
    }
    let origin = doc["origins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|origin| origin["path"] == serde_json::json!(["check", "scoring", "aggregation"]))
        .unwrap();
    assert_eq!(origin["origin"]["kind"], "built_in");
}

#[test]
fn simple_judging_delivers_report_shape_labels_and_platform_refusal() {
    let fixture = Fixture::new();
    fixture.problem("[check]\nkind = 'checker'\nchecker = { sources = ['checker.py'], language = 'python', profile = 'simple' }\nvalidator = { sources = ['validator.py'], language = 'python', profile = 'simple' }\n");
    // A failed expectation exits 3 (a judge failure), never 1 (the rejection).
    fixture.write("problems/A/checker.py", "import pathlib, sys\nif len(sys.argv) != 4 or pathlib.Path(sys.argv[2]).read_text() != 'ok' or pathlib.Path(sys.argv[3]).read_text() != 'ok\\n':\n    sys.exit(3)\nprint('custom rejection', file=sys.stderr)\nsys.exit(1)\n");
    fixture.write("problems/A/validator.py", "import pathlib, sys\nassert len(sys.argv) == 2\nassert pathlib.Path(sys.argv[1]).read_bytes() == sys.stdin.buffer.read()\n");
    let sequence = fs::read(fixture.path(".cpforge/job-seq")).unwrap();
    let output = fixture.run(&["--problem", "A", "test", "--json"]);
    let doc = document(&output);
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(doc["target"]["check"]["kind"], "checker");
        assert_eq!(doc["roles"].as_array().unwrap().len(), 2);
        assert_eq!(doc["executions"].as_array().unwrap().len(), 3);
        let test = &doc["tests"][0];
        assert_eq!(test["executions"]["first"], 0);
        assert_eq!(test["executions"]["count"], 3);
        assert_eq!(test["validation"]["outcome"], "valid");
        assert_eq!(test["check"]["kind"], "rejected");
        assert_eq!(test["check"]["fail"], "wrong_answer");
        assert_eq!(test["check"]["role"], "checker");
        assert_eq!(test["diagnostic"], false);
        assert_eq!(test["verdict"]["kind"], "wrong_answer");
        wire::read::read_check_config(&doc["target"]["check"]).unwrap();
        wire::read::read_validation(&test["validation"]).unwrap();
        wire::read::read_judge(&test["judges"][0]).unwrap();
        wire::read::read_roles(&doc["roles"], false).unwrap();
        assert_eq!(test["judges"][0]["profile"], "simple");
        assert_eq!(test["judges"][0]["execution"], 2);
        assert_eq!(test["judges"][0]["result"]["kind"], "rejected");
        assert!(test["judges"][0]["message"].is_null());
        assert!(test["judges"][0]["report"].is_null());
        assert!(doc["fingerprints"]["judge"].is_string(), "{doc}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("simple checker") && stderr.contains("inputs validated"),
            "{stderr}"
        );
        assert!(
            stderr.contains("WA")
                && stderr.contains("checker")
                && stderr.contains("custom rejection"),
            "{stderr}"
        );
    } else {
        assert_eq!(output.status.code(), Some(10), "{output:?}");
        assert_eq!(doc["refusal"]["class"], "infrastructure");
        fixture.no_admission(&sequence);
    }
}

#[test]
fn simple_checker_answer_none_stages_empty_answer_and_preserves_absence() {
    let fixture = Fixture::new();
    fixture.problem("[check]\nkind = 'checker'\nanswer = 'none'\nchecker = { sources = ['checker.py'], language = 'python', profile = 'simple' }\n");
    fixture.write(
        "problems/A/tests.toml",
        "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\norigin = 'custom'\nenabled = true\n",
    );
    fixture.write("problems/A/checker.py", "import pathlib, sys\nassert len(sys.argv) == 4\nassert pathlib.Path(sys.argv[3]).read_bytes() == b''\nassert pathlib.Path(sys.argv[2]).read_text() == 'ok'\n");
    let sequence = fs::read(fixture.path(".cpforge/job-seq")).unwrap();
    let output = fixture.run(&["--problem", "A", "test", "--json"]);
    let doc = document(&output);
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(doc["target"]["check"]["answer"], "none");
        assert_eq!(doc["tests"][0]["answer"], "absent");
        assert_eq!(doc["tests"][0]["check"]["kind"], "passed");
        assert_eq!(doc["tests"][0]["verdict"]["kind"], "passed");
    } else {
        assert_eq!(output.status.code(), Some(10), "{output:?}");
        fixture.no_admission(&sequence);
    }
}

#[test]
fn simple_judging_configuration_errors_precede_platform_refusal() {
    let fixture = Fixture::new();
    fixture.problem("[check]\nkind = 'checker'\nanswer = 'invalid'\nchecker = { program = 'checker', profile = 'simple' }\n");
    let sequence = fs::read(fixture.path(".cpforge/job-seq")).unwrap();
    let output = fixture.run(&["--problem", "A", "test", "--json"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(document(&output)["refusal"]["class"], "config");
    fixture.no_admission(&sequence);
}
