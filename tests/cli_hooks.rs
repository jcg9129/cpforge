//! Hook authorization, captured context and job lifecycle through the CLI.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Workspace(PathBuf);
impl Workspace {
    fn new(script: &str) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "cli-hooks-{}",
                cpforge::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&path).unwrap();
        let w = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(path).unwrap(),
        ));
        w.config("after_test = ['h']\non_switch = ['h']", "");
        w.write("hook.sh", script);
        w.write("user/cpforge/config.toml", "schema = 1\n");
        w.write(
            ".cpforge/job-seq",
            "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
        );
        for name in ["A", "B"] {
            w.write(
                &format!("problems/{name}/problem.toml"),
                &format!(
                    "schema = 1\n[problem]\nid = 'local:{name}'\n[config]\nlanguage = 'fixture'\n"
                ),
            );
            w.write(
                &format!("problems/{name}/main.sh"),
                &format!(
                    "printf 'test\\n' >> '{}/order'\nprintf '7\\n'\n",
                    w.0.display()
                ),
            );
            w.write(&format!("problems/{name}/tests/a.in"), "7\n");
            w.write(&format!("problems/{name}/tests/a.ans"), "7\n");
            w.write(&format!("problems/{name}/tests.toml"), "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n");
        }
        w
    }
    fn config(&self, events: &str, settings: &str) {
        self.write("cpforge.toml", &format!("schema = 1\n[languages.fixture]\nextensions = ['sh']\nsources = ['main.sh']\nbuild = false\n[languages.fixture.run]\nargv = ['/bin/sh', '{{source}}']\n[commands.h]\nargv = ['/bin/sh', '{{workspace}}/hook.sh', '{{problem_id}}', '{{workspace}}']\n{settings}\n[hooks]\n{events}\n"));
    }
    fn trust(&self, enabled: bool) {
        let root = serde_json::to_string(self.0.to_str().unwrap()).unwrap();
        self.write(
            "user/cpforge/config.toml",
            &format!("schema = 1\n[hooks]\ntrusted_workspaces = [{root}]\nenabled = {enabled}\n"),
        );
    }
    fn path(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cpf"));
        c.current_dir(&self.0)
            .args(args)
            .env("HOME", self.path("user"))
            .env("XDG_CONFIG_HOME", self.path("user"))
            .env_remove("CPF_HOOK_DEPTH")
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
    fn run(&self, args: &[&str]) -> Output {
        wait(self.command(args).spawn().unwrap())
    }
    fn test(&self) -> Output {
        self.run(&["--problem", "A", "test", "--json"])
    }
    fn reports(&self) -> Vec<Value> {
        let mut paths: Vec<_> = fs::read_dir(self.path(".cpforge/runs"))
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect();
        paths.sort();
        paths
            .into_iter()
            .map(|p| serde_json::from_slice(&fs::read(p.join("report.json")).unwrap()).unwrap())
            .collect()
    }
    fn await_file(&self, path: &str) {
        let start = Instant::now();
        while !self.path(path).exists() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "no marker {path}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wait(mut child: Child) -> Output {
    let read = |pipe: Option<_>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = Read::read_to_end(&mut pipe, &mut bytes);
            }
            bytes
        })
    };
    // stdout and stderr have different types; erase only the bounded test readers.
    let stdout = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("hook command timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}
fn document(output: &Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["outcome"], "finished", "{value}");
    value
}
fn warning(report: &Value, kind: &str) -> bool {
    report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["kind"] == kind)
}

