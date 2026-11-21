//! Pruning fixtures and subprocesses use real, independently opened job leases.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use cpforge::storage::job::{Epoch, JobId, JobLease, JobOwner, JobRecord, ProbeOutcome, probe};
use cpforge::storage::prune::{
    PrunePolicy, PruneSelection, ReclaimReport, reclaim, select_and_rename,
};
use cpforge::storage::{WorkspaceGuard, WorkspaceLock, measure_tree};

// Avoid fork/exec in another test briefly inheriting a fixture's Unix flock.
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
        // All fixture writes stay inside the worktree, including helper writes.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/prune-fixtures")
            .join(format!("{:032x}", u128::from_le_bytes(random)));
        fs::create_dir_all(root.join(".cpforge/runs")).unwrap();
        Self {
            root: cpforge::storage::paths::plain(&fs::canonicalize(root).unwrap()),
            _serial: serial,
        }
    }

    fn runs(&self) -> PathBuf {
        self.root.join(".cpforge/runs")
    }

    fn job(&self, n: u64) -> PathBuf {
        self.runs().join(id(n).as_str())
    }

    fn reclaim_path(&self, n: u64) -> PathBuf {
        self.runs().join(id(n).reclaim_name())
    }

    fn lock(&self) -> WorkspaceLock {
        WorkspaceLock::new(&self.root).unwrap()
    }

    fn add(&self, n: u64, bytes: usize) -> JobOwner {
        let lock = self.lock();
        let guard = lock.acquire("fixture job").unwrap();
        let path = guard.create_dir_all(self.job(n)).unwrap();
        fs::write(
            path.join("job.toml"),
            JobRecord {
                epoch: Epoch::parse("0123456789abcdef0123456789abcdef").unwrap(),
                n,
            }
            .encode(),
        )
        .unwrap();
        fs::write(path.join("payload"), vec![0_u8; bytes]).unwrap();
        JobLease::create_and_lock(path).unwrap()
    }

    fn select(&self, policy: PrunePolicy) -> PruneSelection {
        let lock = self.lock();
        let guard = lock.acquire("fixture prune").unwrap();
        select_and_rename(&guard, policy)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Teardown only; the behavior under test always uses the storage walker.
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn id(n: u64) -> JobId {
    JobId::parse(&format!("20261120230104123-{n:016x}")).unwrap()
}

fn assert_reclaimed(report: &ReclaimReport) {
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(report.walk.errors.is_empty(), "{report:?}");
    assert!(report.walk.skipped.is_empty(), "{report:?}");
}

fn measured(guard: &WorkspaceGuard<'_>, path: &Path) -> u64 {
    let report = measure_tree(guard, path).unwrap();
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(report.skipped.is_empty(), "{report:?}");
    report.bytes
}

#[test]
fn keep_boundary_uses_ids_even_when_creation_and_modification_order_differ() {
    let fixture = Fixture::new();
    let policy = PrunePolicy::new(2, u64::MAX);
    assert_eq!(PrunePolicy::default(), PrunePolicy::new(20, 4 << 30));
    // The lexically newest job is created first. Give the oldest job a visibly
    // newer mtime too, without depending on filesystem timestamp granularity.
    drop(fixture.add(30, 7));
    drop(fixture.add(20, 7));
    assert!(fixture.select(policy).renamed.is_empty());
    drop(fixture.add(10, 7));
    let old = fs::metadata(fixture.job(30)).unwrap().modified().unwrap();
    let mut directory = fs::OpenOptions::new();
    directory.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
        };
        directory
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    directory
        .open(fixture.job(10))
        .unwrap()
        .set_modified(old + Duration::from_secs(60))
        .unwrap();
    assert!(fs::metadata(fixture.job(10)).unwrap().modified().unwrap() > old);
    let selection = fixture.select(policy);
    assert_eq!(selection.renamed, vec![id(10)]);
    assert!(fixture.job(20).is_dir());
    assert!(fixture.job(30).is_dir());
    assert!(fixture.reclaim_path(10).is_dir());
    assert_reclaimed(&reclaim(&selection.runs_dir));
    assert!(!fixture.reclaim_path(10).exists());
}

