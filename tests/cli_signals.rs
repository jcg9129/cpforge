//! Real console signals at each job-command and lifecycle boundary.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use cpf_fixture::harness::{Identity, assert_all_gone, fixture_path};
use serde_json::Value;
use std::fs;
#[cfg(feature = "storage-test-hooks")]
use std::io::Write;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    Build,
    Test,
    Run,
    Cmd,
    Repeat,
    Each,
    AfterTest,
    Switch,
}
const JOBS: [Job; 8] = [
    Job::Build,
    Job::Test,
    Job::Run,
    Job::Cmd,
    Job::Repeat,
    Job::Each,
    Job::AfterTest,
    Job::Switch,
];

struct Workspace(PathBuf);
impl Workspace {
    fn new(job: Job, live: bool) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "cli-signals-{}",
                cpforge::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&path).unwrap();
        let w = Self(cpforge::storage::paths::plain(
            &fs::canonicalize(path).unwrap(),
        ));
        let fixture = fixture_path().to_string_lossy().into_owned();
        // A root, its child, and its grandchild each publish an identity before
        // a bounded workload. The fixture lifetime also bounds descendants if
        // cpf itself crashes. Readiness never depends on that duration.
        let root = format!("identity-file:{}", w.path("root").display());
        let descendants = format!(
            "spawn:1:identity-file:{},spawn:1:identity-file:{};sleep:15000,sleep:15000",
            w.path("child").display(),
            w.path("grandchild").display()
        );
        let argv = if live {
            serde_json::to_string(&[fixture.as_str(), &root, &descendants, "sleep:15000"]).unwrap()
        } else {
            serde_json::to_string(&[fixture.as_str(), "exit:0"]).unwrap()
        };
        let success = serde_json::to_string(&[fixture.as_str(), "exit:0"]).unwrap();
        let primary = if matches!(job, Job::AfterTest | Job::Switch) {
            &success
        } else {
            &argv
        };
        let language = if job == Job::Build {
            // Override the built-in compiler; no toolchain is needed to reach
            // the build cancellation path, and its declared outputs stay intact.
            format!("[languages.cpp.build]\nargv = {primary}\n")
        } else {
            format!(
                "[languages.fixture]\nextensions = ['sh']\nsources = ['main.sh']\nbuild = false\n[languages.fixture.run]\nargv = {primary}\n"
            )
        };
        let repeat = if matches!(job, Job::Repeat | Job::Each) {
            3
        } else {
            1
        };
        let each = job == Job::Each;
        let hooks = match job {
            Job::AfterTest => "[hooks]\nafter_test = ['h', 'h']\n",
            Job::Switch => "[hooks]\non_switch = ['h', 'h']\n",
            _ => "",
        };
        w.write("cpforge.toml", &format!(
            "schema = 1\n[run]\ndeadline_s = 15\n[build]\ndeadline_s = 15\n{language}\n[commands.x]\nargv = {argv}\ndeadline_s = 15\nrepeat = {repeat}\nfor_each_test = {each}\n[commands.h]\nargv = {argv}\ndeadline_s = 15\n{hooks}"
        ));
        w.write(
            ".cpforge/job-seq",
            "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
        );
        w.write(
            "user/cpforge/config.toml",
            &format!(
                "schema = 1\n[hooks]\ntrusted_workspaces = [{}]\n",
                serde_json::to_string(w.0.to_str().unwrap()).unwrap()
            ),
        );
        let language = if job == Job::Build { "cpp" } else { "fixture" };
        w.write(
            "problems/A/problem.toml",
            &format!("schema = 1\n[problem]\nid = 'local:A'\n[config]\nlanguage = '{language}'\n"),
        );
        w.write("problems/A/main.cpp", "int main() {}\n");
        w.write("problems/A/main.sh", "fixture input\n");
        let mut catalog = String::from("schema = 1\n");
        for id in ["a", "b"] {
            w.write(&format!("problems/A/tests/{id}.in"), "");
            w.write(&format!("problems/A/tests/{id}.ans"), "");
            catalog.push_str(&format!("[[test]]\nid = '{id}'\ninput = 'tests/{id}.in'\nanswer = 'tests/{id}.ans'\norigin = 'custom'\nenabled = true\n"));
        }
        w.write("problems/A/tests.toml", &catalog);
        w
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn write(&self, name: &str, text: &str) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn command(&self, job: Job, json: bool) -> Command {
        let mut c = self.raw_command();
        // `switch` takes its problem as an argument and rejects `--problem`.
        if job != Job::Switch {
            c.args(["--problem", "A"]);
        }
        match job {
            Job::Build => {
                c.arg("build");
            }
            Job::Test | Job::AfterTest => {
                c.arg("test");
            }
            Job::Run => {
                c.arg("run");
            }
            Job::Cmd | Job::Repeat | Job::Each => {
                c.args(["cmd", "x"]);
            }
            Job::Switch => {
                c.args(["switch", "A"]);
            }
        }
        if json && !matches!(job, Job::Run | Job::Switch) {
            c.arg("--json");
        }
        c
    }
    fn raw_command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_cpf"));
        c.current_dir(&self.0)
            .env("HOME", self.path("user"))
            .env("XDG_CONFIG_HOME", self.path("user"))
            .env_remove("CPF_HOOK_DEPTH")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_SIGNAL_READY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
    fn jobs(&self) -> Vec<PathBuf> {
        fs::read_dir(self.path(".cpforge/runs"))
            .into_iter()
            .flatten()
            .map(|e| e.unwrap().path())
            .collect()
    }
    fn report(&self) -> Value {
        let jobs = self.jobs();
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        serde_json::from_slice(&fs::read(jobs[0].join("report.json")).unwrap()).unwrap()
    }
    fn identities(&self, child: &mut Running) -> Vec<Identity> {
        ["root", "child", "grandchild"]
            .iter()
            .map(|name| {
                child.until(|| self.path(name).exists());
                Identity::from_file(&self.path(name)).unwrap()
            })
            .collect()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        // Panic/timeout cleanup, independent of the supervisor under test.
        for name in ["root", "child", "grandchild"] {
            if let Ok(identity) = Identity::from_file(&self.path(name)) {
                if identity.is_live() {
                    unsafe {
                        if name == "root" {
                            libc::kill(-(identity.pid as i32), libc::SIGKILL);
                        }
                        libc::kill(identity.pid as i32, libc::SIGKILL);
                    }
                }
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Readers start at spawn, not at wait: neither a report nor a barrier can fill a
// pipe and deadlock the harness. Every wait shares the child's hard deadline.
struct Running {
    child: Child,
    until: Instant,
    stdout: Receiver<Vec<u8>>,
    stderr: Receiver<Vec<u8>>,
    #[cfg(feature = "storage-test-hooks")]
    barriers: Receiver<String>,
}
impl Running {
    fn spawn(command: Command) -> Self {
        Self::start(command, false)
    }
    // The child is stored in the returned value and waited on by `wait`.
    #[allow(clippy::zombie_processes)]
    fn start(mut command: Command, broken_stdout: bool) -> Self {
        let mut child = command.spawn().unwrap();
        let (out_tx, stdout) = mpsc::channel();
        let (bar_tx, _barriers) = mpsc::channel();
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            if broken_stdout {
                drop(out);
                let _ = out_tx.send(Vec::new());
                return;
            }
            let mut all = Vec::new();
            let mut reader = BufReader::new(out);
            loop {
                let mut line = Vec::new();
                if reader.read_until(b'\n', &mut line).unwrap() == 0 {
                    break;
                }
                if let Some(name) = line.strip_prefix(b"CPF-PAUSED ") {
                    let _ = bar_tx.send(String::from_utf8_lossy(name).trim().to_owned());
                } else {
                    all.extend(line);
                }
            }
            let _ = out_tx.send(all);
        });
        let (err_tx, stderr) = mpsc::channel();
        let mut err = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            err.read_to_end(&mut bytes).unwrap();
            let _ = err_tx.send(bytes);
        });
        Self {
            child,
            until: Instant::now() + TIMEOUT,
            stdout,
            stderr,
            #[cfg(feature = "storage-test-hooks")]
            barriers: _barriers,
        }
    }
    fn remaining(&self) -> Duration {
        self.until.saturating_duration_since(Instant::now())
    }
    fn until(&mut self, mut ready: impl FnMut() -> bool) {
        while !ready() {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "cpf exited before readiness"
            );
            assert!(Instant::now() < self.until, "cpf readiness deadline");
            std::thread::yield_now();
        }
    }
    fn signal(&self, signal: i32) {
        // The child is still owned and unreaped at every call site.
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
    }
    #[cfg(feature = "storage-test-hooks")]
    fn barrier(&self, name: &str) {
        assert_eq!(self.barriers.recv_timeout(self.remaining()).unwrap(), name);
    }
    #[cfg(feature = "storage-test-hooks")]
    fn release(&mut self) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"xxxxxxxx")
            .unwrap();
    }
    fn wait(mut self) -> Output {
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < self.until, "cpf exit deadline");
            std::thread::yield_now();
        };
        Output {
            status,
            stdout: self.stdout.recv_timeout(self.remaining()).unwrap(),
            stderr: self.stderr.recv_timeout(self.remaining()).unwrap(),
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn assert_cancelled(w: &Workspace, job: Job, output: &Output, signal: i32, json: bool) {
    assert_eq!(output.status.signal(), Some(signal), "{job:?}: {output:?}");
    let report = w.report();
    let result = &report["result"];
    assert_eq!(
        result["verdict"],
        if job == Job::AfterTest {
            "passed"
        } else {
            "cancelled"
        }
    );
    assert_eq!(
        result["cancellation"]["source"],
        if signal == libc::SIGINT {
            "console"
        } else {
            "terminate"
        }
    );
    assert_eq!(
        result["cancellation"]["phase"],
        if job == Job::AfterTest {
            "hooks"
        } else {
            "primary"
        }
    );
    assert_eq!(
        result["pre_delivery_exit"]["reraise"],
        if signal == libc::SIGINT {
            "sigint"
        } else {
            "sigterm"
        }
    );
    let executions = report["executions"].as_array().unwrap();
    let active = if job == Job::AfterTest { 2 } else { 0 };
    assert_eq!(executions[active]["coverage"]["kind"], "started");
    assert_eq!(executions[active]["summary"]["class"]["kind"], "cancelled");
    for execution in executions.iter().skip(active + 1) {
        assert_eq!(execution["coverage"]["kind"], "skipped");
        assert_eq!(execution["coverage"]["reason"], "cancelled");
        assert!(execution["run"].is_null());
    }
    if matches!(job, Job::AfterTest | Job::Switch) {
        assert_eq!(report["hooks"][0]["outcome"]["kind"], "cancelled");
        assert_eq!(report["hooks"][1]["outcome"]["reason"], "cancelled");
    }
    if job == Job::AfterTest {
        assert_eq!(result["primary_verdict"], "passed");
        assert_eq!(report["fact"]["outcome"]["kind"], "published");
        assert_eq!(result["partial"], false);
        assert!(
            report["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["kind"] == "late_cancellation")
        );
    }
    if job == Job::Test {
        assert_eq!(report["tests"][0]["verdict"]["kind"], "cancelled");
        assert_eq!(report["tests"][1]["verdict"]["kind"], "cancelled");
        assert_eq!(report["fact"]["outcome"]["reason"], "not_completed");
    }
    if job == Job::Switch {
        assert_eq!(output.stdout, b"local:A\n");
        assert!(
            fs::read_to_string(w.path(".cpforge/state.toml"))
                .unwrap()
                .contains("local:A")
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("selection was committed"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("switch hooks: cancelled"));
    } else {
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(if job == Job::AfterTest {
                "OK"
            } else {
                "CANCELLED"
            }),
            "{output:?}"
        );
        if json && job != Job::Run {
            assert_eq!(output.stdout.last(), Some(&b'\n'));
            let delivered: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(delivered["result"], *result);
            assert_eq!(delivered["executions"], report["executions"]);
        } else {
            assert!(output.stdout.is_empty(), "{output:?}");
        }
    }
}

#[test]
fn signals_during_every_job_persist_and_deliver_before_reraise() {
    for job in JOBS {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            for json in [false, true] {
                if json && matches!(job, Job::Run | Job::Switch) {
                    continue;
                }
                let w = Workspace::new(job, true);
                let mut child = Running::spawn(w.command(job, json));
                let identities = w.identities(&mut child);
                child.signal(signal);
                let output = child.wait();
                assert_cancelled(&w, job, &output, signal, json);
                assert_all_gone(&identities, Duration::from_secs(2));
            }
        }
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn second_sigint_kills_every_live_job_tree_before_exiting() {
    for job in JOBS {
        let w = Workspace::new(job, true);
        let mut command = w.command(job, true);
        command.env("CPF_STORAGE_PAUSE_AT", "supervisor.before_cancel_kill");
        let mut child = Running::spawn(command);
        let identities = w.identities(&mut child);
        let job_dir = w.jobs().pop().unwrap();
        assert!(matches!(
            cpforge::storage::job::probe(&job_dir),
            cpforge::storage::job::ProbeOutcome::Held
        ));
        child.signal(libc::SIGINT);
        child.barrier("supervisor.before_cancel_kill");
        let first = Instant::now();
        // Time is used only to satisfy the contract's debounce threshold;
        // readiness and the live run are established by explicit barriers.
        while first.elapsed() < Duration::from_millis(120) {
            std::thread::yield_now();
        }
        assert!(identities.iter().all(Identity::is_live));
        child.signal(libc::SIGINT);
        let output = child.wait();
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGINT),
            "{job:?}: {output:?}"
        );
        assert_all_gone(&identities, Duration::from_secs(2));
        // Killing just cpf would release this lease too, so assert tree cleanup
        // independently above. No report is required on this path (E §8.2).
        let lease = cpforge::storage::job::probe(&job_dir);
        assert!(matches!(
            lease,
            cpforge::storage::job::ProbeOutcome::Acquired(_)
        ));
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn planning_signals_refuse_without_admitting_a_job() {
    for job in JOBS.into_iter().filter(|job| *job != Job::Switch) {
        for barrier in ["config.load", "selection.resolve"] {
            for signal in [libc::SIGINT, libc::SIGTERM] {
                let w = Workspace::new(job, true);
                let sequence = fs::read(w.path(".cpforge/job-seq")).unwrap();
                let mut command = w.command(job, true);
                command.env("CPF_STORAGE_PAUSE_AT", barrier);
                let mut child = Running::spawn(command);
                child.barrier(barrier);
                child.signal(signal);
                child.release();
                let output = child.wait();
                assert_eq!(output.status.signal(), Some(signal), "{job:?}: {output:?}");
                assert!(w.jobs().is_empty());
                assert_eq!(fs::read(w.path(".cpforge/job-seq")).unwrap(), sequence);
                if job == Job::Run {
                    assert!(output.stdout.is_empty());
                } else {
                    let refusal: Value = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(refusal["outcome"], "refused");
                    assert_eq!(refusal["refusal"]["class"], "cancelled");
                    assert!(refusal["job"].is_null());
                    assert_eq!(refusal["persistence"]["report"]["kind"], "not_attempted");
                    assert_eq!(refusal["result"]["cancellation"]["phase"], "primary");
                }
                assert!(!w.path("root").exists());
            }
        }
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn signals_between_repeat_and_test_iterations_skip_all_remaining_admissions() {
    for job in [Job::Repeat, Job::Each] {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            for keep_going in [false, true] {
                let w = Workspace::new(job, false);
                let config = fs::read_to_string(w.path("cpforge.toml")).unwrap().replace(
                    "[commands.x]",
                    &format!("[commands.x]\nkeep_going = {keep_going}"),
                );
                w.write("cpforge.toml", &config);
                let mut command = w.command(job, true);
                command.env("CPF_STORAGE_PAUSE_AT", "command.between_iterations");
                let mut child = Running::spawn(command);
                child.barrier("command.between_iterations");
                child.signal(signal);
                child.release();
                let output = child.wait();
                assert_eq!(output.status.signal(), Some(signal), "{output:?}");
                let report = w.report();
                let delivered: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(delivered["executions"], report["executions"]);
                assert_eq!(report["result"]["verdict"], "cancelled");
                assert_eq!(report["result"]["partial"], true);
                let executions = report["executions"].as_array().unwrap();
                assert_eq!(executions.len(), if job == Job::Each { 6 } else { 3 });
                assert_eq!(executions[0]["summary"]["class"]["kind"], "exited_zero");
                for execution in &executions[1..] {
                    assert_eq!(execution["coverage"]["reason"], "cancelled");
                    assert!(execution["run"].is_null());
                }
                if job == Job::Each {
                    assert_eq!(executions[0]["role"]["test"], "a");
                    assert_eq!(executions[3]["role"]["test"], "b");
                }
            }
        }
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn signals_after_delivery_leave_the_frozen_report_and_status_unchanged_unless_forced() {
    for job in JOBS {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            for force in [false, true] {
                // Raw run's stdin helper may still be blocked reading cpf's
                // stdin after the run, so it could consume the barrier's
                // release byte. The forced case needs no release; the orderly
                // case shares this code path with every other job command.
                if job == Job::Run && !force {
                    continue;
                }
                let w = Workspace::new(job, false);
                let mut command = w.command(job, true);
                command
                    .env("CPF_STORAGE_PAUSE_AT", "job.after_delivery")
                    .env("CPF_STORAGE_SIGNAL_READY", w.path("signal-observed"));
                let mut child = Running::spawn(command);
                child.barrier("job.after_delivery");
                let report = w.report();
                assert!(report["result"]["cancellation"].is_null());
                let before = fs::read(w.jobs()[0].join("report.json")).unwrap();
                child.signal(signal);
                child.until(|| w.path("signal-observed").exists());
                let first = Instant::now();
                if force {
                    while first.elapsed() < Duration::from_millis(120) {
                        std::thread::yield_now();
                    }
                    child.signal(libc::SIGINT);
                } else {
                    child.release();
                }
                let output = child.wait();
                if force {
                    // The forcing signal, rather than the first source, wins.
                    assert_eq!(output.status.signal(), Some(libc::SIGINT), "{output:?}");
                } else {
                    assert_eq!(
                        output.status.code().map(i64::from),
                        report["result"]["pre_delivery_exit"]["code"].as_i64(),
                        "{job:?}: {output:?}"
                    );
                }
                assert_eq!(fs::read(w.jobs()[0].join("report.json")).unwrap(), before);
                if !matches!(job, Job::Run | Job::Switch) {
                    let delivered: Value = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(delivered["result"], report["result"]);
                }
            }
        }
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn read_only_commands_and_switch_before_hooks_retain_default_signal_actions() {
    for args in [
        vec!["--problem", "A", "current"],
        vec!["config", "show"],
        vec!["switch", "A"],
    ] {
        let barrier = if args.contains(&"current") {
            "selection.resolve"
        } else {
            "config.load"
        };
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let w = Workspace::new(Job::Switch, false);
            let mut command = w.raw_command();
            command.args(&args).env("CPF_STORAGE_PAUSE_AT", barrier);
            let child = Running::spawn(command);
            child.barrier(barrier);
            child.signal(signal);
            let output = child.wait();
            assert_eq!(output.status.signal(), Some(signal), "{args:?}: {output:?}");
            assert!(w.jobs().is_empty());
            assert!(output.stdout.is_empty());
        }
    }
}

// Exercise guard restoration in one process: separate CLI invocations cannot
// detect a leaked process-wide handler. The test executable is isolated because
// its signal dispositions must not affect the other tests in this file.
#[test]
fn listener_restoration_helper() {
    if std::env::var_os("CPF_SIGNAL_RESTORE_HELPER").is_none() {
        return;
    }
    use cpforge::cancel_signals::CancelSignals;
    use cpforge::jobs::engine::{CancelSource, CancelState};
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
    }
    let first = CancelState::new();
    let guard = CancelSignals::install(first.clone()).unwrap();
    assert!(CancelSignals::install(CancelState::new()).is_err());
    assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
    let deadline = Instant::now() + TIMEOUT;
    while first.cancellation().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        first.cancellation().unwrap().source,
        CancelSource::Terminate
    );
    drop(guard);
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(signal, std::ptr::null(), &mut action) },
            0
        );
        assert_eq!(action.sa_sigaction, libc::SIG_DFL);
    }
    let second = CancelState::new();
    let guard = CancelSignals::install(second.clone()).unwrap();
    assert!(
        second.cancellation().is_none(),
        "previous command's signal leaked"
    );
    assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0);
    assert_eq!(second.cancellation().unwrap().source, CancelSource::Console);
    drop(guard);
    // Default SIGTERM must terminate this process after both guards were dropped.
    unsafe {
        libc::raise(libc::SIGTERM);
    }
    panic!("restored default SIGTERM did not terminate the process");
}

