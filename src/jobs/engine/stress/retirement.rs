//! Retirement owns one iteration's evidence, never a history of iterations.
//! R1-R4 are performed by the execution adapter and `begin_retirement`; this
//! owner implements R5-R11 while retaining that same admission lease.
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{AdmissionError, Disposition, IterationRunId, RetiringIteration, SaveResult};
use crate::jobs::engine::{ArtifactRef, ExecutionRecord, JobFuture, PersistError, Persistence};
use crate::storage::budget::{JobBudget, Reservation};
use crate::storage::job::JobOwner;

const RETIREMENT_DEADLINE: Duration = Duration::from_secs(5);
const MAX_RUNS: usize = 8;
const PERSISTENCE_EXECUTIONS: u32 = 6 + 8 * 21;
const METADATA_FILES: [&str; 3] = ["spec.json", "record.json", "cleanup.log"];

/// Named observation boundaries, immediately before the corresponding step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetirementStep {
    Enqueue,
    ResourcesReleased,
    Promote,
    Settle,
    Evict,
    Remove,
    Release,
}

#[derive(Debug, Default)]
struct ReleaseState {
    dropped: AtomicBool,
    committed: AtomicU64,
    ticket: Mutex<Option<crate::supervisor::TransferTicket>>,
    changed: tokio::sync::Notify,
}

/// Observe physical release independently of record finalization. The guard
/// stays with the executor's sinks and writers, including transferred creation.
#[derive(Clone, Debug, Default)]
pub struct RunRelease(Arc<ReleaseState>);

impl RunRelease {
    /// A run that never received resources (a refused admission): nothing is
    /// held, so R6 does not wait for it and R11 has nothing of it to release.
    pub fn not_held() -> Self {
        let release = Self::default();
        release.released(0);
        release
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn bind(&self, ticket: crate::supervisor::TransferTicket) {
        *self.0.ticket.lock().unwrap_or_else(|e| e.into_inner()) = Some(ticket);
    }

    /// `committed` is what the run's stream reservation converted to retained
    /// bytes; R11 never releases more than that for the run.
    pub(crate) fn released(&self, committed: u64) {
        self.0.committed.store(committed, Ordering::Release);
        self.0.dropped.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }

    /// R7 moved these bytes out of the run directory. They stay charged to
    /// staging until promotion; R11 must not release them a second time.
    pub fn moved(&self, bytes: u64) -> Result<(), AdmissionError> {
        if !self.0.dropped.load(Ordering::Acquire) {
            return Err(AdmissionError::State);
        }
        // A compare-exchange loop: `fetch_update` is deprecated on current
        // toolchains, and its replacement does not exist at the MSRV.
        let committed = &self.0.committed;
        let mut current = committed.load(Ordering::Acquire);
        loop {
            let next = current.checked_sub(bytes).ok_or(AdmissionError::State)?;
            match committed.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }

    fn committed(&self) -> u64 {
        self.0.committed.load(Ordering::Acquire)
    }

    pub async fn wait(&self) {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.0.dropped.load(Ordering::Acquire) {
                break;
            }
            changed.await;
        }
        // The supervisor drops the resource guard just before returning the
        // slot. Its ticket is the barrier for the slot and credits as well.
        let ticket = self
            .0
            .ticket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(ticket) = ticket {
            let _ = ticket.outcome().await;
        }
    }
}

/// A finalized run held next to the admission lease. No persistence ownership
/// exists until R5. `encoded_spec` is the exact <=256 KiB admission encoding;
/// absence records a spec-size refusal, rather than encoding it again at R5.
#[derive(Debug)]
pub struct RetirementRun {
    pub id: IterationRunId,
    pub encoded_spec: Option<Vec<u8>>,
    pub digest: Option<[u8; 32]>,
    pub record: Arc<ExecutionRecord>,
    pub metadata: Option<Reservation>,
    pub writable: bool,
    /// `RunResources::track_release`, or `RunRelease::not_held` for a run
    /// whose admission was refused before it received resources.
    pub release: RunRelease,
    /// The run's stream bytes (`RunResources::retained_counter`). Writers
    /// finish updating it before `release` becomes ready; R11 releases at most
    /// what the run's reservation committed.
    pub retained_bytes: Arc<AtomicU64>,
    /// Staging uses independent reservations; the stream counter excludes it.
    pub staging_release: RunRelease,
}

/// Ownership handed to the job's one persistence worker in R5.
#[derive(Debug)]
pub struct IterationArtifacts {
    pub id: IterationRunId,
    pub encoded_spec: Option<Vec<u8>>,
    pub digest: Option<[u8; 32]>,
    pub record: Arc<ExecutionRecord>,
    pub metadata: Option<Reservation>,
    pub writable: bool,
    pub owner: JobOwner,
}

#[derive(Clone, Debug)]
pub struct SettledRun {
    pub id: IterationRunId,
    pub spec: ArtifactRef,
    pub record: ArtifactRef,
    pub cleanup: ArtifactRef,
    /// Publication warnings in spec/record/cleanup order, for the report adapter.
    pub uncertain: [bool; 3],
    pub cleanup_pending: [bool; 3],
}

impl SettledRun {
    pub fn unavailable(id: IterationRunId, digest: Option<[u8; 32]>) -> Self {
        let reference = || ArtifactRef {
            digest: None,
            path: None,
            error: Some(PersistError::WorkerUnavailable),
        };
        Self {
            id,
            spec: ArtifactRef {
                digest,
                ..reference()
            },
            record: reference(),
            cleanup: reference(),
            uncertain: [false; 3],
            cleanup_pending: [false; 3],
        }
    }
}

/// Bounded evidence for the report adapter. Paths are job-relative. The
/// saturation rule for dropped paths is the report's u32 list-drop rule.
#[derive(Clone, Debug, Default)]
pub struct CleanupFailure {
    pub paths: Vec<PathBuf>,
    pub paths_dropped: u32,
}

impl CleanupFailure {
    fn push(&mut self, path: PathBuf) {
        if self.paths.len() < 16 {
            self.paths.push(path);
        } else {
            self.paths_dropped = self.paths_dropped.saturating_add(1);
        }
    }