#[test]
fn active_jobs_count_in_usage_and_do_not_take_inactive_retention_slots() {
    let fixture = Fixture::new();
    let active = fixture.add(1, 800);
    drop(fixture.add(2, 10));
    drop(fixture.add(3, 10));
    let lock = fixture.lock();
    let guard = lock.acquire("measure active usage").unwrap();
    let total = measured(&guard, &fixture.runs());
    let active_bytes = measured(&guard, &fixture.job(1));
    let kept = select_and_rename(&guard, PrunePolicy::new(2, u64::MAX));
    assert!(kept.renamed.is_empty());
    assert_eq!(kept.report.skipped_active, vec![id(1)]);
    assert_eq!(kept.report.usage.bytes, total);
    // Active usage alone exceeds the target: all inactive jobs can be removed
    // even though the target remains unreachable.
    let selection = select_and_rename(&guard, PrunePolicy::new(2, active_bytes - 1));
    assert_eq!(selection.renamed, vec![id(2), id(3)]);
    assert_eq!(selection.report.projected_bytes, active_bytes);
    assert_eq!(selection.report.skipped_active, vec![id(1)]);
    assert!(fixture.job(1).is_dir());
    drop(guard);
    assert_reclaimed(&reclaim(&selection.runs_dir));
    drop(active);
}

#[test]
fn byte_policy_removes_oldest_until_projected_usage_reaches_target() {
    let fixture = Fixture::new();
    for n in 1..=4 {
        drop(fixture.add(n, 100));
    }
    let lock = fixture.lock();
    let guard = lock.acquire("byte pruning").unwrap();
    let total = measured(&guard, &fixture.runs());
    let one = measured(&guard, &fixture.job(1));
    let two = measured(&guard, &fixture.job(2));
    let target = total - one - two;
    let selection = select_and_rename(&guard, PrunePolicy::new(4, target));
    assert_eq!(selection.renamed, vec![id(1), id(2)]);
    assert_eq!(selection.report.projected_bytes, target);
    assert_eq!(selection.report.usage.bytes, total);
    assert!(!selection.report.incomplete_measurement);
    // Renaming alone has not freed actual bytes yet.
    assert_eq!(measured(&guard, &fixture.runs()), total);
    drop(guard);
    assert_reclaimed(&reclaim(&selection.runs_dir));
}

#[test]
fn count_candidates_reduce_projection_before_considering_kept_jobs() {
    let fixture = Fixture::new();
    for n in 1..=3 {
        drop(fixture.add(n, 100));
    }
    let lock = fixture.lock();
    let guard = lock.acquire("combined pruning triggers").unwrap();
    let target = measured(&guard, &fixture.job(2)) + measured(&guard, &fixture.job(3));
    let selection = select_and_rename(&guard, PrunePolicy::new(2, target));
    assert_eq!(selection.renamed, vec![id(1)]);
    assert_eq!(selection.report.projected_bytes, target);
    assert!(fixture.job(2).is_dir());
    assert!(fixture.job(3).is_dir());
}

#[test]
fn logical_measurement_counts_sparse_lengths_and_each_hard_link() {
    let fixture = Fixture::new();
    drop(fixture.add(1, 0));
    drop(fixture.add(2, 0));
    fs::OpenOptions::new()
        .write(true)
        .open(fixture.job(1).join("payload"))
        .unwrap()
        .set_len(1024 * 1024)
        .unwrap();
    fs::hard_link(
        fixture.job(1).join("payload"),
        fixture.job(2).join("linked-payload"),
    )
    .unwrap();
    let lock = fixture.lock();
    let guard = lock.acquire("logical usage").unwrap();
    let one = measured(&guard, &fixture.job(1));
    let two = measured(&guard, &fixture.job(2));
    assert!(one >= 1024 * 1024);
    assert!(two >= 1024 * 1024);
    let selection = select_and_rename(&guard, PrunePolicy::new(2, two));
    assert_eq!(selection.report.usage.bytes, one + two);
    assert_eq!(selection.renamed, vec![id(1)]);
    assert_eq!(selection.report.projected_bytes, two);
}

