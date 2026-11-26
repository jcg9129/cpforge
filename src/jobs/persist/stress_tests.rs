use super::*;
use crate::jobs::engine::stress::{BatchRole, IterationArtifacts, IterationRunId};
use crate::jobs::engine::tests::run_test;
use crate::storage::budget::JobBudget;
use crate::storage::job::JobLease;

fn fixture() -> (PathBuf, JobOwner, Arc<JobBudget>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!(
            "stress-persist-{}",
            crate::build::BuildId::generate().unwrap()
        ));
    std::fs::create_dir_all(&path).unwrap();
    let budget = Arc::new(JobBudget::new(64 << 20));
    let owner = JobLease::create_and_lock_with_budget(&path, budget.clone()).unwrap();
    (path, owner, budget)
}

async fn enqueue(service: &mut JobPersistence, owner: &JobOwner, budget: &Arc<JobBudget>, k: u64) {
    for role in BatchRole::ALL {
        let id = IterationRunId { iteration: k, role };
        std::fs::create_dir_all(id.directory(owner.directory())).unwrap();
        service
            .enqueue_iteration(IterationArtifacts {
                id,
                encoded_spec: Some(b"{}".to_vec()),
                digest: Some([0x42; 32]),
                record: Arc::new(ExecutionRecord::NotStarted(StartFailure {
                    reason: crate::report::NotStartedReason::Setup,
                    api: None,
                    os_error: None,
                    invalid_limit: None,
                })),
                metadata: Some(budget.reserve(1 << 20).unwrap()),
                writable: true,
                owner: owner.clone(),
            })
            .await;
    }
}

async fn released(owner: std::sync::Weak<crate::storage::job::JobLease>, budget: &JobBudget) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while owner.upgrade().is_some() || budget.usage().retained != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("persistence lease and accounting did not release");
    assert_eq!(budget.usage().reserved, 0);
}

#[test]
fn st_l12_slow_writer_settles_each_batch_and_leaves_no_pending_cells() {
    run_test(move || async move {
        let (path, owner, budget) = fixture();
        let weak = Arc::downgrade(&owner);
        let clock = clock::Clock::controlled();
        let mut service =
            JobPersistence::start_clock(owner.clone(), DEADLINE, None, None, clock.clone())
                .unwrap();
        let (writer, mut events) = scripted::ScriptedWriter::new(false);
        *service.script.lock().unwrap() = Some(writer);
        service.configure(174, crate::supervisor::CancelToken::new());
        for k in 0..25 {
            if k >= 20 {
                assert_eq!(
                    service.iteration_debug(),
                    stress::PersistenceDebug::default()
                );
                continue; // R4 count: deliberately no hand-over to persistence.
            }
            enqueue(&mut service, &owner, &budget, k).await;
            assert!(service.iteration_debug().queued_artifacts <= 15);
            assert_eq!(service.iteration_debug().pending_cells, 15);
            let driver = async {
                for _ in 0..15 {
                    let write = events.recv().await.unwrap();
                    assert!(write.path.starts_with(PathBuf::from("iterations")));
                    clock.advance(Duration::from_millis(200));
                    write.release.send(()).unwrap();
                }
            };
            let settle = service.settle_iteration();
            let (runs, ()) = tokio::join!(settle, driver);
            assert_eq!(runs.len(), 5);
            for run in runs {
                for reference in [run.spec, run.record, run.cleanup] {
                    assert_eq!(reference.error, None);
                    assert!(path.join(reference.path.unwrap()).is_file());
                }
            }
            assert_eq!(
                service.iteration_debug(),
                stress::PersistenceDebug::default()
            );
            assert_eq!(budget.usage().reserved, 0);
        }
        drop(service);
        drop(owner);
        released(weak, &budget).await;
        std::fs::remove_dir_all(path).unwrap();
    });
}