    fn failed(&self) -> bool {
        !self.paths.is_empty() || self.paths_dropped != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotSavedReason {
    ResourcesHeld,
    Io,
}

/// Saved-failure staging/promotion plugs into this hook. A hook must release accounting
/// for promoted files and subtract moved streams from each run's byte counter;
/// moved judge files must also be deducted with `staging_release.moved`.
/// Cancellation never interrupts a promotion already in progress.
pub trait RetirementHooks {
    fn promote<'a>(&'a mut self, _runs: &'a [RetirementRun]) -> JobFuture<'a, SaveOutcome> {
        Box::pin(async { SaveOutcome::NotSaved(NotSavedReason::Io) })
    }

    /// Used by injected cleanup failures; normal removal uses the no-follow walker.
    fn remove(
        &mut self,
        root: &std::path::Path,
        path: &std::path::Path,
    ) -> Result<crate::storage::WalkReport, crate::storage::StorageError> {
        crate::storage::remove_owned_tree(root, path)
    }
}

#[derive(Debug, Default)]
pub struct NoRetirementHooks;
impl RetirementHooks for NoRetirementHooks {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveOutcome {
    NotSaved(NotSavedReason),
    Staged,
    Promoted,
}

impl SaveOutcome {
    fn result(self) -> SaveResult {
        match self {
            Self::NotSaved(_) => SaveResult::NotSaved,
            Self::Staged => SaveResult::Staged,
            Self::Promoted => SaveResult::Promoted,
        }
    }
}

#[derive(Debug)]
pub struct RetirementResult {
    pub saved: Option<SaveOutcome>,
    pub cleanup_failed: CleanupFailure,
    /// Only save attempts return record refs. Other evidence has been evicted.
    pub records: Vec<SettledRun>,
}

#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PersistenceDebug {
    pub pending_cells: usize,
    pub queued_artifacts: usize,
    pub tail_artifacts: usize,
    pub tail_reserved: u64,
    pub tail_charges: usize,
}

#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetirementDebug {
    pub live_iterations: usize,
    pub records: usize,
    pub specs: usize,
    pub metadata_reserved: u64,
    pub persistence: PersistenceDebug,
}

#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Clone, Debug, Default)]
pub struct RetirementProbe(Arc<Mutex<RetirementDebug>>);

#[cfg(any(test, feature = "storage-test-hooks"))]
impl RetirementProbe {
    pub fn snapshot(&self, persistence: &dyn Persistence) -> RetirementDebug {
        let mut snapshot = *self.0.lock().unwrap_or_else(|e| e.into_inner());
        snapshot.persistence = persistence.iteration_debug();
        snapshot
    }
}

/// Contains only the live iteration. Construct at R1, with the instant its
/// last record finalized, so R5 credit waits count against the R6 deadline.
#[derive(Debug)]
pub struct RetirementBatch {
    runs: Vec<RetirementRun>,
    finalized_at: tokio::time::Instant,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    probe: RetirementProbe,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    observer: Option<crate::jobs::engine::Observation>,
}

impl RetirementBatch {
    pub fn new(
        runs: Vec<RetirementRun>,
        finalized_at: tokio::time::Instant,
    ) -> Result<Self, AdmissionError> {
        if runs.len() > MAX_RUNS
            || runs.windows(2).any(|pair| {
                pair[0].id.iteration != pair[1].id.iteration
                    || pair[0].id.role.index() >= pair[1].id.role.index()
            })
            || runs.iter().any(|run| {
                run.encoded_spec
                    .as_ref()
                    .is_some_and(|s| s.len() > 256 * 1024)
                    || run
                        .metadata
                        .as_ref()
                        .is_some_and(|r| r.bytes() > 1024 * 1024)
            })
        {
            return Err(AdmissionError::State);
        }
        #[cfg(any(test, feature = "storage-test-hooks"))]
        let probe = RetirementProbe(Arc::new(Mutex::new(RetirementDebug {
            live_iterations: 1,
            records: runs.len(),
            specs: runs.iter().filter(|r| r.encoded_spec.is_some()).count(),
            metadata_reserved: runs
                .iter()
                .filter_map(|r| r.metadata.as_ref())
                .map(Reservation::bytes)
                .sum(),
            persistence: PersistenceDebug::default(),
        })));
        Ok(Self {
            #[cfg(any(test, feature = "storage-test-hooks"))]
            probe,
            runs,
            finalized_at,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            observer: None,
        })
    }

    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub fn observe(
        mut self,
        observer: impl Fn(crate::jobs::engine::Point) + Send + Sync + 'static,
    ) -> Self {
        self.observer = Some(crate::jobs::engine::Observation(Arc::new(observer)));
        self
    }

    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub fn debug(&self, persistence: &dyn Persistence) -> RetirementDebug {
        self.probe.snapshot(persistence)
    }

    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub fn probe(&self) -> RetirementProbe {
        self.probe.clone()
    }