#[test]
fn untrusted_disabled_override_and_depth_do_not_execute_hooks() {
    for reason in ["untrusted", "disabled", "config_override", "depth"] {
        let w = Workspace::new("echo unexpected > \"$2/hooked\"\n");
        if reason != "untrusted" {
            w.trust(reason != "disabled");
        }
        w.write("override.toml", "schema = 1\n");
        let mut command = w.command(&["--problem", "A", "test", "--json"]);
        if reason == "config_override" {
            command.args(["--config", "override.toml"]);
        }
        if reason == "depth" {
            command.env("CPF_HOOK_DEPTH", "malformed");
        }
        let report = document(&wait(command.spawn().unwrap()), 0);
        assert_eq!(report["hooks"][0]["outcome"]["kind"], "not_planned");
        assert_eq!(report["hooks"][0]["outcome"]["reason"], reason);
        let warnings: Vec<_> = report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["kind"] == "hooks_disabled")
            .collect();
        assert_eq!(warnings.len(), 1, "{report}");
        assert_eq!(warnings[0]["reason"], reason);
        assert_eq!(report["result"]["counts"]["hooks"]["planned"], 0);
        assert!(!w.path("hooked").exists());
    }
}

#[test]
fn trusted_hook_follows_tests_and_receives_injected_identity() {
    let w = Workspace::new(
        "test \"$1\" = \"$CPF_PROBLEM_ID\" || exit 11\ntest \"$2\" = \"$CPF_WORKSPACE\" || exit 12\ntest \"$CPF_HOOK_DEPTH\" = 1 || exit 13\ntest -n \"$CPF_JOB_ID\" || exit 14\ntest -d \"$CPF_PROBLEM_DIR\" || exit 15\nprintf 'hook:%s\\n' \"$1\" >> \"$2/order\"\n",
    );
    w.trust(true);
    let report = document(&w.test(), 0);
    assert_eq!(
        fs::read_to_string(w.path("order")).unwrap(),
        "test\nhook:local:A\n"
    );
    assert_eq!(report["hooks"][0]["outcome"]["kind"], "succeeded");
    assert_eq!(report["hooks"][0]["execution"], 1);
    assert_eq!(report["executions"][1]["stage"], "hook");
    assert_eq!(report["result"]["counts"]["hooks"]["started"], 1);
    let mut delivered = report;
    delivered.as_object_mut().unwrap().remove("persistence");
    assert_eq!(delivered, w.reports()[0]);
}

#[test]
fn ordinary_hook_failure_warns_and_preserves_test_exit_even_after_wrong_answer() {
    for wrong in [false, true] {
        let w = Workspace::new("exit 19\n");
        w.trust(true);
        if wrong {
            w.write("problems/A/tests/a.ans", "8\n");
        }
        let report = document(&w.test(), i32::from(wrong));
        assert_eq!(
            report["result"]["verdict"],
            if wrong { "wrong_answer" } else { "passed" }
        );
        assert_eq!(report["hooks"][0]["outcome"]["kind"], "failed");
        assert_eq!(report["hooks"][0]["outcome"]["class"], "nonzero_exit");
        assert!(warning(&report, "hook"));
    }
}

