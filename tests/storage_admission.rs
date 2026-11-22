//! Registration and real-process admission/lease/crash boundaries, without runs.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(all(feature = "storage-test-hooks", target_os = "linux"))]
use cpforge::storage::Cleanup;
use cpforge::storage::admission::{
    AdmissionError, AdmissionErrorKind, RegisteredJob, register_job,
};
use cpforge::storage::job::{Epoch, JobId, JobRecord, ProbeOutcome, probe, read_job_record};
use cpforge::storage::job_seq::read_job_sequence;
use cpforge::storage::prune::{PrunePolicy, reclaim, select_and_rename};
#[cfg(feature = "storage-test-hooks")]
use cpforge::storage::{Durability, LockOptions, TestHooks};
use cpforge::storage::{Record, WorkspaceLock};

const EPOCH: &str = "0123456789abcdef0123456789abcdef";
const LIMIT: u64 = 1 << 30;
// Serialize fixture lifetimes against other tests' fork/exec inheriting flocks.
static FIXTURES: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let serial = FIXTURES.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/admission-fixtures")
            .join(format!("{:032x}", u128::from_le_bytes(random)));
        fs::create_dir_all(root.join(".cpforge/runs")).unwrap();
        let fixture = Self {
            root: cpforge::storage::paths::plain(&fs::canonicalize(root).unwrap()),
            _serial: serial,
        };
        fixture.set_next(0);
        fixture
    }

    fn runs(&self) -> PathBuf {
        self.root.join(".cpforge/runs")
    }

    fn sequence_path(&self) -> PathBuf {
        self.root.join(".cpforge/job-seq")
    }

    fn set_next(&self, next: u64) {
        fs::write(
            self.sequence_path(),
            format!("schema = 1\nepoch = \"{EPOCH}\"\nnext = \"{next}\"\n"),
        )
        .unwrap();
    }

    fn next(&self) -> u64 {
        match read_job_sequence(self.sequence_path()) {
            Record::Valid(record) => {
                assert_eq!(record.epoch, EPOCH);
                record.next
            }
            record => panic!("expected sequence: {record:?}"),
        }
    }

    fn admit(
        &self,
        policy: PrunePolicy,
        limit: u64,
        now: SystemTime,
    ) -> Result<RegisteredJob, AdmissionError> {
        let lock = WorkspaceLock::new(&self.root).unwrap();
        register_job(
            lock.acquire("test registration").unwrap(),
            policy,
            limit,
            now,
        )
    }

    fn jobs(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = fs::read_dir(self.runs())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        paths
    }

    fn prune(&self) {
        let lock = WorkspaceLock::new(&self.root).unwrap();
        let guard = lock.acquire("test prune").unwrap();
        let selection = select_and_rename(&guard, PrunePolicy::zero());
        assert!(selection.report.errors.is_empty(), "{selection:?}");
        assert!(selection.report.failed_renames.is_empty(), "{selection:?}");
        drop(guard);
        let report = reclaim(selection.runs_dir);
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(report.walk.errors.is_empty(), "{report:?}");
        assert!(report.walk.skipped.is_empty(), "{report:?}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn now(n: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_790_000_000 + n)
}

fn assert_record(job: &RegisteredJob, n: u64) {
    assert_eq!(job.sequence, (Epoch::parse(EPOCH).unwrap(), n));
    let expected = JobRecord {
        epoch: Epoch::parse(EPOCH).unwrap(),
        n,
    };
    assert!(
        matches!(read_job_record(job.dir.join("job.toml")), Record::Valid(record)
        if record == expected)
    );
    assert_eq!(job.dir.file_name().unwrap(), job.id.as_str());
}

fn assert_inactive(dir: &Path) {
    let start = Instant::now();
    loop {
        match probe(dir) {
            ProbeOutcome::Absent | ProbeOutcome::Acquired(_) => break,
            // Windows can release terminated processes' locks asynchronously.
            ProbeOutcome::Held if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            outcome => panic!("expected inactive job: {outcome:?}"),
        }
    }
}

#[test]
fn fail_closed_sequence_matrix_preserves_bytes_and_creates_no_job() {
    for (bytes, expected) in [
        (None, "missing"),
        (
            Some("schema = 1\nepoch = 'bad'\nnext = '0'".to_owned()),
            "malformed",
        ),
        (Some("schema = 2\nnext = false".to_owned()), "newer"),
        (
            Some(format!(
                "schema = 1\nepoch = '{EPOCH}'\nnext = \"18446744073709551615\"\n"
            )),
            "exhausted",
        ),
    ] {
        let fixture = Fixture::new();
        if let Some(bytes) = &bytes {
            fs::write(fixture.sequence_path(), bytes).unwrap();
        } else {
            fs::remove_file(fixture.sequence_path()).unwrap();
        }
        let error = fixture
            .admit(PrunePolicy::default(), LIMIT, now(0))
            .unwrap_err();
        match expected {
            "missing" => assert!(matches!(error.kind, AdmissionErrorKind::SequenceMissing)),
            "malformed" => assert!(matches!(
                error.kind,
                AdmissionErrorKind::SequenceMalformed(_)
            )),
            "newer" => assert!(matches!(
                error.kind,
                AdmissionErrorKind::SequenceTooNew { schema: 2, .. }
            )),
            "exhausted" => assert!(matches!(error.kind, AdmissionErrorKind::SequenceExhausted)),
            _ => unreachable!(),
        }
        let message = error.to_string();
        if matches!(expected, "missing" | "malformed") {
            assert!(message.contains("run `cpf fsck --apply`"));
        }
        if expected == "newer" {
            assert!(message.contains("needs a newer cpf"));
        }
        if expected == "exhausted" {
            assert!(message.contains("SequenceExhausted"));
        }
        assert!(error.report.allocated.is_none());
        assert!(error.report.created_dir.is_none());
        assert!(fixture.jobs().is_empty());
        assert_eq!(
            fs::read(fixture.sequence_path()).ok(),
            bytes.map(String::into_bytes)
        );
    }
}

#[test]
fn non_regular_sequence_is_unreadable_and_preserved_on_every_platform() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.sequence_path()).unwrap();
    fs::create_dir(fixture.sequence_path()).unwrap();
    let error = fixture
        .admit(PrunePolicy::default(), LIMIT, now(0))
        .unwrap_err();
    let AdmissionErrorKind::SequenceUnreadable(source) = &error.kind else {
        panic!("{error:?}")
    };
    assert!(error.to_string().contains(&source.to_string()));
    assert!(
        error
            .to_string()
            .contains("fix access to `.cpforge/job-seq`")
    );
    assert!(fixture.sequence_path().is_dir());
    assert_eq!(fs::read_dir(fixture.sequence_path()).unwrap().count(), 0);
    assert!(fixture.jobs().is_empty());
}