#[test]
fn newer_metadata_is_protected_at_job_and_run_depths() {
    let fixture = Fixture::new();
    for n in 1..=7 {
        drop(fixture.add(n, 7));
    }
    fs::write(fixture.job(1).join("job.toml"), "schema = 2\n").unwrap();
    fs::write(
        fixture.job(2).join("report.json"),
        r#"{"schema":2,"unknown":[]}"#,
    )
    .unwrap();
    fs::create_dir(fixture.job(3).join("run-a")).unwrap();
    fs::write(fixture.job(3).join("run-a/spec.json"), r#"{"schema":2}"#).unwrap();
    fs::create_dir(fixture.job(4).join("run-b")).unwrap();
    fs::write(fixture.job(4).join("run-b/record.json"), r#"{"schema":2}"#).unwrap();
    // Supported schemas need no current format's fields for pruning.
    fs::write(fixture.job(5).join("report.json"), r#"{"schema":1}"#).unwrap();
    fs::write(
        fixture.job(6).join("spec.json"),
        r#"{"schema":1,"anything":[]}"#,
    )
    .unwrap();
    // Same names outside the record positions (child output) are not records.
    fs::create_dir_all(fixture.job(7).join("run-c/cwd")).unwrap();
    fs::write(
        fixture.job(7).join("run-c/cwd/record.json"),
        r#"{"schema":2}"#,
    )
    .unwrap();
    fs::write(fixture.job(7).join("run-c/cwd/job.toml"), "schema = 2\n").unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    assert_eq!(selection.renamed, vec![id(5), id(6), id(7)]);
    assert_eq!(selection.report.protected.len(), 4);
    for (n, protected) in (1..=4).zip(&selection.report.protected) {
        assert_eq!(protected.job, id(n));
        assert_eq!(protected.schema, 2);
        assert!(fixture.job(n).is_dir());
    }
    assert_reclaimed(&reclaim(&selection.runs_dir));
}

#[test]
fn protected_inactive_jobs_still_count_for_keep_jobs() {
    let fixture = Fixture::new();
    drop(fixture.add(1, 7));
    drop(fixture.add(2, 7));
    fs::write(fixture.job(2).join("job.toml"), "schema = 2\n").unwrap();
    let selection = fixture.select(PrunePolicy::new(1, u64::MAX));
    assert_eq!(selection.renamed, vec![id(1)]);
    assert_eq!(selection.report.protected.len(), 1);
}

#[test]
fn interrupted_jobs_with_missing_or_malformed_metadata_are_prunable() {
    let fixture = Fixture::new();
    for n in 1..=6 {
        drop(fixture.add(n, 0));
    }
    fs::remove_file(fixture.job(1).join("job.toml")).unwrap();
    fs::write(fixture.job(2).join("job.toml"), "schema = [").unwrap();
    fs::remove_file(fixture.job(3).join("lease")).unwrap();
    fs::remove_file(fixture.job(3).join("job.toml")).unwrap();
    fs::write(fixture.job(4).join("job.toml"), "schema = 1\n").unwrap();
    fs::write(fixture.job(5).join("report.json"), b"\xff").unwrap();
    fs::write(
        fixture.job(6).join("report.json"),
        vec![b' '; 1024 * 1024 + 1],
    )
    .unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    assert_eq!(selection.renamed, (1..=6).map(id).collect::<Vec<_>>());
    assert!(selection.report.unreadable_metadata.is_empty());
    assert_reclaimed(&reclaim(&selection.runs_dir));
    assert_eq!(fs::read_dir(fixture.runs()).unwrap().count(), 0);
}

#[test]
fn nonregular_metadata_and_failed_lease_probes_keep_the_job() {
    let fixture = Fixture::new();
    for n in 1..=3 {
        drop(fixture.add(n, 1));
    }
    fs::remove_file(fixture.job(1).join("job.toml")).unwrap();
    fs::create_dir(fixture.job(1).join("job.toml")).unwrap();
    fs::remove_file(fixture.job(2).join("lease")).unwrap();
    fs::create_dir(fixture.job(2).join("lease")).unwrap();
    fs::create_dir_all(fixture.job(3).join("run-a/record.json")).unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    assert!(selection.renamed.is_empty());
    assert_eq!(selection.report.failed_probes.len(), 1);
    assert_eq!(selection.report.failed_probes[0].0, fixture.job(2));
    assert_eq!(selection.report.unreadable_metadata.len(), 2);
    for n in 1..=3 {
        assert!(fixture.job(n).is_dir());
    }
}

#[cfg(unix)]
#[test]
fn unreadable_metadata_and_lease_are_left_with_errors() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid only reads process identity. Root bypasses these modes.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("chmod unreadability requires an unprivileged user; skipped as root");
        return;
    }
    let fixture = Fixture::new();
    for n in 1..=3 {
        drop(fixture.add(n, 0));
    }
    fs::write(fixture.job(3).join("report.json"), r#"{"schema":2}"#).unwrap();
    let paths = [
        fixture.job(1).join("job.toml"),
        fixture.job(2).join("lease"),
        fixture.job(3).join("report.json"),
    ];
    for path in &paths {
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    }
    let selection = fixture.select(PrunePolicy::zero());
    for path in &paths {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(selection.renamed.is_empty());
    assert_eq!(selection.report.unreadable_metadata.len(), 2);
    assert_eq!(selection.report.failed_probes.len(), 1);
    assert_eq!(
        fixture.select(PrunePolicy::zero()).renamed,
        vec![id(1), id(2)]
    );
}

#[cfg(unix)]
#[test]
fn incomplete_measurement_reports_known_bytes_and_uses_only_keep_jobs() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid only reads process identity. Root bypasses these modes.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("chmod unreadability requires an unprivileged user; skipped as root");
        return;
    }
    let fixture = Fixture::new();
    for n in 1..=3 {
        drop(fixture.add(n, 100));
    }
    // An unmeasurable entry beside the jobs: inside a job it would also keep
    // that job (unlistable records), which is tested separately.
    let hidden = fixture.job(1).parent().unwrap().join("hidden");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("unknown-bytes"), vec![0_u8; 1000]).unwrap();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
    let selection = fixture.select(PrunePolicy::new(2, 0));
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(selection.renamed, vec![id(1)]);
    assert!(selection.report.incomplete_measurement);
    assert!(selection.report.usage.bytes >= 300);
    assert_eq!(selection.report.usage.errors.len(), 1);
    assert_eq!(selection.report.usage.errors[0].0, hidden);
    assert!(selection.report.projected_bytes > 0);
    assert!(fixture.job(2).is_dir());
    assert!(fixture.job(3).is_dir());
    assert_reclaimed(&reclaim(&selection.runs_dir));
}

