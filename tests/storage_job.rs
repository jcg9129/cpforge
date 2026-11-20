//! Portable job metadata and lease tests, independent of execution supervisors.

mod storage_support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use cpforge::storage::job::{
    Epoch, JobLease, JobRecord, ProbeLock, ProbeOutcome, classify_job_record, probe,
    publish_job_record, read_job_record,
};
use cpforge::storage::{ErrorKind, MalformedRecord, Record};
use storage_support::{Directory, assert_no_temps};

const EPOCH: &str = "0123456789abcdef0123456789abcdef";

fn record(n: u64) -> JobRecord {
    JobRecord {
        epoch: Epoch::parse(EPOCH).unwrap(),
        n,
    }
}

/// A released lease can stay `Held` briefly: on Unix a concurrent test's
/// spawn shares open file descriptions between fork and exec, and Windows
/// releases a terminated process's locks asynchronously. Only `Held` is
/// retried, within a bound; every other outcome fails at once.
fn acquired(path: &Path) -> ProbeLock {
    let start = std::time::Instant::now();
    loop {
        match probe(path) {
            ProbeOutcome::Acquired(lock) => return lock,
            ProbeOutcome::Held if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            outcome => panic!("expected acquired probe, got {outcome:?}"),
        }
    }
}

#[test]
fn record_encoding_is_exact_and_round_trips_full_u64_range() {
    assert_eq!(
        record(42).encode(),
        b"schema = 1\nepoch = \"0123456789abcdef0123456789abcdef\"\nn = \"42\"\n"
    );
    for n in [
        0,
        9_223_372_036_854_775_807,
        9_223_372_036_854_775_808,
        18_446_744_073_709_551_614,
        u64::MAX,
    ] {
        let record = record(n);
        let bytes = record.encode();
        assert_eq!(
            bytes,
            format!("schema = 1\nepoch = \"{EPOCH}\"\nn = \"{n}\"\n").as_bytes()
        );
        assert!(matches!(classify_job_record(&bytes), Record::Valid(value) if value == record));
    }
}

#[test]
fn supported_records_reject_missing_invalid_and_unknown_fields() {
    for fields in [
        "n = '0'".to_owned(),
        format!("epoch = '{EPOCH}'"),
        "epoch = 1\nn = '0'".to_owned(),
        "epoch = true\nn = '0'".to_owned(),
        "epoch = []\nn = '0'".to_owned(),
        format!("epoch = '{EPOCH}'\nn = '0'\nunknown = 1"),
        format!("epoch = '{EPOCH}'\nn = '0'\n[extra]"),
    ] {
        let text = format!("schema = 1\n{fields}\n");
        assert!(
            matches!(
                classify_job_record(text.as_bytes()),
                Record::Malformed(MalformedRecord::Fields(_))
            ),
            "{text}"
        );
    }
    for epoch in [
        "",
        "0123",
        "0123456789abcdef0123456789abcdef0",
        "g123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdeF",
        "0123456789abcdef0123456789abcdeｆ",
    ] {
        let text = format!("schema = 1\nepoch = '{epoch}'\nn = '0'\n");
        assert!(
            matches!(
                classify_job_record(text.as_bytes()),
                Record::Malformed(MalformedRecord::Fields(_))
            ),
            "{epoch}"
        );
    }
    for n in [
        "5",
        "'01'",
        "'18446744073709551616'",
        "'-1'",
        "'+1'",
        "''",
        "'00'",
        "' 1'",
        "'1 '",
        "'١'",
        "'１'",
        "'1_000'",
        "true",
        "1.0",
        "[]",
    ] {
        let text = format!("schema = 1\nepoch = '{EPOCH}'\nn = {n}\n");
        assert!(
            matches!(
                classify_job_record(text.as_bytes()),
                Record::Malformed(MalformedRecord::Fields(_))
            ),
            "{n}"
        );
    }
}