#[cfg(unix)]
#[test]
fn unreadable_sequence_names_io_error_and_resumes_same_epoch_when_restored() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: reads process identity only; root bypasses these permissions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("chmod unreadability requires an unprivileged user; skipped as root");
        return;
    }
    let fixture = Fixture::new();
    let path = fixture.sequence_path();
    let bytes = fs::read(&path).unwrap();
    let permissions = fs::metadata(&path).unwrap().permissions();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let result = fixture.admit(PrunePolicy::default(), LIMIT, now(0));
    fs::set_permissions(&path, permissions).unwrap();
    let error = result.unwrap_err();
    let AdmissionErrorKind::SequenceUnreadable(source) = &error.kind else {
        panic!("{error:?}")
    };
    assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains(&source.to_string()));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(fixture.jobs().is_empty());
    let job = fixture
        .admit(PrunePolicy::default(), LIMIT, now(1))
        .unwrap();
    assert_record(&job, 0);
    assert_eq!(fixture.next(), 1);
}

#[test]
fn successful_registration_recovers_once_advances_once_and_shares_owner_accounting() {
    let fixture = Fixture::new();
    // Registration also creates runs/ if it is absent.
    fs::remove_dir(fixture.runs()).unwrap();
    let leftover = fixture.root.join(format!(".cpf-tmp-{}", "a".repeat(32)));
    fs::write(&leftover, b"interrupted metadata").unwrap();
    let job = fixture
        .admit(PrunePolicy::default(), LIMIT, now(0))
        .unwrap();
    assert!(!leftover.exists());
    assert_record(&job, 0);
    assert_eq!(fixture.next(), 1);
    assert_eq!(fixture.jobs(), vec![job.dir.clone()]);
    assert!(
        job.prune_report.selection.errors.is_empty(),
        "{:?}",
        job.prune_report
    );
    assert!(job.prune_report.reclaim.errors.is_empty());
    assert_eq!(job.prune_report.publications.len(), 2);
    let len = fs::metadata(job.dir.join("job.toml")).unwrap().len();
    assert_eq!(job.budget.usage().retained, len);
    assert_eq!(job.budget.usage().reserved, 0);
    assert!(Arc::ptr_eq(job.owner.budget().unwrap(), &job.budget));
    let owner = Arc::clone(&job.owner);
    let last = Arc::clone(&owner);
    let dir = job.dir.clone();
    let budget = Arc::clone(&job.budget);
    drop(job);
    assert!(matches!(probe(&dir), ProbeOutcome::Held));
    drop(owner);
    assert!(matches!(probe(&dir), ProbeOutcome::Held));
    assert_eq!(budget.usage().retained, len);
    drop(last);
    assert_inactive(&dir);
    assert_eq!(budget.usage().retained, 0);
    fixture.prune();
    assert!(fixture.jobs().is_empty());
}