#[test]
fn listener_drop_restores_defaults_and_reinstallation_has_no_pending_intent() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "listener_restoration_helper", "--nocapture"])
        .env("CPF_SIGNAL_RESTORE_HELPER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = Running::spawn(command).wait();
    assert_eq!(output.status.signal(), Some(libc::SIGTERM), "{output:?}");
}

#[test]
fn delivery_failure_outranks_real_signal_cancellation_for_every_machine_job() {
    for job in JOBS
        .into_iter()
        .filter(|job| !matches!(job, Job::Run | Job::Switch))
    {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let w = Workspace::new(job, true);
            let mut child = Running::start(w.command(job, true), true);
            let identities = w.identities(&mut child);
            child.signal(signal);
            let output = child.wait();
            assert_eq!(output.status.code(), Some(11), "{job:?}: {output:?}");
            let report = w.report();
            // The status chosen at F: the re-raised signal's shell status.
            assert_eq!(
                report["result"]["pre_delivery_exit"]["code"],
                if signal == libc::SIGINT { 130 } else { 143 }
            );
            assert_eq!(
                report["result"]["cancellation"]["source"],
                if signal == libc::SIGINT {
                    "console"
                } else {
                    "terminate"
                }
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("cannot deliver machine output")
            );
            assert_all_gone(&identities, Duration::from_secs(2));
        }
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn real_signals_on_both_sides_of_fact_commit_keep_the_frozen_verdict() {
    for barrier in ["fact.cutoff", "fact.after_cutoff", "fact.after_replace"] {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let w = Workspace::new(Job::Test, false);
            let mut command = w.command(Job::Test, true);
            command.env("CPF_STORAGE_PAUSE_AT", barrier);
            let mut child = Running::spawn(command);
            child.barrier(barrier);
            child.signal(signal);
            child.release();
            let output = child.wait();
            assert_eq!(
                output.status.signal(),
                Some(signal),
                "{barrier}: {output:?}"
            );
            let report = w.report();
            let delivered: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(delivered["result"], report["result"]);
            assert_eq!(report["result"]["primary_verdict"], "passed");
            assert_eq!(report["result"]["verdict"], "passed");
            assert_eq!(report["result"]["cancellation"]["phase"], "fact");
            let published = barrier != "fact.cutoff";
            assert_eq!(
                report["fact"]["outcome"]["kind"],
                if published {
                    "published"
                } else {
                    "not_attempted"
                }
            );
            assert_eq!(w.path(".cpforge/facts/A/solution.toml").exists(), published);
        }
    }
}