#[test]
fn existing_reclaim_target_blocks_only_its_candidate_and_is_not_subtracted() {
    let fixture = Fixture::new();
    for n in 1..=3 {
        drop(fixture.add(n, 100));
    }
    // An empty directory must not be silently replaced by Unix rename.
    fs::create_dir(fixture.reclaim_path(1)).unwrap();
    let lock = fixture.lock();
    let guard = lock.acquire("no overwrite").unwrap();
    let one = measured(&guard, &fixture.job(1));
    let three = measured(&guard, &fixture.job(3));
    // Keep trigger selects job 1, but its failed rename frees no projected
    // bytes; the byte trigger must continue on to job 2.
    let selection = select_and_rename(&guard, PrunePolicy::new(2, one + three));
    assert_eq!(selection.renamed, vec![id(2)]);
    assert_eq!(selection.report.failed_renames.len(), 1);
    assert_eq!(selection.report.failed_renames[0].job, id(1));
    assert!(matches!(
        selection.report.failed_renames[0].error.kind,
        cpforge::storage::ErrorKind::Exists { .. }
    ));
    assert_eq!(selection.report.projected_bytes, one + three);
    assert!(fixture.job(1).is_dir());
    assert_eq!(fs::read_dir(fixture.reclaim_path(1)).unwrap().count(), 0);
    drop(guard);
    assert_reclaimed(&reclaim(&selection.runs_dir));
    let later = fixture.select(PrunePolicy::zero());
    assert_eq!(later.renamed, vec![id(1), id(3)]);
}