#[test]
fn schema_classification_precedes_all_field_decoding() {
    assert!(matches!(
        classify_job_record(b"epoch = 'bad'\nn = 5"),
        Record::Malformed(MalformedRecord::SchemaMissing)
    ));
    for schema in ["'1'", "true", "1.0", "[]", "{}"] {
        let text = format!("schema = {schema}\n");
        assert!(matches!(
            classify_job_record(text.as_bytes()),
            Record::Malformed(MalformedRecord::SchemaMalformed)
        ));
    }
    for schema in [0, -1] {
        let text = format!("schema = {schema}\n");
        assert!(matches!(
            classify_job_record(text.as_bytes()),
            Record::Malformed(MalformedRecord::SchemaTooOld { .. })
        ));
    }
    for text in [
        "schema = 2",
        "schema = 2\nepoch = 'bad'\nn = 5\nunknown = true",
    ] {
        assert!(matches!(
            classify_job_record(text.as_bytes()),
            Record::TooNew {
                schema: 2,
                supported: 1
            }
        ));
    }
    assert!(matches!(
        classify_job_record(b"schema = 2\nbad = ["),
        Record::Malformed(MalformedRecord::Toml(_))
    ));
    assert!(matches!(
        classify_job_record(b"\xff"),
        Record::Malformed(MalformedRecord::Encoding(_))
    ));
}

#[test]
fn record_reads_distinguish_missing_unreadable_and_oversized() {
    let dir = Directory::new();
    let path = dir.path("job.toml");
    assert!(matches!(read_job_record(&path), Record::Missing));
    fs::create_dir(&path).unwrap();
    assert!(matches!(read_job_record(&path), Record::Unreadable(_)));
    fs::remove_dir(&path).unwrap();
    let bytes = vec![b' '; 1024 * 1024 + 1];
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        read_job_record(&path),
        Record::Malformed(MalformedRecord::TooLarge { limit: 1_048_576 })
    ));
    assert!(matches!(
        classify_job_record(&bytes),
        Record::Malformed(MalformedRecord::TooLarge { limit: 1_048_576 })
    ));
    fs::write(&path, record(u64::MAX).encode()).unwrap();
    assert!(matches!(read_job_record(&path), Record::Valid(value) if value == record(u64::MAX)));
}