#[test]
fn switch_hooks_run_after_commit_and_only_identity_reaches_stdout() {
    let w = Workspace::new(
        "grep -q \"$1\" \"$2/.cpforge/state.toml\" || exit 22\necho hook-out\necho hook-err >&2\nexit 3\n",
    );
    w.trust(true);
    let output = w.run(&["switch", "B"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"local:B\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    for text in ["hook-out", "hook-err", "hook h: failed"] {
        assert!(stderr.contains(text), "{stderr}");
    }
    let report = &w.reports()[0];
    assert_eq!(report["command"], "hook");
    assert_eq!(report["result"]["verdict"], "nonzero_exit");
    assert_eq!(report["hooks"][0]["trigger"], "on_switch");
}

#[test]
fn switch_hook_admission_failure_warns_after_committing() {
    let w = Workspace::new("echo unexpected > \"$2/hooked\"\n");
    w.trust(true);
    w.write(".cpforge/job-seq", "malformed");
    let output = w.run(&["switch", "B"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"local:B\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("warning"));
    assert!(!w.path("hooked").exists());
    assert!(
        fs::read_to_string(w.path(".cpforge/state.toml"))
            .unwrap()
            .contains("local:B")
    );
}

#[test]
fn hook_invoking_switch_inherits_depth_and_stops_recursion() {
    let cpf = env!("CARGO_BIN_EXE_cpf");
    let quoted = format!("'{}'", cpf.replace('\'', "'\\''"));
    let w = Workspace::new(&format!("echo hook >> \"$2/hooked\"\n{quoted} switch B\n"));
    w.trust(true);
    let output = w.run(&["switch", "A"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"local:A\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("hooks_disabled: depth"));
    assert_eq!(fs::read_to_string(w.path("hooked")).unwrap(), "hook\n");
    assert_eq!(w.reports().len(), 1);
    assert!(
        fs::read_to_string(w.path(".cpforge/state.toml"))
            .unwrap()
            .contains("local:B")
    );
}

#[test]
fn concurrent_switch_does_not_retarget_either_trigger() {
    for switch in [false, true] {
        let w = Workspace::new(
            "echo started > \"$2/started\"\nwhile [ ! -f \"$2/release\" ]; do sleep 0.02; done\necho \"$1:$CPF_PROBLEM_ID\" > \"$2/captured\"\n",
        );
        w.trust(true);
        let args: &[&str] = if switch {
            &["switch", "A"]
        } else {
            &["--problem", "A", "test", "--json"]
        };
        let child = w.command(args).spawn().unwrap();
        w.await_file("started");
        let output = wait(
            w.command(&["switch", "B"])
                .env("CPF_HOOK_DEPTH", "1")
                .spawn()
                .unwrap(),
        );
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(output.stdout, b"local:B\n");
        w.write("release", "go");
        let output = wait(child);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(
            fs::read_to_string(w.path("captured")).unwrap(),
            "local:A:local:A\n"
        );
        assert_eq!(w.reports()[0]["problem"]["identity"], "local:A");
    }
}

#[test]
fn cancelled_primary_skips_hooks_and_cancelling_hook_skips_later_hooks() {
    use std::os::unix::process::ExitStatusExt;
    for during_hook in [false, true] {
        let w = Workspace::new("echo started > \"$2/started\"\nexec sleep 15\n");
        w.trust(true);
        if during_hook {
            w.config("after_test = ['h', 'h']", "");
        } else {
            w.write(
                "problems/A/main.sh",
                &format!(
                    "echo started > '{}/started'\nexec sleep 15\n",
                    w.0.display()
                ),
            );
        }
        let child = w
            .command(&["--problem", "A", "test", "--json"])
            .spawn()
            .unwrap();
        w.await_file("started");
        // SAFETY: signal only the child process owned by this fixture.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
            0
        );
        let output = wait(child);
        assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        if during_hook {
            assert_eq!(report["hooks"][0]["outcome"]["kind"], "cancelled");
            assert_eq!(report["hooks"][1]["outcome"]["reason"], "cancelled");
            assert_eq!(report["result"]["primary_verdict"], "passed");
        } else {
            assert_eq!(report["hooks"][0]["outcome"]["kind"], "skipped");
            assert_eq!(report["hooks"][0]["outcome"]["reason"], "job_not_eligible");
        }
    }
}

#[test]
fn deadlines_use_hook_default_or_the_commands_own_value() {
    for (settings, expected) in [("", 30_000), ("deadline_s = 0.04", 40)] {
        let w = Workspace::new(if settings.is_empty() {
            "exit 0\n"
        } else {
            "exec sleep 15\n"
        });
        w.trust(true);
        w.config("after_test = ['h']", settings);
        let report = document(&w.test(), 0);
        let run = report["executions"][1]["run"].as_str().unwrap();
        let job = report["job"]["id"].as_str().unwrap();
        let spec: Value = serde_json::from_slice(
            &fs::read(w.path(&format!(".cpforge/runs/{job}/{run}/spec.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            spec["wall_deadline_us"],
            (expected * 1000).to_string(),
            "{spec}"
        );
        if !settings.is_empty() {
            assert_eq!(report["hooks"][0]["outcome"]["class"], "wall_deadline");
            assert!(warning(&report, "hook"));
        }
    }
}

#[test]
fn read_only_commands_never_admit_hooks() {
    let w = Workspace::new("echo unexpected > \"$2/hooked\"\n");
    w.trust(true);
    for args in [
        vec!["--problem", "A", "current"],
        vec!["--problem", "A", "test", "ls"],
        vec!["--problem", "A", "config", "show"],
        vec!["--problem", "A", "config", "check", "--tools"],
        vec!["config", "paths"],
        vec!["--problem", "A", "doctor"],
    ] {
        let output = w.run(&args);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(!w.path("hooked").exists());
        assert!(!w.path(".cpforge/runs").exists());
    }
}

#[test]
fn canonical_trust_accepts_symlinks_and_ignores_unresolvable_entries() {
    let w = Workspace::new("echo ran >> \"$2/hooked\"\n");
    std::os::unix::fs::symlink(&w.0, w.path("alias")).unwrap();
    let alias = serde_json::to_string(w.path("alias").to_str().unwrap()).unwrap();
    w.write(
        "user/cpforge/config.toml",
        &format!("schema = 1\n[hooks]\ntrusted_workspaces = [{alias}]\n"),
    );
    let report = document(&w.test(), 0);
    assert_eq!(report["hooks"][0]["outcome"]["kind"], "succeeded");
    let missing = serde_json::to_string(w.path("missing").to_str().unwrap()).unwrap();
    w.write(
        "user/cpforge/config.toml",
        &format!("schema = 1\n[hooks]\ntrusted_workspaces = [{alias}, {missing}]\n"),
    );
    // An unresolvable entry trusts nothing but leaves the alias trusted.
    let report = document(&w.test(), 0);
    assert_eq!(report["hooks"][0]["outcome"]["kind"], "succeeded");
    assert_eq!(fs::read_to_string(w.path("hooked")).unwrap(), "ran\nran\n");
    // Only the unresolvable entry: untrusted.
    w.write(
        "user/cpforge/config.toml",
        &format!("schema = 1\n[hooks]\ntrusted_workspaces = [{missing}]\n"),
    );
    let report = document(&w.test(), 0);
    assert_eq!(report["hooks"][0]["outcome"]["reason"], "untrusted");
    assert_eq!(fs::read_to_string(w.path("hooked")).unwrap(), "ran\nran\n");
}

#[test]
fn ordered_hooks_continue_after_ordinary_failures_and_have_separate_runs() {
    let w = Workspace::new("echo hook >> \"$2/hooked\"\nexit 23\n");
    w.trust(true);
    w.config(
        "after_test = ['h', 'h', 'h']\non_switch = ['h', 'h', 'h']",
        "",
    );
    let report = document(&w.test(), 0);
    assert_eq!(report["hooks"].as_array().unwrap().len(), 3);
    for (index, hook) in report["hooks"].as_array().unwrap().iter().enumerate() {
        assert_eq!(hook["execution"], index + 1);
        assert_eq!(hook["outcome"]["kind"], "failed");
    }
    assert_eq!(
        fs::read_to_string(w.path("hooked")).unwrap(),
        "hook\nhook\nhook\n"
    );
    let output = w.run(&["switch", "A"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"local:A\n");
    assert_eq!(
        fs::read_to_string(w.path("hooked")).unwrap(),
        "hook\nhook\nhook\nhook\nhook\nhook\n"
    );
}

#[test]
fn cancelling_switch_hook_preserves_identity_and_committed_selection() {
    use std::os::unix::process::ExitStatusExt;
    let w = Workspace::new("echo started > \"$2/started\"\nexec sleep 15\n");
    w.trust(true);
    let child = w.command(&["switch", "A"]).spawn().unwrap();
    w.await_file("started");
    // SAFETY: signal only the child process owned by this fixture.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let output = wait(child);
    assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
    assert_eq!(output.stdout, b"local:A\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("selection was committed"));
    assert!(
        fs::read_to_string(w.path(".cpforge/state.toml"))
            .unwrap()
            .contains("local:A")
    );
    let report = &w.reports()[0];
    assert_eq!(report["command"], "hook");
    assert_eq!(report["result"]["verdict"], "cancelled");
}