#[test]
fn tiny_job_budget_leaves_inactive_directory_and_sequence_gap() {
    let fixture = Fixture::new();
    let len = JobRecord {
        epoch: Epoch::parse(EPOCH).unwrap(),
        n: 0,
    }
    .encode()
    .len() as u64;
    // Enough for the final file alone still cannot cover the publication temp.
    for (n, limit) in [1, len, len * 2 - 1].into_iter().enumerate() {
        let error = fixture
            .admit(PrunePolicy::default(), limit, now(n as u64))
            .unwrap_err();
        assert!(
            matches!(error.kind, AdmissionErrorKind::BudgetExceeded(_)),
            "{error:?}"
        );
        assert_eq!(error.report.allocated.as_ref().unwrap().1, n as u64);
        let dir = error.report.created_dir.as_ref().unwrap();
        assert!(dir.is_dir());
        assert!(!dir.join("job.toml").exists());
        assert!(!dir.join("lease").exists());
        assert_eq!(fs::read_dir(dir).unwrap().count(), 0);
        assert_inactive(dir);
        assert_eq!(fixture.next(), n as u64 + 1);
    }
    let job = fixture
        .admit(PrunePolicy::default(), len * 2, now(4))
        .unwrap();
    assert_record(&job, 3);
    assert_eq!(fixture.next(), 4);
    fixture.prune();
    assert_eq!(fixture.jobs(), vec![job.dir.clone()]);
    drop(job);
    fixture.prune();
    assert!(fixture.jobs().is_empty());
}

