use super::*;
use std::fs;
use std::sync::{Arc, Barrier};
use std::time::Duration;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "facts-{}",
                crate::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(path.join(".cpforge/facts/p")).unwrap();
        let fixture = Self(crate::storage::paths::plain(
            &fs::canonicalize(path).unwrap(),
        ));
        fixture.sequence("0123456789abcdef0123456789abcdef");
        fixture
    }
    fn sequence(&self, epoch: &str) {
        fs::write(
            self.0.join(".cpforge/job-seq"),
            format!("schema = 1\nepoch = '{epoch}'\nnext = '10'\n"),
        )
        .unwrap();
    }
    fn path(&self) -> PathBuf {
        self.0.join(".cpforge/facts/p/solution.toml")
    }
    fn publish(&self, record: &FactRecord, cutoff: &mut dyn FnMut() -> bool) -> Publication {
        publish_fact(
            &self.0,
            &Identifier::new("p").unwrap(),
            &Identifier::new("solution").unwrap(),
            record,
            cutoff,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fact(n: u64) -> FactRecord {
    FactRecord {
        epoch: Epoch::parse("0123456789abcdef0123456789abcdef").unwrap(),
        n,
        job: JobId::parse("20260101000000000-0123456789abcdef").unwrap(),
        finished_at: "2026-01-01T00:00:00.000Z".into(),
        verdict: JobVerdict::Passed,
        source: [0x12; 32],
        tests: [0xab; 32],
        capture_check: CaptureCheck::Checked,
    }
}
fn reason(outcome: &PublicationOutcome) -> &str {
    match outcome {
        PublicationOutcome::Published => "published",
        PublicationOutcome::Superseded => "superseded",
        PublicationOutcome::NotAttemptedCancelled => "cancelled",
        PublicationOutcome::NotPublished { reason, .. } => reason,
    }
}
#[test]
fn codec_validates_all_fields_after_schema_and_preserves_u64_range() {
    for n in [0, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
        for capture_check in [
            CaptureCheck::Checked,
            CaptureCheck::Unchecked {
                reasons: vec![CaptureCheckReason::ShellStage],
            },
        ] {
            let mut expected = fact(n);
            expected.capture_check = capture_check;
            assert!(
                matches!(classify_fact(&expected.encode()), Record::Valid(actual) if actual == expected)
            );
        }
    }
    assert!(matches!(
        classify_fact(&vec![b' '; super::super::RECORD_SIZE_LIMIT + 1]),
        Record::Malformed(super::super::MalformedRecord::TooLarge { .. })
    ));
    assert!(matches!(
        classify_fact(b"schema = 9\nn = 3"),
        Record::TooNew { .. }
    ));
    for raw in [
        b"".as_slice(),
        b"schema = '1'",
        b"schema = 0",
        b"schema = 1",
        b"not toml",
        b"\xff",
    ] {
        assert!(matches!(classify_fact(raw), Record::Malformed(_)));
    }
    let encoded = String::from_utf8(fact(1).encode()).unwrap();
    for field in [
        "epoch",
        "n",
        "job",
        "finished_at",
        "verdict",
        "source",
        "tests",
        "capture_check",
    ] {
        let mut document: DocumentMut = encoded.parse().unwrap();
        document.remove(field);
        assert!(
            matches!(
                classify_fact(document.to_string().as_bytes()),
                Record::Malformed(_)
            ),
            "{field}"
        );
    }
    for (key, value) in [
        ("n", "01"),
        ("n", "18446744073709551616"),
        ("epoch", "ABC"),
        ("job", "bad"),
        ("finished_at", "2026-02-30T00:00:00.000Z"),
        ("finished_at", "2026-01-01T24:00:00.000Z"),
        ("verdict", "cancelled"),
        ("source", "ABC"),
        ("tests", "abcd"),
    ] {
        let mut document: DocumentMut = encoded.parse().unwrap();
        document[key] = toml_edit::value(value);
        assert!(
            matches!(
                classify_fact(document.to_string().as_bytes()),
                Record::Malformed(_)
            ),
            "{key} {value}"
        );
    }
}
#[test]
fn every_publication_decision_row_and_read_classification() {
    let f = Fixture::new();
    assert!(matches!(read_fact(f.path()), Record::Missing));
    assert_eq!(
        reason(&f.publish(&fact(3), &mut || false).outcome),
        "published"
    );
    for (old, expected) in [(4, "superseded"), (3, "superseded"), (2, "published")] {
        let bytes = fact(old).encode();
        fs::write(f.path(), &bytes).unwrap();
        assert_eq!(
            reason(&f.publish(&fact(3), &mut || false).outcome),
            expected
        );
        if expected == "superseded" {
            assert_eq!(fs::read(f.path()).unwrap(), bytes);
        }
    }
    for bytes in [
        b"schema = 9\nn = 1".as_slice(),
        b"schema = 1\nn = '01'",
        b"malformed",
    ] {
        fs::write(f.path(), bytes).unwrap();
        let expected = if bytes.starts_with(b"schema = 9") {
            "fact_too_new"
        } else {
            "published"
        };
        assert_eq!(
            reason(&f.publish(&fact(3), &mut || false).outcome),
            expected
        );
        if expected == "fact_too_new" {
            assert_eq!(fs::read(f.path()).unwrap(), bytes);
        }
    }
    let mut old = fact(99);
    old.epoch = Epoch::parse("ffffffffffffffffffffffffffffffff").unwrap();
    fs::write(f.path(), old.encode()).unwrap();
    assert_eq!(
        reason(&f.publish(&fact(3), &mut || false).outcome),
        "published"
    );
    fs::remove_file(f.path()).unwrap();
    fs::create_dir(f.path()).unwrap();
    assert!(matches!(read_fact(f.path()), Record::Unreadable(_)));
    assert_eq!(
        reason(&f.publish(&fact(3), &mut || false).outcome),
        "fact_unreadable"
    );
    assert!(f.path().is_dir());
    fs::remove_dir(f.path()).unwrap();
    let old = fact(1).encode();
    fs::write(f.path(), &old).unwrap();
    for sequence in [
        None,
        Some("broken"),
        Some("schema = 9"),
        Some("schema = 1\nepoch = 'bad'"),
        Some("schema = 1\nepoch = 'ffffffffffffffffffffffffffffffff'\nnext = '4'"),
    ] {
        let path = f.0.join(".cpforge/job-seq");
        match sequence {
            None => fs::remove_file(path).unwrap(),
            Some(bytes) => fs::write(path, bytes).unwrap(),
        }
        let expected = if sequence.is_some_and(|s| s.contains("ffff")) {
            "epoch_changed"
        } else {
            "sequence_unavailable"
        };
        let mut calls = 0;
        let result = f.publish(&fact(3), &mut || {
            calls += 1;
            false
        });
        assert_eq!(calls, 1);
        assert_eq!(reason(&result.outcome), expected);
        assert_eq!(fs::read(f.path()).unwrap(), old);
    }
    fs::remove_file(f.0.join(".cpforge/job-seq")).unwrap();
    fs::create_dir(f.0.join(".cpforge/job-seq")).unwrap();
    assert_eq!(
        reason(&f.publish(&fact(3), &mut || false).outcome),
        "sequence_unavailable"
    );
}
#[test]
fn cancellation_cutoff_is_after_both_reads_and_has_priority_over_decision_table() {
    let f = Fixture::new();
    let old = fact(1).encode();
    fs::write(f.path(), &old).unwrap();
    fs::write(f.0.join(".cpforge/job-seq"), "schema = 99").unwrap();
    let mut calls = 0;
    assert_eq!(
        reason(
            &f.publish(&fact(3), &mut || {
                calls += 1;
                true
            })
            .outcome
        ),
        "cancelled"
    );
    assert_eq!(calls, 1);
    assert_eq!(fs::read(f.path()).unwrap(), old);
    f.sequence("0123456789abcdef0123456789abcdef");
    // Mutation inside cutoff proves both records were read before it, under
    // the lock: the classified values, rather than a second read, decide.
    assert_eq!(
        reason(
            &f.publish(&fact(3), &mut || {
                fs::write(f.0.join(".cpforge/job-seq"), "schema = 99").unwrap();
                fs::write(f.path(), "schema = 99").unwrap();
                false
            })
            .outcome
        ),
        "published"
    );
}
#[test]
fn cancellation_after_cutoff_barrier_does_not_revoke_commit() {
    let f = Fixture::new();
    fs::write(f.path(), fact(1).encode()).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let intent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let other_barrier = barrier.clone();
    let other_intent = intent.clone();
    let cancel = std::thread::spawn(move || {
        other_barrier.wait();
        other_intent.store(true, std::sync::atomic::Ordering::SeqCst);
        other_barrier.wait();
    });
    let mut calls = 0;
    let result = f.publish(&fact(3), &mut || {
        calls += 1;
        let snapshot = intent.load(std::sync::atomic::Ordering::SeqCst);
        barrier.wait();
        barrier.wait();
        snapshot
    });
    cancel.join().unwrap();
    assert_eq!(calls, 1);
    assert!(intent.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(reason(&result.outcome), "published");
    assert!(matches!(read_fact(f.path()), Record::Valid(record) if record.n == 3));
}
#[test]
fn held_workspace_lock_is_a_warning_outcome() {
    let f = Fixture::new();
    let old = fact(1).encode();
    fs::write(f.path(), &old).unwrap();
    let lock = WorkspaceLock::new(&f.0).unwrap();
    let _guard = lock.acquire("hold").unwrap();
    // The update fills the fields that exist only with the test features.
    #[allow(clippy::needless_update)]
    let options = LockOptions {
        timeout: Duration::ZERO,
        ..LockOptions::default()
    };
    let result = publish_fact_with_options(
        &f.0,
        &Identifier::new("p").unwrap(),
        &Identifier::new("solution").unwrap(),
        &fact(3),
        &mut || panic!("no cutoff without lock"),
        options,
    );
    assert_eq!(reason(&result.outcome), "lock_busy");
    assert_eq!(fs::read(f.path()).unwrap(), old);
}
#[test]
fn precommit_io_failure_preserves_old_record() {
    let f = Fixture::new();
    let old = fact(1).encode();
    fs::write(f.path(), &old).unwrap();
    let result = f.publish(&fact(3), &mut || {
        // A deterministic pre-commit path error without permission dependence.
        fs::rename(f.0.join(".cpforge/facts/p"), f.0.join(".cpforge/saved")).unwrap();
        fs::write(f.0.join(".cpforge/facts/p"), "blocked").unwrap();
        false
    });
    assert_eq!(reason(&result.outcome), "io");
    assert_eq!(
        fs::read(f.0.join(".cpforge/saved/solution.toml")).unwrap(),
        old
    );
}
#[test]
fn later_admission_finishing_first_wins_with_both_leases_held() {
    use crate::storage::admission::register_job;
    use crate::storage::prune::PrunePolicy;
    let f = Fixture::new();
    let lock = WorkspaceLock::new(&f.0).unwrap();
    let first = register_job(
        lock.acquire("first").unwrap(),
        PrunePolicy::new(20, 1 << 30),
        1 << 20,
        std::time::SystemTime::now(),
    )
    .unwrap();
    let second = register_job(
        lock.acquire("second").unwrap(),
        PrunePolicy::new(20, 1 << 30),
        1 << 20,
        std::time::SystemTime::now(),
    )
    .unwrap();
    assert!(first.sequence.1 < second.sequence.1);
    let mut a = fact(first.sequence.1);
    a.job = first.id.clone();
    let mut b = fact(second.sequence.1);
    b.job = second.id.clone();
    assert_eq!(reason(&f.publish(&b, &mut || false).outcome), "published");
    assert_eq!(reason(&f.publish(&a, &mut || false).outcome), "superseded");
    assert!(matches!(read_fact(f.path()), Record::Valid(record) if record == b));
    drop((first, second));
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn postcommit_durability_failure_is_published() {
    let f = Fixture::new();
    fs::write(f.path(), fact(1).encode()).unwrap();
    let result = publish_fact_with_options(
        &f.0,
        &Identifier::new("p").unwrap(),
        &Identifier::new("solution").unwrap(),
        &fact(3),
        &mut || false,
        LockOptions {
            test_hooks: super::super::TestHooks {
                post_commit_sync_failure: true,
                ..super::super::TestHooks::default()
            },
            ..LockOptions::default()
        },
    );
    assert_eq!(reason(&result.outcome), "published");
    assert!(result.durability_uncertain);
    assert!(matches!(read_fact(f.path()), Record::Valid(record) if record == fact(3)));
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn process_death_at_fact_commit_leaves_complete_old_or_new_record() {
    for (point, committed) in [
        ("fact.before_replace", false),
        ("atomic_replace.temp_synced", false),
        ("atomic_replace.renamed", true),
        ("fact.after_replace", true),
    ] {
        let f = Fixture::new();
        let old = fact(1).encode();
        let new = fact(3).encode();
        fs::write(f.path(), &old).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "storage::facts::tests::fact_crash_child",
                "--nocapture",
            ])
            .env("CPF_FACT_HELPER_ROOT", &f.0)
            .env("CPF_STORAGE_CRASH_AT", point)
            .env_remove("CPF_STORAGE_PAUSE_AT")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{point}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&format!("CPF-CRASH-AT {point}")),
            "{output:?}"
        );
        assert_eq!(
            fs::read(f.path()).unwrap(),
            if committed { new } else { old },
            "{point}"
        );
        assert!(matches!(read_fact(f.path()), Record::Valid(_)));
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
#[ignore = "subprocess helper for fact commit crash tests"]
fn fact_crash_child() {
    #[cfg(windows)]
    {
        // SAFETY: suppress process-local crash dialogs in the abort fixture.
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(0x0002);
        }
    }
    #[cfg(unix)]
    {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: process-local core dump limit, valid pointer and resource.
        unsafe {
            libc::setrlimit(libc::RLIMIT_CORE, &limit);
        }
    }
    let root = PathBuf::from(std::env::var_os("CPF_FACT_HELPER_ROOT").expect("helper root"));
    let result = publish_fact(
        &root,
        &Identifier::new("p").unwrap(),
        &Identifier::new("solution").unwrap(),
        &fact(3),
        &mut || false,
    );
    panic!("crash point not reached: {result:?}");
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn failed_write_and_sync_before_commit_leave_previous_fact_byte_identical() {
    for fail_write in [false, true] {
        let f = Fixture::new();
        let old = fact(1).encode();
        fs::write(f.path(), &old).unwrap();
        let result = publish_fact_with_options(
            &f.0,
            &Identifier::new("p").unwrap(),
            &Identifier::new("solution").unwrap(),
            &fact(3),
            &mut || false,
            LockOptions {
                test_hooks: super::super::TestHooks {
                    fail_write_disk_full: fail_write,
                    fail_pre_commit_sync: !fail_write,
                    ..super::super::TestHooks::default()
                },
                ..LockOptions::default()
            },
        );
        assert_eq!(reason(&result.outcome), "io");
        assert_eq!(fs::read(f.path()).unwrap(), old);
    }
}
