#![cfg(feature = "storage-test-hooks")]
//! Independent library drivers with pipe barriers, ahead of CLI integration.
#[allow(dead_code)] // The shared fixture also serves execution tests.
mod ready_support;

use cpforge::jobs::{CaptureCheck, prepare_build_job};
use cpforge::storage::job::{ProbeOutcome, probe};
use cpforge::storage::prune::PrunePolicy;
use ready_support::{Fixture, Tools, released};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::ManuallyDrop;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

struct Driver {
    child: Child,
    messages: Receiver<String>,
}
impl Driver {
    fn start(f: &Fixture, mode: &str) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "lifecycle_driver", "--nocapture"])
            .env("CPF_LIFECYCLE_ROOT", &f.0)
            .env("CPF_LIFECYCLE_MODE", mode)
            .env_remove("CPF_STORAGE_CRASH_AT")
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if mode == "capture" {
            command.env("CPF_STORAGE_PAUSE_AT", "inputs.source_copied");
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, messages }
    }
    fn message(&self, prefix: &str) -> String {
        loop {
            let line = self
                .messages
                .recv_timeout(Duration::from_secs(60))
                .expect("driver pipe barrier timed out");
            if let Some((_, tail)) = line.split_once(prefix) {
                return tail.into();
            }
        }
    }
    fn resume(&mut self) {
        self.child.stdin.as_mut().unwrap().write_all(b"x").unwrap();
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn competing_admission_keeps_capture_live_external_edit_isolated_and_killed_owner_reclaimed() {
    let f = Fixture::new();
    f.write("problem/main.cpp", "old cpp");
    f.write("problem/main.py", "old python");
    // Resolve before launching the drivers; neither races config publication.
    f.target("cpp", "sources = ['main.cpp', 'main.py']\n");
    let mut a = Driver::start(&f, "capture");
    let id = a.message("REGISTERED ");
    a.message("CPF-PAUSED inputs.source_copied");
    let dir = f.path(".cpforge/runs").join(&id);
    assert!(matches!(probe(&dir), ProbeOutcome::Held));
    assert_eq!(
        fs::read(dir.join("inputs").join("src").join("main.cpp")).unwrap(),
        b"old cpp"
    );
    assert!(!dir.join("inputs").join("src").join("main.py").exists());
    let mut b = Driver::start(&f, "admit");
    b.message("ADMITTED");
    assert!(b.child.wait().unwrap().success());
    assert!(dir.exists());
    assert!(matches!(probe(&dir), ProbeOutcome::Held));
    a.resume();
    a.message("READY");
    // The parent is an external writer, separate from both job processes.
    f.write("problem/main.cpp", "new cpp");
    f.write("problem/main.py", "new python");
    a.resume();
    a.message("ISOLATED");
    a.child.kill().unwrap();
    assert!(!a.child.wait().unwrap().success());
    released(&dir);
    f.prune();
    assert!(!dir.exists());
}

fn signal(value: &str) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}
fn barrier() {
    std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
}

#[test]
#[ignore = "subprocess library driver"]
fn lifecycle_driver() {
    let root = std::env::var_os("CPF_LIFECYCLE_ROOT").expect("driver workspace");
    // The parent owns fixture teardown; child B must not remove A's workspace.
    let f = ManuallyDrop::new(Fixture(root.into()));
    if std::env::var("CPF_LIFECYCLE_MODE").unwrap() == "admit" {
        let job = f.job(1 << 20, PrunePolicy::zero());
        assert!(job.prune_report.selection.errors.is_empty());
        assert!(job.prune_report.selection.failed_renames.is_empty());
        signal("ADMITTED");
        return;
    }
    let target = cpforge::config::load_config(&cpforge::config::ConfigRequest {
        environment: cpforge::config::Environment::default(),
        current_dir: f.0.clone(),
        config_path: Some(f.path("user.toml")),
        workspace_root: Some(f.0.clone()),
        problem_file: Some(f.path("problem/problem.toml")),
        ..cpforge::config::ConfigRequest::default()
    })
    .target()
    .unwrap();
    let job = f.job(1 << 20, PrunePolicy::default());
    signal(&format!("REGISTERED {}", job.id));
    let ready = prepare_build_job(job, &target, f.inputs(&Tools)).unwrap();
    assert_eq!(ready.capture_check, CaptureCheck::Checked);
    signal("READY");
    barrier();
    let copies = ready.sources.source_bindings();
    assert_eq!(fs::read(&copies[0]).unwrap(), b"old cpp");
    assert_eq!(fs::read(&copies[1]).unwrap(), b"old python");
    signal("ISOLATED");
    barrier();
    drop(ready);
}
