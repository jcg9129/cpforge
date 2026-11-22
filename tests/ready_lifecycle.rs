#![cfg(feature = "storage-test-hooks")]
//! Independent library drivers with pipe barriers, ahead of CLI integration.
#[allow(dead_code)] // The shared fixture also serves execution tests.
mod ready_support;

use cpforge::jobs::{CaptureCheck, prepare_build_job};
use cpforge::storage::job::{ProbeOutcome, probe};
use cpforge::storage::prune::PrunePolicy;
use ready_support::{Fixture, Tools, released};
use sha2::{Digest, Sha256};
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
        if matches!(mode, "capture" | "freeze") {
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "driver exit timed out"
            );
            std::thread::yield_now();
        }
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn freezing_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("problem/main.cpp", &"old first\n".repeat(8192));
    f.write("problem/main.py", &"old second\n".repeat(8192));
    f.write("problem/tests/one.in", &"old input\n".repeat(8192));
    f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'one'\ninput = 'tests/one.in'\norigin = 'custom'\nenabled = true\n");
    f.target("cpp", "sources = ['main.cpp', 'main.py']\n");
    f
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn edits_during_freezing_capture_whole_files_and_publish_only_a_complete_manifest() {
    for replace in [false, true] {
        let f = freezing_fixture();
        let mut driver = Driver::start(&f, "freeze");
        let dir = f.path(".cpforge/runs").join(driver.message("REGISTERED "));
        driver.message("CPF-PAUSED inputs.source_copied");
        assert!(matches!(probe(&dir), ProbeOutcome::Held));
        assert!(!dir.join("inputs/src/main.py").exists());
        assert!(!dir.join("inputs/capture.json").exists());
        assert!(!dir.join("builds").exists());
        let after = "new second with different length\n".repeat(8192);
        let after_input = "new input\n".repeat(8192);
        // Test data has already been read; source handles have been opened but
        // only the first source has been copied. Exercise both editor styles.
        if replace {
            f.write("problem/replacement", &after);
            fs::rename(f.path("problem/replacement"), f.path("problem/main.py")).unwrap();
            f.write("problem/tests/replacement", &after_input);
            fs::rename(
                f.path("problem/tests/replacement"),
                f.path("problem/tests/one.in"),
            )
            .unwrap();
        } else {
            f.write("problem/main.py", &after);
            f.write("problem/tests/one.in", &after_input);
        }
        driver.resume();
        driver.message("READY");
        assert_eq!(
            fs::read(dir.join("inputs/src/main.cpp")).unwrap(),
            "old first\n".repeat(8192).as_bytes()
        );
        let second = fs::read(dir.join("inputs/src/main.py")).unwrap();
        let before = "old second\n".repeat(8192);
        assert!(
            second == before.as_bytes() || second == after.as_bytes(),
            "mixed source copy"
        );
        assert_eq!(
            second,
            if replace {
                before.as_bytes()
            } else {
                after.as_bytes()
            }
        );
        assert_eq!(
            fs::read(dir.join("inputs/one.in")).unwrap(),
            "old input\n".repeat(8192).as_bytes()
        );
        let bytes = fs::read(dir.join("inputs/capture.json")).unwrap();
        let manifest = cpforge::report::wire::read::read_json(&bytes).unwrap();
        assert_eq!(manifest["document"], "capture_manifest");
        assert_eq!(
            manifest["sources"],
            serde_json::json!([{ "utf8": "main.cpp" }, { "utf8": "main.py" }])
        );
        driver.resume();
        assert!(driver.wait().success());
        // Dropping the ready capability cannot alter the published manifest.
        assert_eq!(fs::read(dir.join("inputs/capture.json")).unwrap(), bytes);
        released(&dir);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn interruption_during_freezing_leaves_no_manifest_or_ready_job() {
    let f = freezing_fixture();
    let mut driver = Driver::start(&f, "freeze");
    let dir = f.path(".cpforge/runs").join(driver.message("REGISTERED "));
    driver.message("CPF-PAUSED inputs.source_copied");
    f.write("problem/main.py", "edited second");
    f.write("problem/tests/one.in", "edited test");
    // Kill at exactly the same barrier used by the edit test, before the
    // consuming prepare_job call can expose a ready capability to any run.
    driver.child.kill().unwrap();
    assert!(!driver.wait().success());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match driver
            .messages
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        {
            Ok(line) => assert!(!line.contains("READY"), "interrupted freeze became ready"),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("driver output did not close"),
        }
    }
    assert!(!dir.join("inputs/capture.json").exists());
    assert!(!dir.join("inputs/src/main.py").exists());
    assert!(!dir.join("builds").exists());
    assert!(!dir.join("r00000").exists());
    assert_eq!(
        fs::read(dir.join("inputs/src/main.cpp")).unwrap(),
        "old first\n".repeat(8192).as_bytes()
    );
    assert_eq!(
        fs::read(dir.join("inputs/one.in")).unwrap(),
        "old input\n".repeat(8192).as_bytes()
    );
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
    if std::env::var("CPF_LIFECYCLE_MODE").unwrap() == "freeze" {
        let frozen = job
            .inputs()
            .freeze(
                &f.store(),
                &[cpforge::names::Identifier::new("one").unwrap()],
                &cpforge::storage::inputs::CapturedSource::from_target(&target),
                &f.path("problem"),
            )
            .unwrap();
        let ready = cpforge::jobs::prepare_job(job, frozen, &target, f.inputs(&Tools)).unwrap();
        assert_eq!(ready.capture_check, CaptureCheck::Checked);
        assert_eq!(ready.sources.files().len(), 2);
        assert_eq!(ready.manifest.entries().len(), 1);
        let input = &ready.manifest.entries()[0].input;
        let bytes = fs::read(&input.path).unwrap();
        assert_eq!(bytes, "old input\n".repeat(8192).as_bytes());
        assert_eq!(input.len, bytes.len() as u64);
        assert_eq!(input.sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
        signal("READY");
        barrier();
        drop(ready);
        return;
    }
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
