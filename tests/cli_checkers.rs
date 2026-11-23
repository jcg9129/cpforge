//! Unsupported judging declarations are configuration errors before admission.

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
    for kind in ["checker", "interactive", "future-kind"] {
        fixture.problem(&format!("[check]\nkind = '{kind}'\n"));
        fixture.refuses("check.kind");
    }
    // The existing problem-kind gate applies on all planning platforms too.
    for kind in ["checker", "interactive", "multi-file", "answer", "unknown"] {
        fixture.write(
            "problems/A/problem.toml",
            &format!(
                "schema = 1\n[problem]\nid = 'local:A'\nkind = '{kind}'\n\
                 [config]\nlanguage = 'python'\n\
                 [config.run]\nargv = ['sh', '-c', 'printf ok']\n"
            ),
        );
        fixture.refuses("problem kind");
    }
}

#[test]
fn checker_spec_refuses_before_admission() {
    let fixture = Fixture::new();
    for declaration in [
        "[check]\nchecker = { sources = ['checker.cpp'], language = 'cpp', profile = 'testlib' }\n",
        "[check]\nkind = 'compare'\nchecker = { path = 'checker' }\n",
        "[check.checker]\n",
        "check = { checker = false }\n",
    ] {
        fixture.problem(declaration);
        fixture.refuses("check.checker");
    }
}

#[test]
fn validator_spec_refuses_before_admission() {
    let fixture = Fixture::new();
    for declaration in [
        "[check]\nvalidator = { sources = ['validator.cpp'], language = 'cpp', profile = 'testlib-validator' }\n",
        "[check.validator]\n",
    ] {
        fixture.problem(declaration);
        fixture.refuses("check.validator");
    }
}

#[test]
fn interactor_settings_refuse_before_admission() {
    let fixture = Fixture::new();
    fixture.problem("[check]\ninteractor = { path = 'interactor' }\n");
    fixture.refuses("check.interactor");
    for field in [
        "",
        "interactor = { sources = ['interactor.cpp'], language = 'cpp', profile = 'testlib', seed = 'env' }\n",
        "checking = 'interactor'\n",
        "checking = 'checker'\n",
        "checking = 'interactor+checker'\n",
        "mode = 'direct'\n",
        "idle_s = 1\n",
        "early_reader_close = false\n",
        "interactor_allowance_s = 1.0\n",
        "peer_allowance_s = 1.0\n",
    ] {
        fixture.problem(&format!("[check.interactive]\n{field}"));
        fixture.refuses("check.interactive");
    }
}

#[test]
fn answer_policy_refuses_before_admission() {
    let fixture = Fixture::new();
    for answer in ["none", "file"] {
        fixture.problem(&format!("[check]\nanswer = '{answer}'\n"));
        fixture.refuses("check.answer");
    }
}

#[test]
fn scoring_policy_refuses_before_admission() {
    let fixture = Fixture::new();
    for scoring in [
        "'pass_fail'",
        "{ points = true, scale = 100, aggregation = 'sum' }",
        "{ points = true, scale = 100, aggregation = 'min' }",
        "{}",
    ] {
        fixture.problem(&format!("[check]\nscoring = {scoring}\n"));
        fixture.refuses("check.scoring");
    }
}

#[test]
fn inspection_and_commands_without_dependencies_remain_available() {
    let fixture = Fixture::new();
    fixture.problem("[check]\nkind = 'checker'\nchecker = { path = 'checker' }\n");
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
        "[check]\nnotes = 'preserved'\n",
        "[check]\nkind = 'compare'\nnotes = 'preserved'\n",
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