#[test]
fn nonjobs_are_reported_and_reclaim_retries_every_reserved_prefix_entry() {
    let fixture = Fixture::new();
    let file_id = id(11);
    fs::write(fixture.runs().join(file_id.as_str()), b"not a directory").unwrap();
    fs::create_dir(fixture.runs().join("unknown")).unwrap();
    fs::write(fixture.runs().join("notes"), b"keep me").unwrap();
    let leftover = fixture.reclaim_path(1);
    fs::create_dir_all(leftover.join("nested")).unwrap();
    fs::write(leftover.join("nested/payload"), b"remove me").unwrap();
    fs::write(
        fixture.runs().join(".reclaim-interrupted"),
        b"leftover file",
    )
    .unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    assert!(selection.renamed.is_empty());
    assert_eq!(selection.report.non_job_entries.len(), 3);
    assert_eq!(selection.report.usage.bytes, 44);
    let report = reclaim(&selection.runs_dir);
    assert_reclaimed(&report);
    assert_eq!(report.reclaimed.len(), 2);
    assert!(!leftover.exists());
    assert_eq!(fs::read_dir(fixture.runs()).unwrap().count(), 3);
    assert_reclaimed(&reclaim(&selection.runs_dir));
}

#[cfg(unix)]
#[test]
fn nofollow_keeps_job_links_and_unlinks_reclaim_links_without_touching_targets() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    drop(fixture.add(1, 1));
    symlink(fixture.job(1), fixture.job(2)).unwrap();
    symlink(fixture.job(1), fixture.reclaim_path(3)).unwrap();
    // A link in governed metadata is unreadable through checked tool paths.
    symlink(
        fixture.job(1).join("job.toml"),
        fixture.job(1).join("report.json"),
    )
    .unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    assert!(selection.renamed.is_empty());
    assert_eq!(selection.report.non_job_entries, vec![fixture.job(2)]);
    assert_eq!(selection.report.unreadable_metadata.len(), 1);
    assert_reclaimed(&reclaim(&selection.runs_dir));
    assert!(fixture.job(1).join("payload").is_file());
    assert!(fixture.job(2).exists());
    assert!(fs::symlink_metadata(fixture.reclaim_path(3)).is_err());
}

#[test]
fn missing_runs_is_a_noop_and_a_non_directory_is_reported() {
    let fixture = Fixture::new();
    fs::remove_dir(fixture.runs()).unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    assert!(selection.renamed.is_empty());
    assert!(selection.report.errors.is_empty());
    assert_reclaimed(&reclaim(&selection.runs_dir));
    fs::write(fixture.runs(), b"not runs").unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    assert!(selection.renamed.is_empty());
    assert!(!selection.report.errors.is_empty());
    assert!(!reclaim(&selection.runs_dir).errors.is_empty());
    assert_eq!(fs::read(fixture.runs()).unwrap(), b"not runs");
}