#[test]
fn signals_in_later_iterations_preserve_completed_runs_and_skip_the_tail() {
    for job in [Job::Repeat, Job::Each] {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            let w = Workspace::new(job, true);
            let config = fs::read_to_string(w.path("cpforge.toml")).unwrap();
            let (head, commands) = config.split_once("[commands.x]\n").unwrap();
            let (command, tail) = commands.split_once("[commands.h]").unwrap();
            let (argv, settings) = command.split_once('\n').unwrap();
            let fixture: Vec<String> =
                serde_json::from_str(argv.strip_prefix("argv = ").unwrap()).unwrap();
            // The fixture is launched only on repeat 2, and on the second test
            // for for_each_test. All earlier iterations finish successfully.
            let mut argv = vec!["/bin/sh".to_owned(), "-c".into()];
            let script = if job == Job::Each {
                "if [ \"$1\" != 2 ]; then exit 0; fi; case \"$2\" in */b.in) ;; *) exit 0 ;; esac; shift; shift; exec \"$@\""
            } else {
                "if [ \"$1\" != 2 ]; then exit 0; fi; shift; exec \"$@\""
            };
            argv.extend([script.into(), "cpf-iteration".into(), "{iteration}".into()]);
            if job == Job::Each {
                argv.push("{test_input}".into());
            }
            argv.extend(fixture);
            w.write(
                "cpforge.toml",
                &format!(
                    "{head}[commands.x]\nargv = {}\n{settings}[commands.h]{tail}",
                    serde_json::to_string(&argv).unwrap()
                ),
            );
            let mut child = Running::spawn(w.command(job, true));
            let identities = w.identities(&mut child);
            child.signal(signal);
            let output = child.wait();
            assert_eq!(output.status.signal(), Some(signal), "{job:?}: {output:?}");
            let report = w.report();
            let delivered: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(delivered["executions"], report["executions"]);
            let active = if job == Job::Each { 4 } else { 1 };
            let executions = report["executions"].as_array().unwrap();
            for execution in &executions[..active] {
                assert_eq!(execution["summary"]["class"]["kind"], "exited_zero");
            }
            assert_eq!(executions[active]["role"]["iteration"], 2);
            assert_eq!(executions[active]["summary"]["class"]["kind"], "cancelled");
            for execution in &executions[active + 1..] {
                assert_eq!(execution["coverage"]["reason"], "cancelled");
                assert!(execution["run"].is_null());
            }
            assert_all_gone(&identities, Duration::from_secs(2));
        }
    }
}
