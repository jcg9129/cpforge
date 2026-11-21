//! Reserved file-I/O declarations refuse before any workspace admission.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const UNSUPPORTED: &str = "file-I/O problems are not supported in this version";

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

    fn run(&self, args: &[&str], status: i32) -> Output {
        let sequence = fs::read(self.path(".cpforge/job-seq")).unwrap();
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
fn job_commands_refuse_file_io_with_any_cwd_before_admission() {
    let fixture = Fixture::new();
    for declaration in [
        "io = { input = 'task.in' }\n",
        "io = { output = 'task.out' }\n",
        "io = { input = 'task.in', output = 'task.out' }\n",
    ] {
        for cwd in ["", "[config.run]\ncwd = { selector = 'workspace' }\n"] {
            fixture.problem(&format!("{declaration}{cwd}"));
            for command in ["test", "build", "run"] {
                // Report §5.2: configuration errors (7) precede the unsupported
                // execution platform (10), including on Windows.
                let output = fixture.run(&[command], 7);
                fixture.diagnostic(&output, "config.io", UNSUPPORTED);
                assert!(!String::from_utf8_lossy(&output.stderr).contains("unsupported platform"));
            }
        }
    }
    // Even another unsupported target property cannot hide the loader error.
    fixture.write(
        "problems/A/problem.toml",
        "[problem]\nid = 'local:A'\nkind = 'future-kind'\n[config]\nlanguage = 'cpp'\nio = { input = 'task.in' }\n",
    );
    fixture.diagnostic(&fixture.run(&["test"], 7), "config.io", UNSUPPORTED);
    fixture.problem("io = { input = 'task.in' }\n");
    fixture.write("cpforge.toml", "[commands.inspect]\nargv = ['unused']\n");
    let output = fixture.run(&["cmd", "inspect"], 7);
    fixture.diagnostic(&output, "config.io", UNSUPPORTED);
    for command in ["test", "build"] {
        let output = fixture.run(&[command, "--json"], 7);
        let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(doc["outcome"], "refused");
        assert_eq!(doc["refusal"]["class"], "config");
    }
}

#[test]
fn inspection_reports_file_io_like_other_phase_two_errors() {
    let fixture = Fixture::new();
    fixture.problem("io = { input = 'task.in', output = 'task.out' }\n");
    for args in [
        vec!["config", "check"],
        vec!["config", "check", "--tools"],
        vec!["config", "show"],
        vec!["config", "show", "--origin"],
        vec!["doctor"],
    ] {
        let output = fixture.run(&args, 7);
        fixture.diagnostic(&output, "config.io", UNSUPPORTED);
    }
    let output = fixture.run(&["config", "show", "--json"], 7);
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(doc["document"], "error");
    assert_eq!(doc["class"], "config");
    assert_eq!(doc["exit"], 7);
    assert_eq!(doc["diagnostics"][0]["key"], "config.io");
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
        assert!(!stderr.contains(UNSUPPORTED), "{stderr}");
        fixture.write(file, "schema = 1\n");
    }
}

#[cfg(windows)]
#[test]
fn valid_config_on_windows_still_refuses_execution_before_admission() {
    let fixture = Fixture::new();
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
        assert!(!String::from_utf8_lossy(&output.stderr).contains(UNSUPPORTED));
    }
}