#[test]
fn admission_prunes_inactive_jobs_beyond_keep_count_and_reclaims_before_returning() {
    let fixture = Fixture::new();
    let mut dirs = Vec::new();
    for n in 0..3 {
        let job = fixture
            .admit(PrunePolicy::default(), LIMIT, now(n))
            .unwrap();
        dirs.push(job.dir.clone());
    }
    let active = fixture
        .admit(PrunePolicy::new(1, u64::MAX), LIMIT, now(3))
        .unwrap();
    assert_eq!(active.prune_report.renamed.len(), 2);
    assert_eq!(active.prune_report.reclaim.reclaimed.len(), 2);
    assert!(!dirs[0].exists());
    assert!(!dirs[1].exists());
    assert!(dirs[2].is_dir());
    assert_eq!(fixture.jobs().len(), 2);
    // Workspace usage is a pruning target; an active lease is always preserved.
    let second = fixture.admit(PrunePolicy::zero(), LIMIT, now(4)).unwrap();
    assert_eq!(
        second.prune_report.selection.skipped_active,
        vec![active.id.clone()]
    );
    assert!(active.dir.is_dir());
    assert!(!dirs[2].exists());
    assert_record(&second, 4);
}

#[test]
fn sequence_refusal_still_reclaims_candidates_renamed_in_step_two() {
    let fixture = Fixture::new();
    let old = fixture
        .admit(PrunePolicy::default(), LIMIT, now(0))
        .unwrap();
    let old_dir = old.dir.clone();
    drop(old);
    fs::remove_file(fixture.sequence_path()).unwrap();
    let error = fixture
        .admit(PrunePolicy::zero(), LIMIT, now(1))
        .unwrap_err();
    assert!(matches!(error.kind, AdmissionErrorKind::SequenceMissing));
    assert_eq!(error.report.renamed.len(), 1);
    assert_eq!(error.report.reclaim.reclaimed.len(), 1);
    assert!(!old_dir.exists());
    assert!(fixture.jobs().is_empty());
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn metadata_publish_collision_keeps_gap_and_still_reclaims_selected_jobs() {
    fn occupy_record(target: &Path) {
        fs::write(target, b"independent creator").unwrap();
    }
    let fixture = Fixture::new();
    let old = fixture
        .admit(PrunePolicy::default(), LIMIT, now(0))
        .unwrap();
    let old_dir = old.dir.clone();
    drop(old);
    let lock = WorkspaceLock::new(&fixture.root).unwrap();
    let guard = lock
        .acquire_with_options(
            "metadata failure",
            LockOptions {
                test_hooks: TestHooks {
                    before_file_publish: Some(occupy_record),
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let error = register_job(guard, PrunePolicy::zero(), LIMIT, now(1)).unwrap_err();
    assert!(
        matches!(error.kind, AdmissionErrorKind::Storage(_)),
        "{error:?}"
    );
    assert_eq!(fixture.next(), 2);
    assert_eq!(error.report.allocated.as_ref().unwrap().1, 1);
    assert_eq!(error.report.publications.len(), 1);
    assert_eq!(error.report.reclaim.reclaimed.len(), 1);
    assert!(!old_dir.exists());
    let dir = error.report.created_dir.as_ref().unwrap();
    assert_eq!(fs::read_dir(dir).unwrap().count(), 1);
    assert_eq!(
        fs::read(dir.join("job.toml")).unwrap(),
        b"independent creator"
    );
    assert_inactive(dir);
    let job = fixture.admit(PrunePolicy::zero(), LIMIT, now(2)).unwrap();
    assert_record(&job, 2);
    assert!(!dir.exists());
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn lease_creation_failure_preserves_published_metadata_and_inactive_directory() {
    fn occupy_lease(target: &Path) {
        fs::write(target.parent().unwrap().join("lease"), b"").unwrap();
    }
    let fixture = Fixture::new();
    let lock = WorkspaceLock::new(&fixture.root).unwrap();
    let guard = lock
        .acquire_with_options(
            "lease failure",
            LockOptions {
                test_hooks: TestHooks {
                    before_file_publish: Some(occupy_lease),
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let error = register_job(guard, PrunePolicy::default(), LIMIT, now(0)).unwrap_err();
    assert!(
        matches!(error.kind, AdmissionErrorKind::Storage(_)),
        "{error:?}"
    );
    assert_eq!(fixture.next(), 1);
    assert_eq!(error.report.publications.len(), 2);
    let dir = error.report.created_dir.as_ref().unwrap();
    assert!(
        matches!(read_job_record(dir.join("job.toml")), Record::Valid(record) if record.n == 0)
    );
    assert_inactive(dir);
    fixture.prune();
    assert!(fixture.jobs().is_empty());
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn recovery_and_durability_warnings_survive_successful_registration() {
    let fixture = Fixture::new();
    let leftover = fixture.root.join(format!(".cpf-tmp-{}", "b".repeat(32)));
    fs::write(&leftover, b"interrupted metadata").unwrap();
    let lock = WorkspaceLock::new(&fixture.root).unwrap();
    let guard = lock
        .acquire_with_options(
            "registration warnings",
            LockOptions {
                test_hooks: TestHooks {
                    recovery_remove_failure: Some(leftover.clone()),
                    post_commit_sync_failure: true,
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let job = register_job(guard, PrunePolicy::default(), LIMIT, now(0)).unwrap();
    assert_record(&job, 0);
    assert_eq!(fixture.next(), 1);
    assert_eq!(
        job.prune_report.recovery.as_ref().unwrap().paths,
        vec![leftover]
    );
    assert_eq!(job.prune_report.publications.len(), 2);
    assert!(
        job.prune_report
            .publications
            .iter()
            .all(|(_, result)| matches!(result.durability, Durability::Uncertain(_)))
    );
}

#[cfg(all(feature = "storage-test-hooks", target_os = "linux"))]
#[test]
fn failed_temp_unlink_retains_both_logical_lengths_until_successful_removal() {
    let fixture = Fixture::new();
    let lock = WorkspaceLock::new(&fixture.root).unwrap();
    let guard = lock
        .acquire_with_options(
            "publication temp accounting",
            LockOptions {
                test_hooks: TestHooks {
                    force_link_fallback: true,
                    fail_unlink: true,
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let job = register_job(guard, PrunePolicy::default(), LIMIT, now(0)).unwrap();
    let publication = &job.prune_report.publications[1].1;
    let Cleanup::Pending(temp) = &publication.cleanup else {
        panic!("{publication:?}")
    };
    let len = fs::metadata(job.dir.join("job.toml")).unwrap().len();
    assert_eq!(fs::metadata(temp).unwrap().len(), len);
    assert_eq!(job.budget.usage().retained, len * 2);
    assert_eq!(job.budget.usage().reserved, 0);
    // A failed removal must leave accounting intact. Only successful unlink
    // authorizes release, even though these names refer to the same inode.
    assert!(fs::remove_dir(temp).is_err());
    assert_eq!(job.budget.usage().retained, len * 2);
    fs::remove_file(temp).unwrap();
    job.budget.release_retained(len).unwrap();
    assert_eq!(job.budget.usage().retained, len);
}

#[test]
fn last_allocatable_sequence_is_persisted_without_signed_integer_truncation() {
    let fixture = Fixture::new();
    fixture.set_next(u64::MAX - 1);
    let job = fixture
        .admit(PrunePolicy::default(), LIMIT, now(0))
        .unwrap();
    assert_record(&job, u64::MAX - 1);
    assert_eq!(fixture.next(), u64::MAX);
    let bytes = fs::read(fixture.sequence_path()).unwrap();
    let error = fixture
        .admit(PrunePolicy::default(), LIMIT, now(1))
        .unwrap_err();
    assert!(matches!(error.kind, AdmissionErrorKind::SequenceExhausted));
    assert_eq!(fs::read(fixture.sequence_path()).unwrap(), bytes);
    assert_eq!(fixture.jobs(), vec![job.dir.clone()]);
}

#[test]
fn two_processes_admit_distinct_sequences_and_directories() {
    let fixture = Fixture::new();
    let mut first = Helper::start(&fixture.root, None);
    let mut second = Helper::start(&fixture.root, None);
    first.release();
    second.release();
    let (first_n, first_id) = first.admitted();
    let (second_n, second_id) = second.admitted();
    let mut numbers = [first_n, second_n];
    numbers.sort();
    assert_eq!(numbers, [0, 1]);
    assert_ne!(first_id, second_id);
    assert_eq!(fixture.next(), 2);
    for (n, id) in [(first_n, &first_id), (second_n, &second_id)] {
        let dir = fixture.runs().join(id.as_str());
        assert!(matches!(probe(&dir), ProbeOutcome::Held));
        assert!(
            matches!(read_job_record(dir.join("job.toml")), Record::Valid(record) if record.n == n)
        );
    }
    assert_eq!(fixture.jobs().len(), 2);
    first.release();
    second.release();
    first.finish();
    second.finish();
    // Windows can release exited processes' locks asynchronously.
    for id in [first_id, second_id] {
        assert_inactive(&fixture.runs().join(id.as_str()));
    }
    fixture.prune();
    assert!(fixture.jobs().is_empty());
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn another_process_cannot_prune_between_directory_creation_and_lease_lock() {
    let fixture = Fixture::new();
    let mut admission = Helper::start_mode(&fixture.root, None, "paused");
    admission.release();
    admission.message("CPF-ADMISSION-PAUSED");
    let dirs = fixture.jobs();
    assert_eq!(dirs.len(), 1);
    assert!(!dirs[0].join("lease").exists());
    // This process attempts entry while the directory is interrupted-looking;
    // the workspace lock must exclude it before it can make any prune decision.
    let mut blocked = Helper::start_mode(&fixture.root, None, "probe-lock");
    blocked.release();
    blocked.message("CPF-PRUNE-BUSY");
    blocked.finish();
    admission.release();
    let (_, id) = admission.admitted();
    assert_eq!(dirs[0], fixture.runs().join(id.as_str()));
    let mut pruning = Helper::start_mode(&fixture.root, None, "prune");
    pruning.release();
    assert_eq!(pruning.message("CPF-PRUNED "), "0 1");
    pruning.finish();
    assert!(dirs[0].is_dir());
    admission.release();
    admission.finish();
    assert_inactive(&dirs[0]);
    fixture.prune();
    assert!(fixture.jobs().is_empty());
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn clean_apply_racing_paused_admission_reports_busy_and_keeps_directory() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("cpforge.toml"), "schema = 1\n").unwrap();
    fs::create_dir(fixture.root.join("user")).unwrap();
    let mut admission = Helper::start_mode(&fixture.root, None, "paused");
    admission.release();
    admission.message("CPF-ADMISSION-PAUSED");
    let dirs = fixture.jobs();
    assert_eq!(dirs.len(), 1);
    assert!(!dirs[0].join("lease").exists());
    // The paused publication's `.cpf-tmp-` file already sits in the job directory.
    let entries = |dir: &Path| {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = entries(&dirs[0]);
    assert!(
        before.len() == 1 && before[0].to_string_lossy().starts_with(".cpf-tmp-"),
        "{before:?}"
    );
    let sequence = fs::read(fixture.sequence_path()).unwrap();
    let mut clean = Command::new(env!("CARGO_BIN_EXE_cpf"))
        .current_dir(&fixture.root)
        .env("HOME", fixture.root.join("user"))
        .env("USERPROFILE", fixture.root.join("user"))
        .env("XDG_CONFIG_HOME", fixture.root.join("user"))
        .env("APPDATA", fixture.root.join("user"))
        .env_remove("CPF_STORAGE_CRASH_AT")
        .env_remove("CPF_STORAGE_PAUSE_AT")
        .args(["clean", "--apply"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while clean.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = clean.kill();
            let _ = clean.wait();
            panic!("clean did not finish while admission was paused");
        }
        std::thread::yield_now();
    }
    let output = clean.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(8), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("workspace is busy"));
    assert!(output.stdout.is_empty());
    assert_eq!(fixture.jobs(), dirs);
    assert!(dirs[0].is_dir());
    assert_eq!(entries(&dirs[0]), before);
    assert_eq!(fs::read(fixture.sequence_path()).unwrap(), sequence);
    admission.release();
    let (_, id) = admission.admitted();
    assert_eq!(dirs[0], fixture.runs().join(id.as_str()));
    admission.release();
    admission.finish();
    assert_inactive(&dirs[0]);
    fixture.prune();
    assert!(fixture.jobs().is_empty());
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn crash_after_unlock_leaves_renamed_candidates_for_following_admission_to_reclaim() {
    let fixture = Fixture::new();
    let old = fixture
        .admit(PrunePolicy::default(), LIMIT, now(0))
        .unwrap();
    let reclaim_path = fixture.runs().join(old.id.reclaim_name());
    drop(old);
    let mut child = Helper::start_mode(&fixture.root, Some("admission.lock_released"), "prune-all");
    child.release();
    let (status, stderr) = child.wait();
    assert!(!status.success());
    assert!(stderr.contains("CPF-CRASH-AT admission.lock_released"));
    assert_eq!(fixture.next(), 2);
    assert!(reclaim_path.is_dir());
    for dir in fixture.jobs() {
        assert_inactive(&dir);
    }
    let job = fixture.admit(PrunePolicy::zero(), LIMIT, now(2)).unwrap();
    assert_record(&job, 2);
    assert_eq!(job.prune_report.reclaim.reclaimed.len(), 2);
    assert!(!reclaim_path.exists());
    assert_eq!(fixture.jobs(), vec![job.dir.clone()]);
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn admission_crashes_leave_only_allowed_states_and_following_admission_and_prune_work() {
    for (point, has_dir, has_record, has_lease) in [
        ("admission.job_seq_replaced", false, false, false),
        ("admission.directory_created", true, false, false),
        ("admission.job_record_published", true, true, false),
        ("admission.lease_created", true, true, true),
        ("admission.lease_locked", true, true, true),
        ("admission.lock_released", true, true, true),
        // Also cover interruption while a metadata publication temp exists.
        ("file_noclobber.before_publish", true, false, false),
    ] {
        let fixture = Fixture::new();
        let mut child = Helper::start(&fixture.root, Some(point));
        child.release();
        let (status, stderr) = child.wait();
        assert!(!status.success(), "{point}");
        assert!(
            stderr
                .lines()
                .any(|line| line.strip_prefix("CPF-CRASH-AT ") == Some(point)),
            "{stderr}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGABRT));
        }
        assert_eq!(fixture.next(), 1, "{point}");
        let leftovers = fixture.jobs();
        assert_eq!(leftovers.len(), usize::from(has_dir), "{point}");
        for dir in &leftovers {
            assert_eq!(dir.join("job.toml").exists(), has_record, "{point}");
            assert_eq!(dir.join("lease").exists(), has_lease, "{point}");
            assert_inactive(dir);
            if has_record {
                assert!(
                    matches!(read_job_record(dir.join("job.toml")), Record::Valid(record)
                    if record.epoch.as_str() == EPOCH && record.n == 0)
                );
            }
        }
        // Holding this new owner while pruning proves that interrupted storage
        // is reclaimable and a registered live job is excluded.
        let job = fixture
            .admit(PrunePolicy::default(), LIMIT, now(1))
            .unwrap();
        assert_record(&job, 1);
        assert_eq!(fixture.next(), 2);
        fixture.prune();
        for dir in leftovers {
            assert!(!dir.exists(), "{point}");
        }
        assert_eq!(fixture.jobs(), vec![job.dir.clone()]);
        drop(job);
        fixture.prune();
        assert!(fixture.jobs().is_empty(), "{point}");
    }
}

struct Helper {
    child: Child,
    messages: Receiver<String>,
}

impl Helper {
    fn start(root: &Path, crash: Option<&str>) -> Self {
        Self::start_mode(root, crash, "admit")
    }

    fn start_mode(root: &Path, crash: Option<&str>, mode: &str) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "admission_child", "--nocapture"])
            .env("CPF_ADMISSION_ROOT", root)
            .env("CPF_ADMISSION_MODE", mode)
            .env_remove("CPF_STORAGE_CRASH_AT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(point) = crash {
            command.env("CPF_STORAGE_CRASH_AT", point);
        }
        let mut child = command.spawn().unwrap();
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
        helper.message("CPF-ADMISSION-READY");
        helper
    }

    fn message(&self, prefix: &str) -> String {
        loop {
            let line = self.messages.recv_timeout(Duration::from_secs(30)).unwrap();
            if let Some((_, message)) = line.split_once(prefix) {
                return message.to_owned();
            }
        }
    }

    fn admitted(&self) -> (u64, JobId) {
        let message = self.message("CPF-ADMITTED ");
        let (n, id) = message.split_once(' ').unwrap();
        (n.parse().unwrap(), JobId::parse(id).unwrap())
    }

    fn release(&mut self) {
        self.child.stdin.as_mut().unwrap().write_all(b"x").unwrap();
    }

    fn wait(&mut self) -> (ExitStatus, String) {
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "admission helper timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        (status, stderr)
    }

    fn finish(mut self) {
        let (status, stderr) = self.wait();
        assert!(status.success(), "{stderr}");
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
fn admission_child() {
    #[cfg(feature = "storage-test-hooks")]
    suppress_crash_artifacts();
    let root = PathBuf::from(std::env::var_os("CPF_ADMISSION_ROOT").expect("helper workspace"));
    let mode = std::env::var("CPF_ADMISSION_MODE").unwrap();
    println!("CPF-ADMISSION-READY");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
    let lock = WorkspaceLock::new(&root).unwrap();
    #[cfg(feature = "storage-test-hooks")]
    if mode == "probe-lock" {
        let error = lock
            .acquire_with_options(
                "competing prune",
                LockOptions {
                    timeout: Duration::ZERO,
                    ..LockOptions::default()
                },
            )
            .unwrap_err();
        assert!(
            matches!(error.kind, cpforge::storage::ErrorKind::Busy { .. }),
            "{error:?}"
        );
        println!("CPF-PRUNE-BUSY");
        return;
    }
    if mode == "prune" {
        let guard = lock.acquire("competing prune").unwrap();
        let selection = select_and_rename(&guard, PrunePolicy::zero());
        let renamed = selection.renamed.len();
        let skipped = selection.report.skipped_active.len();
        drop(guard);
        let report = reclaim(selection.runs_dir);
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(report.walk.errors.is_empty(), "{report:?}");
        println!("CPF-PRUNED {renamed} {skipped}");
        return;
    }
    #[cfg(feature = "storage-test-hooks")]
    let guard = lock
        .acquire_with_options(
            "subprocess admission",
            LockOptions {
                test_hooks: TestHooks {
                    before_file_publish: if mode == "paused" {
                        Some(pause_before_metadata)
                    } else {
                        None
                    },
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    #[cfg(not(feature = "storage-test-hooks"))]
    let guard = lock.acquire("subprocess admission").unwrap();
    let job = register_job(
        guard,
        if mode == "prune-all" {
            PrunePolicy::zero()
        } else {
            PrunePolicy::default()
        },
        LIMIT,
        now(0),
    )
    .unwrap();
    println!("CPF-ADMITTED {} {}", job.sequence.1, job.id);
    std::io::stdout().flush().unwrap();
    assert!(
        std::env::var_os("CPF_STORAGE_CRASH_AT").is_none(),
        "expected admission crash"
    );
    std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
    drop(job);
}

#[cfg(feature = "storage-test-hooks")]
fn pause_before_metadata(_target: &Path) {
    println!("CPF-ADMISSION-PAUSED");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
}

#[cfg(feature = "storage-test-hooks")]
fn suppress_crash_artifacts() {
    #[cfg(unix)]
    {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid limit, changing only this helper's core-file limit.
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