#[test]
fn record_publication_keeps_existing_metadata_and_primitive_outcomes() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("job metadata").unwrap();
    let job = guard.create_dir_all(".cpforge/runs/job").unwrap();
    let published = publish_job_record(&guard, &job, &record(0)).unwrap();
    assert!(published.committed);
    assert_eq!(published.cleanup, cpforge::storage::Cleanup::Done);
    let error = publish_job_record(&guard, &job, &record(1)).unwrap_err();
    assert!(matches!(error.kind, ErrorKind::Exists { .. }));
    assert!(!error.committed());
    assert_eq!(fs::read(job.join("job.toml")).unwrap(), record(0).encode());
    assert_no_temps(&job);
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn record_publication_retains_committed_durability_warnings() {
    use cpforge::storage::{Durability, LockOptions, TestHooks};
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock
        .acquire_with_options(
            "job metadata",
            LockOptions {
                test_hooks: TestHooks {
                    post_commit_sync_failure: true,
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let job = guard.create_dir_all(".cpforge/runs/job").unwrap();
    let published = publish_job_record(&guard, &job, &record(0)).unwrap();
    assert!(published.committed);
    assert!(matches!(published.durability, Durability::Uncertain(_)));
    assert_eq!(fs::read(job.join("job.toml")).unwrap(), record(0).encode());
}

#[test]
fn owner_clones_keep_one_lock_until_the_last_drop() {
    let dir = Directory::new();
    assert!(matches!(probe(&dir.0), ProbeOutcome::Absent));
    let owner = JobLease::create_and_lock(&dir.0).unwrap();
    assert!(matches!(probe(&dir.0), ProbeOutcome::Held));
    assert_eq!(
        JobLease::create_and_lock(&dir.0).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    let clone = owner.clone();
    drop(owner);
    assert!(matches!(probe(&dir.0), ProbeOutcome::Held));
    drop(clone);
    let lock = acquired(&dir.0);
    assert!(matches!(probe(&dir.0), ProbeOutcome::Held));
    drop(lock);
    drop(acquired(&dir.0));
    assert!(dir.path("lease").is_file());
}

#[test]
fn existing_unlocked_lease_cannot_be_recreated_and_nonfiles_are_failed_probes() {
    let dir = Directory::new();
    fs::write(dir.path("lease"), b"").unwrap();
    assert_eq!(
        JobLease::create_and_lock(&dir.0).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    drop(acquired(&dir.0));
    fs::remove_file(dir.path("lease")).unwrap();
    fs::create_dir(dir.path("lease")).unwrap();
    assert!(matches!(probe(&dir.0), ProbeOutcome::Failed(_)));
}

#[test]
fn independent_process_probes_observe_owner_and_retain_acquired_lock() {
    let dir = Directory::new();
    let owner = JobLease::create_and_lock(&dir.0).unwrap();
    Helper::start(&dir.0, "probe-held").finish();
    drop(owner);
    let child = Helper::start(&dir.0, "probe-acquired");
    assert!(matches!(probe(&dir.0), ProbeOutcome::Held));
    child.finish();
    drop(acquired(&dir.0));
}

#[test]
fn killed_owner_becomes_acquirable_without_age_or_pid_checks() {
    let dir = Directory::new();
    let mut owner = Helper::start(&dir.0, "owner");
    assert!(matches!(probe(&dir.0), ProbeOutcome::Held));
    owner.child.kill().unwrap();
    assert!(!owner.child.wait().unwrap().success());
    drop(acquired(&dir.0));
}

#[test]
fn spawned_child_does_not_retain_owner_or_probe_leases() {
    let dir = Directory::new();
    let owner = JobLease::create_and_lock(&dir.0).unwrap();
    let mut child = Helper::start(&dir.0, "idle");
    drop(owner);
    let lock = acquired(&dir.0);
    assert!(child.child.try_wait().unwrap().is_none());
    drop(lock);
    child.finish();

    let lock = acquired(&dir.0);
    let mut child = Helper::start(&dir.0, "idle");
    drop(lock);
    let next = acquired(&dir.0);
    assert!(child.child.try_wait().unwrap().is_none());
    drop(next);
    child.finish();
}

// Unix only: Windows cannot rename a directory with a file open inside it
// (covered by `windows_rename_needs_every_handle_in_the_job_directory_closed`).
#[cfg(unix)]
#[test]
fn directory_rename_keeps_owner_and_acquired_probe_protection() {
    let dir = Directory::new();
    let original = dir.path("job");
    fs::create_dir(&original).unwrap();
    let owner = JobLease::create_and_lock(&original).unwrap();
    let renamed = dir.path("renamed");
    fs::rename(&original, &renamed).unwrap();
    assert!(matches!(probe(&renamed), ProbeOutcome::Held));
    drop(owner);
    let lock = acquired(&renamed);
    let reclaimed = dir.path(".reclaim-job");
    fs::rename(&renamed, &reclaimed).unwrap();
    assert!(matches!(probe(&reclaimed), ProbeOutcome::Held));
    drop(lock);
    drop(acquired(&reclaimed));
}

#[cfg(unix)]
#[test]
fn unreadable_metadata_and_lease_are_errors_without_proving_inactivity() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no arguments and only reads this process's identity.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = Directory::new();
    fs::write(dir.path("job.toml"), record(0).encode()).unwrap();
    fs::write(dir.path("lease"), b"").unwrap();
    for name in ["job.toml", "lease"] {
        fs::set_permissions(dir.path(name), fs::Permissions::from_mode(0o000)).unwrap();
    }
    let metadata = read_job_record(dir.path("job.toml"));
    let lease = probe(&dir.0);
    for name in ["job.toml", "lease"] {
        fs::set_permissions(dir.path(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(
        matches!(metadata, Record::Unreadable(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert!(
        matches!(lease, ProbeOutcome::Failed(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
}

fn helper_command(path: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "lease_child", "--nocapture"])
        .env("CPF_JOB_LEASE_DIR", path)
        .env("CPF_JOB_LEASE_MODE", mode);
    command
}

/// Pipe barriers keep the child alive until the assertion or kill; startup is
/// bounded and teardown kills/reaps a helper even when a parent assertion fails.
#[derive(Debug)]
struct Helper {
    child: Child,
    messages: Receiver<String>,
}

impl Helper {
    fn start(path: &Path, mode: &str) -> Self {
        let mut child = helper_command(path, mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        let _reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let helper = Self { child, messages };
        loop {
            let line = helper
                .messages
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
            if line.contains("CPF-LEASE-READY") {
                break;
            }
        }
        helper
    }

    fn finish(mut self) {
        self.child.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "subprocess helper"]
fn lease_child() {
    let path = std::env::var_os("CPF_JOB_LEASE_DIR").expect("helper job directory");
    let path = Path::new(&path);
    let mode = std::env::var("CPF_JOB_LEASE_MODE").unwrap();
    let mut owner = None;
    let mut probe_lock = None;
    match mode.as_str() {
        "owner" => owner = Some(JobLease::create_and_lock(path).unwrap()),
        "probe-held" => assert!(matches!(probe(path), ProbeOutcome::Held)),
        "probe-acquired" => probe_lock = Some(acquired(path)),
        "idle" => {}
        #[cfg(feature = "storage-test-hooks")]
        "publish" => {
            suppress_crash_artifacts();
            let lock = cpforge::storage::WorkspaceLock::new(path).unwrap();
            let guard = lock.acquire("job metadata crash").unwrap();
            let job = guard.create_dir_all(".cpforge/runs/job").unwrap();
            publish_job_record(&guard, job, &record(0)).unwrap();
            panic!("expected crash at publication boundary");
        }
        _ => panic!("unknown helper mode: {mode}"),
    }
    println!("CPF-LEASE-READY");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
    drop(probe_lock);
    drop(owner);
}

#[cfg(feature = "storage-test-hooks")]
fn suppress_crash_artifacts() {
    #[cfg(unix)]
    {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: limit is valid; only this helper process's core limit changes.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
    }
    #[cfg(windows)]
    {
        // SAFETY: changes only this helper process's error-reporting mode.
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
            );
        }
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn record_publication_crashes_leave_only_contract_states() {
    for (point, committed) in [
        ("file_noclobber.before_publish", false),
        ("file_noclobber.after_publish", true),
        ("job.record_published", true),
    ] {
        let dir = Directory::new();
        let output = helper_command(&dir.0, "publish")
            .env("CPF_STORAGE_CRASH_AT", point)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .any(|line| line == format!("CPF-CRASH-AT {point}"))
        );
        let job = dir.path(".cpforge/runs/job");
        assert!(matches!(probe(&job), ProbeOutcome::Absent));
        if committed {
            assert_eq!(fs::read(job.join("job.toml")).unwrap(), record(0).encode());
        } else {
            assert!(matches!(
                read_job_record(job.join("job.toml")),
                Record::Missing
            ));
        }
        // Job-local publication temps belong to pruning, not entry recovery.
        let temps_before: Vec<_> = fs::read_dir(&job)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let lock = dir.lock();
        let guard = lock.acquire("inspect interrupted job").unwrap();
        for name in temps_before {
            assert!(job.join(name).exists());
        }
        if !committed {
            assert!(
                publish_job_record(&guard, &job, &record(0))
                    .unwrap()
                    .committed
            );
        } else {
            assert!(matches!(
                publish_job_record(&guard, &job, &record(1))
                    .unwrap_err()
                    .kind,
                ErrorKind::Exists { .. }
            ));
        }
    }
}

#[cfg(windows)]
#[test]
fn incompatible_windows_sharing_is_a_failed_probe() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

    let dir = Directory::new();
    fs::write(dir.path("lease"), b"").unwrap();
    let incompatible = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(dir.path("lease"))
        .unwrap();
    assert!(matches!(probe(&dir.0), ProbeOutcome::Failed(error)
        if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)));
    drop(incompatible);
    drop(acquired(&dir.0));
}