#[cfg(windows)]
#[test]
fn open_file_refuses_rename_and_pruning_retries_after_close() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    drop(fixture.add(1, 100));
    let open = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fixture.job(1).join("payload"))
        .unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    assert!(selection.renamed.is_empty());
    assert_eq!(selection.report.failed_renames.len(), 1);
    assert!(fixture.job(1).is_dir());
    assert_eq!(
        selection.report.projected_bytes,
        selection.report.usage.bytes
    );
    drop(open);
    let selection = fixture.select(PrunePolicy::zero());
    assert_eq!(selection.renamed, vec![id(1)]);
    assert!(selection.report.failed_renames.is_empty());
    assert_reclaimed(&reclaim(&selection.runs_dir));
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn injected_rename_failure_preserves_source_and_projected_usage_until_retry() {
    use cpforge::storage::{LockOptions, TestHooks};
    let fixture = Fixture::new();
    drop(fixture.add(1, 100));
    let lock = fixture.lock();
    let guard = lock
        .acquire_with_options(
            "injected refusal",
            LockOptions {
                test_hooks: TestHooks {
                    fail_prune_rename: true,
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let selection = select_and_rename(&guard, PrunePolicy::zero());
    assert!(selection.renamed.is_empty());
    assert_eq!(selection.report.failed_renames.len(), 1);
    assert_eq!(
        selection.report.projected_bytes,
        selection.report.usage.bytes
    );
    assert!(fixture.job(1).is_dir());
    assert!(!fixture.reclaim_path(1).exists());
    drop(guard);
    let later = fixture.select(PrunePolicy::zero());
    assert_eq!(later.renamed, vec![id(1)]);
    assert_reclaimed(&reclaim(&later.runs_dir));
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn mount_boundary_makes_measurement_incomplete_and_removal_retains_ancestors() {
    use cpforge::storage::prune::reclaim_with_test_hooks;
    use cpforge::storage::{LockOptions, SkipReason, TestHooks};
    let fixture = Fixture::new();
    for n in 1..=2 {
        drop(fixture.add(n, 100));
    }
    let boundary = fixture.job(1).join("mount");
    fs::create_dir(&boundary).unwrap();
    fs::write(boundary.join("payload"), b"boundary data").unwrap();
    let lock = fixture.lock();
    let guard = lock
        .acquire_with_options(
            "mount pruning",
            LockOptions {
                test_hooks: TestHooks {
                    walker_mount_point: Some(boundary.clone()),
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let selection = select_and_rename(&guard, PrunePolicy::new(1, 0));
    assert_eq!(selection.renamed, vec![id(1)]);
    assert!(selection.report.incomplete_measurement);
    assert_eq!(
        selection.report.usage.skipped[0].reason,
        SkipReason::MountPoint
    );
    assert!(fixture.job(2).is_dir());
    drop(guard);
    let moved_boundary = fixture.reclaim_path(1).join("mount");
    let report = reclaim_with_test_hooks(
        &selection.runs_dir,
        &TestHooks {
            walker_mount_point: Some(moved_boundary.clone()),
            ..TestHooks::default()
        },
    );
    assert_eq!(report.walk.skipped.len(), 1);
    assert_eq!(report.walk.skipped[0].path, moved_boundary);
    assert!(report.reclaimed.is_empty());
    assert!(fixture.reclaim_path(1).is_dir());
    assert!(moved_boundary.join("payload").is_file());
    assert_reclaimed(&reclaim(&selection.runs_dir));
}

#[test]
fn concurrent_subprocess_reclaimers_both_succeed() {
    let fixture = Fixture::new();
    // Several trees and many siblings exercise disappearance before stat/open,
    // during child enumeration and while ascending through removed parents.
    for n in 1..=4 {
        let path = fixture.reclaim_path(n);
        for branch in 0..20 {
            let branch = path.join(format!("branch-{branch}/nested"));
            fs::create_dir_all(&branch).unwrap();
            for leaf in 0..10 {
                fs::write(branch.join(format!("file-{leaf}")), b"data").unwrap();
            }
        }
    }
    let mut first = Helper::start(&fixture.root, "reclaim", None);
    let mut second = Helper::start(&fixture.root, "reclaim", None);
    first.release();
    second.release();
    first.finish();
    second.finish();
    assert_eq!(fs::read_dir(fixture.runs()).unwrap().count(), 0);
}

#[test]
fn killed_owner_becomes_prunable_without_age_or_pid_checks() {
    let fixture = Fixture::new();
    // Interrupted admission: the helper takes the lease while holding the lock.
    fs::create_dir(fixture.job(1)).unwrap();
    let mut owner = Helper::start(&fixture.root, "owner", None);
    let selection = fixture.select(PrunePolicy::zero());
    assert!(selection.renamed.is_empty());
    assert_eq!(selection.report.skipped_active, vec![id(1)]);
    owner.child.kill().unwrap();
    assert!(!owner.wait().0.success());
    // Windows releases a killed process's locks asynchronously.
    let start = Instant::now();
    loop {
        match probe(fixture.job(1)) {
            ProbeOutcome::Acquired(lock) => {
                drop(lock);
                break;
            }
            ProbeOutcome::Held if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            outcome => panic!("expected inactive lease after kill: {outcome:?}"),
        }
    }
    let selection = fixture.select(PrunePolicy::zero());
    assert_eq!(selection.renamed, vec![id(1)]);
    assert_reclaimed(&reclaim(&selection.runs_dir));
    assert!(!fixture.job(1).exists());
    assert!(!fixture.reclaim_path(1).exists());
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn crashes_at_rename_boundaries_leave_intact_or_reclaimable_jobs() {
    for point in [
        "prune.before_rename",
        "prune.after_rename",
        "prune.before_rename#2",
        "prune.after_rename#2",
        "prune.before_reclaim",
    ] {
        let fixture = Fixture::new();
        for n in 1..=3 {
            drop(fixture.add(n, 7));
        }
        let mut child = Helper::start(&fixture.root, "crash", Some(point));
        child.release();
        let (status, stderr) = child.wait();
        assert!(!status.success(), "{point}");
        assert!(
            stderr.contains(&format!(
                "CPF-CRASH-AT {}",
                point.split('#').next().unwrap()
            )),
            "{stderr}"
        );
        for n in 1..=3 {
            let original = fixture.job(n);
            let renamed = fixture.reclaim_path(n);
            assert_ne!(original.exists(), renamed.exists(), "{point}: job {n}");
            let path = if original.exists() { original } else { renamed };
            assert_eq!(fs::read(path.join("payload")).unwrap(), vec![0_u8; 7]);
            assert!(path.join("job.toml").is_file());
            assert!(path.join("lease").is_file());
        }
        let later = fixture.select(PrunePolicy::zero());
        assert!(later.report.failed_renames.is_empty(), "{later:?}");
        assert_reclaimed(&reclaim(&later.runs_dir));
        assert_eq!(fs::read_dir(fixture.runs()).unwrap().count(), 0);
    }
}

struct Helper {
    child: Child,
    messages: Receiver<String>,
}

impl Helper {
    fn start(root: &Path, mode: &str, crash: Option<&str>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "prune_child", "--nocapture"])
            .env("CPF_PRUNE_ROOT", root)
            .env("CPF_PRUNE_MODE", mode)
            .env_remove("CPF_STORAGE_CRASH_AT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(crash) = crash {
            command.env("CPF_STORAGE_CRASH_AT", crash);
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
        loop {
            if helper
                .messages
                .recv_timeout(Duration::from_secs(30))
                .unwrap()
                .contains("CPF-PRUNE-READY")
            {
                break;
            }
        }
        helper
    }

    fn release(&mut self) {
        self.child.stdin.take().unwrap().write_all(b"x").unwrap();
    }

    fn wait(&mut self) -> (ExitStatus, String) {
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "helper timed out"
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
fn prune_child() {
    let root = PathBuf::from(std::env::var_os("CPF_PRUNE_ROOT").expect("helper workspace"));
    let mode = std::env::var("CPF_PRUNE_MODE").unwrap();
    let owner = if mode == "owner" {
        let lock = WorkspaceLock::new(&root).unwrap();
        let _guard = lock.acquire("child owner").unwrap();
        Some(JobLease::create_and_lock(root.join(".cpforge/runs").join(id(1).as_str())).unwrap())
    } else {
        None
    };
    #[cfg(feature = "storage-test-hooks")]
    if mode == "crash" {
        suppress_crash_artifacts();
    }
    println!("CPF-PRUNE-READY");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
    match mode.as_str() {
        "owner" => drop(owner),
        "reclaim" => assert_reclaimed(&reclaim(root.join(".cpforge/runs"))),
        #[cfg(feature = "storage-test-hooks")]
        "crash" => {
            let lock = WorkspaceLock::new(&root).unwrap();
            let guard = lock.acquire("crash prune").unwrap();
            let selection = select_and_rename(&guard, PrunePolicy::zero());
            drop(guard);
            let _ = reclaim(&selection.runs_dir);
            panic!("expected requested pruning crash point");
        }
        _ => panic!("unknown helper mode {mode}"),
    }
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

#[cfg(unix)]
#[test]
fn unlistable_run_directory_keeps_its_job() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid only reads process identity. Root bypasses these modes.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("chmod unreadability requires an unprivileged user; skipped as root");
        return;
    }
    let fixture = Fixture::new();
    for n in 1..=2 {
        drop(fixture.add(n, 0));
    }
    let run = fixture.job(1).join("run-1");
    fs::create_dir(&run).unwrap();
    fs::write(run.join("record.json"), r#"{"schema":1}"#).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o000)).unwrap();
    let selection = fixture.select(PrunePolicy::zero());
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(selection.renamed, vec![id(2)]);
    assert!(fixture.job(1).is_dir());
    assert!(!selection.report.unreadable_metadata.is_empty());
}