#[test]
fn st_l18_deadlines_start_at_each_retirement_enqueue_not_record_finalization() {
    run_test(move || async move {
        let (path, owner, budget) = fixture();
        let weak = Arc::downgrade(&owner);
        let clock = clock::Clock::controlled();
        let mut service =
            JobPersistence::start_clock(owner.clone(), DEADLINE, None, None, clock.clone())
                .unwrap();
        // The first record was finalized six seconds before R5.
        clock.advance(Duration::from_secs(6));
        let r5 = clock.now();
        enqueue(&mut service, &owner, &budget, 0).await;
        assert_eq!(service.pending.len(), 15);
        assert!(service.pending.iter().all(|p| p.deadline == r5 + DEADLINE));
        let runs = service.settle_iteration().await;
        for run in runs {
            for reference in [run.spec, run.record, run.cleanup] {
                assert_eq!(reference.error, None);
                assert!(path.join(reference.path.unwrap()).is_file());
            }
        }
        assert!(!service.unavailable.load(Ordering::Acquire));
        enqueue(&mut service, &owner, &budget, 1).await;
        assert_eq!(service.settle_iteration().await.len(), 5);
        assert_eq!(
            service.iteration_debug(),
            stress::PersistenceDebug::default()
        );
        drop(service);
        drop(owner);
        released(weak, &budget).await;
        std::fs::remove_dir_all(path).unwrap();
    });
}

#[test]
fn st_l23_one_persistence_tail_survives_ten_later_refused_batches() {
    run_test(move || async move {
        let (path, owner, budget) = fixture();
        let weak = Arc::downgrade(&owner);
        let report_reservation = budget.reserve(2 << 20).unwrap();
        let clock = clock::Clock::controlled();
        let mut service =
            JobPersistence::start_clock(owner.clone(), DEADLINE, None, None, clock.clone())
                .unwrap();
        for k in 0..2 {
            enqueue(&mut service, &owner, &budget, k).await;
            assert!(
                service
                    .settle_iteration()
                    .await
                    .iter()
                    .all(|r| r.record.error.is_none())
            );
        }
        let (writer, mut events) = scripted::ScriptedWriter::new(false);
        *service.script.lock().unwrap() = Some(writer);
        enqueue(&mut service, &owner, &budget, 2).await;
        let blocked = events.recv().await.unwrap();
        assert_eq!(
            blocked.path,
            PathBuf::from("iterations")
                .join("2.generator")
                .join("spec.json")
        );
        clock.advance(DEADLINE);
        let runs = service.settle_iteration().await;
        assert_eq!(runs.len(), 5);
        for run in runs {
            for reference in [run.spec, run.record, run.cleanup] {
                assert_eq!(reference.error, Some(PersistError::Timeout));
                assert!(reference.path.is_none());
            }
        }
        let tail = service.iteration_debug();
        assert_eq!(tail.pending_cells, 0);
        assert_eq!(tail.queued_artifacts, 0);
        assert_eq!(tail.tail_artifacts, 15);
        assert_eq!(tail.tail_reserved, 5 << 20);
        let cleanup = b"record finalized; process not started\n";
        assert_eq!(
            tail.tail_charges,
            5 * (3 * queue::OVERHEAD + 2 + 64 * 1024 + cleanup.len())
        );
        assert_eq!(budget.usage().reserved, (2 + 5) << 20);
        for k in 3..13 {
            enqueue(&mut service, &owner, &budget, k).await;
            for run in service.settle_iteration().await {
                for reference in [run.spec, run.record, run.cleanup] {
                    assert_eq!(reference.error, Some(PersistError::WorkerUnavailable));
                }
            }
            assert_eq!(service.iteration_debug(), tail);
            assert_eq!(budget.usage().reserved, (2 + 5) << 20);
        }
        let retained = budget.usage().retained;
        blocked.release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !service.worker.as_mut().unwrap().resources_released() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(budget.usage().reserved, 2 << 20);
        assert_eq!(budget.usage().retained, retained + 2); // exactly one late spec
        assert_eq!(
            service.iteration_debug(),
            stress::PersistenceDebug::default()
        );
        assert!(events.try_recv().is_err()); // the other fourteen writes never ran
        assert_eq!(std::fs::read(path.join(&blocked.path)).unwrap(), b"{}");
        drop(service);
        assert_eq!(budget.usage().retained, retained + 2);
        drop(owner);
        drop(report_reservation);
        released(weak, &budget).await;
        std::fs::remove_dir_all(path).unwrap();
    });
}