    #[cfg(any(test, feature = "storage-test-hooks"))]
    fn point(&self, k: u64, step: RetirementStep) {
        if let Some(observer) = &self.observer {
            (observer.0)(crate::jobs::engine::Point::StressRetire { iteration: k, step });
        }
    }

    pub async fn retire<T>(
        mut self,
        lease: RetiringIteration<'_, T>,
        owner: JobOwner,
        budget: &JobBudget,
        persistence: &mut dyn Persistence,
        hooks: &mut dyn RetirementHooks,
    ) -> Result<RetirementResult, AdmissionError> {
        let k = lease.iteration();
        if self.runs.iter().any(|run| run.id.iteration != k) {
            return Err(AdmissionError::State);
        }
        let disposition = lease.disposition();
        // Configure the fixed stress credit allowance and the shared intent.
        // The worker preserves cumulative credit waiting across retirements.
        persistence.configure(PERSISTENCE_EXECUTIONS, lease.owner.cancel.token());
        #[cfg(any(test, feature = "storage-test-hooks"))]
        self.point(k, RetirementStep::Enqueue);
        // R5: keep the admission encoding until R9; persistence gets its own
        // bounded copy. Metadata moves once, or is released unused here.
        for run in &mut self.runs {
            let metadata = run.metadata.take();
            if matches!(disposition, Disposition::Save | Disposition::Keep) {
                persistence
                    .enqueue_iteration(IterationArtifacts {
                        id: run.id,
                        encoded_spec: run.encoded_spec.clone(),
                        digest: run.digest,
                        record: run.record.clone(),
                        metadata,
                        writable: run.writable,
                        owner: owner.clone(),
                    })
                    .await;
            }
        }
        #[cfg(any(test, feature = "storage-test-hooks"))]
        {
            self.probe
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .metadata_reserved = 0;
            self.point(k, RetirementStep::ResourcesReleased);
        }
        let deadline = self.finalized_at + RETIREMENT_DEADLINE;
        let mut cleanup_failed = CleanupFailure::default();
        // R6: one absolute deadline for every holder, not five seconds each.
        for run in &self.runs {
            if tokio::time::timeout_at(deadline, async {
                run.release.wait().await;
                run.staging_release.wait().await;
            })
            .await
            .is_err()
            {
                cleanup_failed.push(PathBuf::from("iterations").join(run.id.to_string()));
            }
        }
        let resources_held = cleanup_failed.failed();
        #[cfg(any(test, feature = "storage-test-hooks"))]
        self.point(k, RetirementStep::Promote);
        let saved = if disposition == Disposition::Save {
            Some(if resources_held {
                SaveOutcome::NotSaved(NotSavedReason::ResourcesHeld)
            } else {
                #[cfg(any(test, feature = "storage-test-hooks"))]
                if let Some(observer) = &self.observer {
                    (observer.0)(crate::jobs::engine::Point::StressPromote(k));
                }
                hooks.promote(&self.runs).await
            })
        } else {
            None
        };
        #[cfg(any(test, feature = "storage-test-hooks"))]
        self.point(k, RetirementStep::Settle);
        let mut settled = if matches!(disposition, Disposition::Save | Disposition::Keep) {
            persistence.settle_iteration().await
        } else {
            Vec::new()
        };
        if disposition == Disposition::Save {
            for run in &self.runs {
                if !settled.iter().any(|result| result.id == run.id) {
                    settled.push(SettledRun::unavailable(run.id, run.digest));
                }
            }
        }
        // Snapshot only bounded cleanup/accounting handles, never evidence.
        let scratch: Vec<_> = self
            .runs
            .iter()
            .filter(|r| r.writable)
            .map(|r| {
                (
                    r.id,
                    r.retained_bytes.clone(),
                    r.release.clone(),
                    r.staging_release.clone(),
                )
            })
            .collect();
        #[cfg(any(test, feature = "storage-test-hooks"))]
        self.point(k, RetirementStep::Evict);
        self.runs.clear(); // R9, before the removal callback can observe us.
        #[cfg(any(test, feature = "storage-test-hooks"))]
        {
            let mut debug = self.probe.0.lock().unwrap_or_else(|e| e.into_inner());
            debug.records = 0;
            debug.specs = 0;
        }
        let records = if disposition == Disposition::Save {
            settled
        } else {
            Vec::new()
        };
        #[cfg(any(test, feature = "storage-test-hooks"))]
        self.point(k, RetirementStep::Remove);
        let mut confirmed_bytes = 0u64;
        if !resources_held && disposition != Disposition::Keep {
            for (id, retained, release, staging) in scratch {
                let directory = id.directory(owner.directory());
                #[cfg(any(test, feature = "storage-test-hooks"))]
                if let Some(observer) = &self.observer {
                    (observer.0)(crate::jobs::engine::Point::StressRemove(id));
                }
                let mut failed = false;
                let paths = if disposition == Disposition::Save {
                    match fs::read_dir(&directory) {
                        Ok(entries) => {
                            let mut paths = Vec::new();
                            for entry in entries {
                                match entry {
                                    // A temporary file there belongs to a
                                    // persistence write still in progress (the
                                    // tail); its bytes are that write's.
                                    Ok(entry)
                                        if METADATA_FILES
                                            .iter()
                                            .any(|name| entry.file_name() == *name)
                                            || entry
                                                .file_name()
                                                .to_str()
                                                .is_some_and(crate::storage::exact_temp_name) => {}
                                    Ok(entry) => paths.push(entry.path()),
                                    Err(_) => {
                                        failed = true;
                                        cleanup_failed
                                            .push(PathBuf::from("iterations").join(id.to_string()));
                                    }
                                }
                            }
                            paths
                        }
                        Err(_) => {
                            failed = true;
                            cleanup_failed.push(PathBuf::from("iterations").join(id.to_string()));
                            Vec::new()
                        }
                    }
                } else {
                    vec![directory]
                };
                for path in paths {
                    match hooks.remove(owner.directory(), &path) {
                        Ok(report) => {
                            for (path, _) in report.errors {
                                failed = true;
                                cleanup_failed.push(relative(&owner, path));
                            }
                            for skipped in report.skipped {
                                failed = true;
                                cleanup_failed.push(relative(&owner, skipped.path));
                            }
                        }
                        Err(_) => {
                            failed = true;
                            cleanup_failed.push(relative(&owner, path));
                        }
                    }
                }
                if !failed {
                    // R11: child-created files are outside the budget, so the
                    // walker's byte total is deliberately not the release size,
                    // and nothing beyond the reservation's commit is released.
                    let bytes = retained
                        .load(Ordering::Acquire)
                        .min(release.committed())
                        .checked_add(staging.committed())
                        .ok_or(AdmissionError::State)?;
                    confirmed_bytes = confirmed_bytes
                        .checked_add(bytes)
                        .ok_or(AdmissionError::State)?;
                }
            }
        }
        #[cfg(any(test, feature = "storage-test-hooks"))]
        self.point(k, RetirementStep::Release);
        budget
            .release_retained(confirmed_bytes)
            .map_err(|_| AdmissionError::State)?;
        lease.complete(saved.map(SaveOutcome::result), cleanup_failed.failed())?;
        #[cfg(any(test, feature = "storage-test-hooks"))]
        {
            self.probe
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .live_iterations = 0;
        }
        Ok(RetirementResult {
            saved,
            cleanup_failed,
            records,
        })
    }
}

fn relative(owner: &JobOwner, path: PathBuf) -> PathBuf {
    path.strip_prefix(owner.directory())
        .map_or_else(|_| path.clone(), std::path::Path::to_path_buf)
}
