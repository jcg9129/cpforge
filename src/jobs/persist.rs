//! One bounded persistence worker per leased job. Deadlines govern observation,
//! not cancellation of filesystem calls. A timed-out worker keeps its resources
//! until the syscall returns and never starts another queued write.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, oneshot};

use super::engine::*;
use crate::report::wire;
use crate::storage::budget::Reservation;
use crate::storage::job::JobOwner;
use crate::storage::{self, Cleanup, Durability, Published, WorkspaceGuard};

const DEADLINE: Duration = Duration::from_secs(5);
const QUEUE: usize = 16;

#[derive(Clone, Debug)]
struct Outcome {
    reference: ArtifactRef,
    uncertain: bool,
    cleanup_pending: bool,
}
impl Outcome {
    fn failed(error: PersistError, digest: Option<[u8; 32]>) -> Self {
        Self {
            reference: ArtifactRef {
                digest,
                path: None,
                error: Some(error),
            },
            uncertain: false,
            cleanup_pending: false,
        }
    }
}

#[derive(Debug)]
struct Pending {
    execution: Option<u32>,
    run: Option<RunId>,
    artefact: &'static str,
    deadline: Instant,
    digest: Option<[u8; 32]>,
    receiver: oneshot::Receiver<Outcome>,
    result: Option<Outcome>,
}

enum Content {
    Report(Arc<ReportData>),
    Bytes(Vec<u8>),
    Record {
        record: Arc<ExecutionRecord>,
        digest: Option<[u8; 32]>,
        spec: Arc<Mutex<Option<ArtifactRef>>>,
    },
}
// Reuse storage's pre-rename seam, after the temporary file is complete and
// synced. The gate is local to this worker thread and never affects other jobs.
#[cfg(all(test, feature = "storage-test-hooks"))]
thread_local! {
    static RENAME_GATE: std::cell::RefCell<Option<Arc<RenameGate>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(all(test, feature = "storage-test-hooks"))]
#[derive(Debug, Default)]
pub(crate) struct RenameGate {
    pub(crate) reached: Notify,
    pub(crate) release: Notify,
    pub(crate) completed: Notify,
}

#[cfg(all(test, feature = "storage-test-hooks"))]
fn pause_before_rename(_: &std::path::Path) {
    let gate = RENAME_GATE.with(|cell| cell.borrow().as_ref().unwrap().clone());
    gate.reached.notify_one();
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), gate.release.notified())
                .await
                .expect("rename gate was not released");
        });
}

struct Write {
    path: PathBuf,
    content: Content,
    reservation: Reservation,
    deadline: Instant,
    sender: oneshot::Sender<Outcome>,
    digest: Option<[u8; 32]>,
    spec_result: Option<Arc<Mutex<Option<ArtifactRef>>>>,
    #[cfg(test)]
    gate: Option<Arc<Notify>>,
    #[cfg(all(test, feature = "storage-test-hooks"))]
    rename_gate: Option<Arc<RenameGate>>,
}

struct Request {
    execution: Option<u32>,
    run: Option<RunId>,
    artefact: &'static str,
    path: PathBuf,
    content: Content,
    reservation: Option<Reservation>,
    digest: Option<[u8; 32]>,
    spec_result: Option<Arc<Mutex<Option<ArtifactRef>>>>,
}

/// Construct from the same ready job passed to the engine. Construction never
/// starts a process or needs a runtime. Settlement needs Tokio's time driver.
#[derive(Debug)]
pub struct JobPersistence {
    _owner: JobOwner,
    sender: Option<mpsc::SyncSender<Write>>,
    unavailable: Arc<AtomicBool>,
    pending: Vec<Pending>,
    active: Vec<usize>,
    specs: BTreeMap<u32, Arc<Mutex<Option<ArtifactRef>>>>,
    bound: Duration,
    report_attempted: bool,
    #[cfg(test)]
    report_gate: Option<Arc<Notify>>,
    #[cfg(all(test, feature = "storage-test-hooks"))]
    rename_gate: Option<Arc<RenameGate>>,
}
impl JobPersistence {
    /// Reserves the worker's thread credit and creates the worker. Failure
    /// returns the credit; the job must not start (execution contract §3.3).
    pub fn new(ready: &super::ReadyJob<'_>) -> std::io::Result<Self> {
        Self::start(ready.owner.clone(), DEADLINE, None)
    }

    pub(crate) fn for_execution(ready: &super::ReadyExecutionJob<'_>) -> std::io::Result<Self> {
        Self::start(ready.owner.clone(), DEADLINE, None)
    }

    #[cfg(test)]
    pub(crate) fn stalled(ready: &super::ReadyJob<'_>, bound: Duration, gate: Arc<Notify>) -> Self {
        Self::start(ready.owner.clone(), bound, Some(gate)).expect("persistence worker")
    }

    #[cfg(test)]
    pub(crate) fn stalled_report(
        ready: &super::ReadyJob<'_>,
        bound: Duration,
        gate: Arc<Notify>,
    ) -> Self {
        let mut service =
            Self::start(ready.owner.clone(), bound, None).expect("persistence worker");
        service.report_gate = Some(gate);
        service
    }

    #[cfg(all(test, feature = "storage-test-hooks"))]
    pub(crate) fn stalled_rename(
        ready: &super::ReadyJob<'_>,
        bound: Duration,
        gate: Arc<RenameGate>,
    ) -> Self {
        let mut service = Self::start(ready.owner.clone(), bound, None).unwrap();
        service.rename_gate = Some(gate);
        service
    }

    fn start(owner: JobOwner, bound: Duration, gate: Option<Arc<Notify>>) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Write>(QUEUE);
        let unavailable = Arc::new(AtomicBool::new(false));
        let stopped = unavailable.clone();
        let worker_owner = owner.clone();
        let credit = crate::supervisor::persistence_credit()?;
        std::thread::Builder::new()
            .name("cpf-persistence".into())
            .spawn(move || {
                let _credit = credit;
                // A blocking test gate models an uninterruptible writer. It is
                // released by the test; production never installs one.
                if let Some(gate) = gate {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                        .expect("test gate runtime");
                    runtime.block_on(gate.notified());
                }
                while let Ok(mut write) = receiver.recv() {
                    #[cfg(test)]
                    if let Some(gate) = write.gate.take() {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_time()
                            .build()
                            .unwrap();
                        runtime.block_on(gate.notified());
                    }
                    if stopped.load(Ordering::Acquire) || Instant::now() >= write.deadline {
                        stopped.store(true, Ordering::Release);
                        break;
                    }
                    #[cfg(all(
                        feature = "storage-test-hooks",
                        any(target_os = "linux", target_os = "macos")
                    ))]
                    let milestones = match &write.content {
                        Content::Record { record, .. } => match record.as_ref() {
                            ExecutionRecord::Started(root) => root.milestones.clone(),
                            ExecutionRecord::NotStarted(_) => None,
                        },
                        _ => None,
                    };
                    let bytes = match write.content {
                        Content::Bytes(bytes) => Ok(bytes),
                        Content::Report(report) => wire::encode_report_data(&report),
                        Content::Record {
                            record,
                            digest,
                            spec,
                        } => {
                            let reference = spec
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone()
                                .unwrap_or(ArtifactRef {
                                    digest,
                                    path: None,
                                    error: Some(PersistError::WorkerUnavailable),
                                });
                            wire::encode_record(&wire::Record {
                                platform: platform(),
                                spec: &reference,
                                execution: &record,
                            })
                        }
                    };
                    let result = match bytes {
                        Ok(bytes)
                            if (bytes.len() as u64)
                                .checked_mul(2)
                                .is_some_and(|needed| needed <= write.reservation.bytes()) =>
                        {
                            let guard = WorkspaceGuard::leased(&worker_owner);
                            if let Some(parent) =
                                write.path.parent().filter(|p| !p.as_os_str().is_empty())
                            {
                                guard
                                    .created
                                    .borrow_mut()
                                    .push(worker_owner.directory().join(parent));
                            }
                            #[cfg(all(test, feature = "storage-test-hooks"))]
                            let guard = {
                                let mut guard = guard;
                                if let Some(gate) = &write.rename_gate {
                                    RENAME_GATE
                                        .with(|cell| *cell.borrow_mut() = Some(gate.clone()));
                                    guard.test_hooks.before_file_publish =
                                        Some(pause_before_rename);
                                }
                                guard
                            };
                            let published =
                                storage::publish_file_noclobber(&guard, &write.path, &bytes);
                            #[cfg(all(test, feature = "storage-test-hooks"))]
                            RENAME_GATE.with(|cell| *cell.borrow_mut() = None);
                            account(&mut write.reservation, bytes.len() as u64, &published);
                            match published {
                                Ok(published) => Outcome {
                                    reference: ArtifactRef {
                                        digest: write.digest,
                                        path: Some(write.path),
                                        error: None,
                                    },
                                    uncertain: matches!(
                                        published.durability,
                                        Durability::Uncertain(_)
                                    ),
                                    cleanup_pending: matches!(
                                        published.cleanup,
                                        Cleanup::Pending(_)
                                    ),
                                },
                                Err(error) => Outcome::failed(storage_error(&error), write.digest),
                            }
                        }
                        _ => Outcome::failed(PersistError::Io { os_error: None }, write.digest),
                    };
                    // A completion after its deadline is never a publication observation.
                    if Instant::now() >= write.deadline || stopped.load(Ordering::Acquire) {
                        stopped.store(true, Ordering::Release);
                        #[cfg(all(test, feature = "storage-test-hooks"))]
                        if let Some(gate) = &write.rename_gate {
                            gate.completed.notify_one();
                        }
                        break;
                    }
                    #[cfg(all(
                        feature = "storage-test-hooks",
                        any(target_os = "linux", target_os = "macos")
                    ))]
                    if let (Some(stamps), Some(_)) = (milestones, &result.reference.path) {
                        stamps.mark(crate::supervisor::Milestone::RecordPersisted);
                    }
                    if let Some(spec) = write.spec_result {
                        *spec.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(result.reference.clone());
                    }
                    let _ = write.sender.send(result);
                }
            })?;
        Ok(Self {
            _owner: owner,
            sender: Some(sender),
            unavailable,
            pending: Vec::new(),
            active: Vec::new(),
            specs: BTreeMap::new(),
            bound,
            report_attempted: false,
            #[cfg(test)]
            report_gate: None,
            #[cfg(all(test, feature = "storage-test-hooks"))]
            rename_gate: None,
        })
    }

    fn expire(&mut self) {
        let now = Instant::now();
        let mut timeout = false;
        for index in std::mem::take(&mut self.active) {
            let pending = &mut self.pending[index];
            if pending.result.is_some() {
                continue;
            }
            match pending.receiver.try_recv() {
                Ok(result) => pending.result = Some(result),
                Err(oneshot::error::TryRecvError::Empty) => timeout |= now >= pending.deadline,
                Err(oneshot::error::TryRecvError::Closed) => {
                    timeout |= self.unavailable.load(Ordering::Acquire) || now >= pending.deadline;
                    if !timeout {
                        pending.result = Some(Outcome::failed(
                            PersistError::WorkerUnavailable,
                            pending.digest,
                        ));
                    }
                }
            }
            if pending.result.is_none() {
                self.active.push(index);
            }
        }
        if timeout {
            self.unavailable.store(true, Ordering::Release);
            self.sender.take();
            for pending in &mut self.pending {
                if pending.result.is_none() {
                    pending.result = Some(Outcome::failed(PersistError::Timeout, pending.digest));
                }
            }
        }
    }

    fn enqueue_write(&mut self, request: Request) {
        let Request {
            execution,
            run,
            artefact,
            path,
            content,
            reservation,
            digest,
            spec_result,
        } = request;
        self.expire();
        let deadline = Instant::now() + self.bound;
        let (sender, receiver) = oneshot::channel();
        let spec_cell = spec_result.clone();
        let result = if self.unavailable.load(Ordering::Acquire) {
            Some(Outcome::failed(PersistError::WorkerUnavailable, digest))
        } else if let Some(reservation) = reservation {
            let write = Write {
                path,
                content,
                reservation,
                deadline,
                sender,
                digest,
                spec_result,
                #[cfg(test)]
                gate: if artefact == "report" {
                    self.report_gate.take()
                } else {
                    None
                },
                #[cfg(all(test, feature = "storage-test-hooks"))]
                rename_gate: self.rename_gate.take(),
            };
            match &self.sender {
                Some(sender) => match sender.try_send(write) {
                    Ok(()) => None,
                    Err(mpsc::TrySendError::Full(_)) => {
                        Some(Outcome::failed(PersistError::QueueFull, digest))
                    }
                    Err(_) => Some(Outcome::failed(PersistError::WorkerUnavailable, digest)),
                },
                None => Some(Outcome::failed(PersistError::WorkerUnavailable, digest)),
            }
        } else {
            Some(Outcome::failed(PersistError::Io { os_error: None }, digest))
        };
        match &result {
            None => self.active.push(self.pending.len()),
            // The record embeds this reference; never leave it at a default.
            Some(outcome) => {
                if let Some(cell) = spec_cell {
                    *cell.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(outcome.reference.clone());
                }
            }
        }
        self.pending.push(Pending {
            execution,
            run,
            artefact,
            deadline,
            digest,
            receiver,
            result,
        });
    }

    async fn wait(&mut self) {
        loop {
            self.expire();
            let Some(pending) = self
                .pending
                .iter_mut()
                .filter(|p| p.result.is_none())
                .min_by_key(|p| p.deadline)
            else {
                break;
            };
            let deadline = tokio::time::Instant::from_std(pending.deadline);
            // Bind first: the timer future must not borrow `pending` in the arms.
            let received = tokio::time::timeout_at(deadline, &mut pending.receiver).await;
            match received {
                Ok(Ok(result)) => pending.result = Some(result),
                Ok(Err(_)) if !self.unavailable.load(Ordering::Acquire) => {
                    pending.result = Some(Outcome::failed(
                        PersistError::WorkerUnavailable,
                        pending.digest,
                    ))
                }
                _ => {
                    self.unavailable.store(true, Ordering::Release);
                    self.sender.take();
                    for pending in &mut self.pending {
                        if pending.result.is_none() {
                            pending.result =
                                Some(Outcome::failed(PersistError::Timeout, pending.digest));
                        }
                    }
                }
            }
        }
    }
}
impl Persistence for JobPersistence {
    fn enqueue(&mut self, mut artifact: PendingArtifacts) {
        artifact.writable &= Arc::ptr_eq(&artifact.owner, &self._owner);
        let run = artifact.run.clone();
        let prefix = PathBuf::from(run.as_str());
        let digest = artifact.spec.as_ref().and_then(|spec| spec.digest);
        if !artifact.finalized {
            let cell = Arc::new(Mutex::new(None));
            self.specs.insert(artifact.execution, cell.clone());
            let Some(spec) = &artifact.spec else {
                return;
            };
            let bytes = wire::encode_spec(&spec.wire_spec());
            let allowance = if artifact.writable && bytes.is_ok() {
                artifact.metadata.take()
            } else {
                None
            };
            self.enqueue_write(Request {
                execution: Some(artifact.execution),
                run: Some(run),
                artefact: "spec",
                path: prefix.join("spec.json"),
                content: Content::Bytes(bytes.unwrap_or_default()),
                reservation: allowance,
                digest,
                spec_result: Some(cell),
            });
        } else {
            let Some(record) = artifact.record else {
                return;
            };
            let cleanup = match record.as_ref() {
                ExecutionRecord::Started(root) => format!(
                    "record finalized\ncleanup: {:?}\nkill attempts: {}\n",
                    root.termination.cleanup, root.kill_attempts
                ),
                ExecutionRecord::NotStarted(_) => "record finalized; process not started\n".into(),
            };
            let mut metadata = if artifact.writable {
                artifact.metadata.take()
            } else {
                None
            };
            let record_allowance = metadata.as_mut().and_then(|r| r.split(128 * 1024));
            let cleanup_allowance = metadata.as_mut().and_then(|r| r.split(128 * 1024));
            let cell = self
                .specs
                .get(&artifact.execution)
                .cloned()
                .unwrap_or_default();
            self.enqueue_write(Request {
                execution: Some(artifact.execution),
                run: Some(run.clone()),
                artefact: "record",
                path: prefix.join("record.json"),
                content: Content::Record {
                    record,
                    digest,
                    spec: cell,
                },
                reservation: record_allowance,
                digest: None,
                spec_result: None,
            });
            self.enqueue_write(Request {
                execution: Some(artifact.execution),
                run: Some(run),
                artefact: "cleanup_log",
                path: prefix.join("cleanup.log"),
                content: Content::Bytes(cleanup.into_bytes()),
                reservation: cleanup_allowance,
                digest: None,
                spec_result: None,
            });
        }
    }

    fn settle<'a>(
        &'a mut self,
        executions: &'a mut [ExecutionReport],
    ) -> JobFuture<'a, Vec<Warning>> {
        Box::pin(async move {
            self.wait().await;
            let mut warnings = Vec::new();
            for pending in self.pending.drain(..) {
                let outcome = pending.result.expect("settled result");
                if let Some(execution) = pending.execution.and_then(|index| {
                    executions
                        .iter_mut()
                        .find(|execution| execution.plan.index == index)
                }) {
                    match pending.artefact {
                        "spec" => execution.spec = Some(outcome.reference.clone()),
                        "record" => execution.record = Some(outcome.reference.clone()),
                        _ => {}
                    }
                }
                if let (Some(error), Some(run)) = (outcome.reference.error, pending.run) {
                    warnings.push(Warning::Persistence {
                        artefact: pending.artefact,
                        run,
                        error,
                    });
                }
                if outcome.uncertain {
                    warnings.push(Warning::Durability {
                        artefact: pending.artefact,
                        detail: "uncertain",
                    });
                }
                if outcome.cleanup_pending {
                    warnings.push(Warning::Durability {
                        artefact: pending.artefact,
                        detail: "cleanup_pending",
                    });
                }
            }
            self.active.clear();
            self.specs.clear();
            warnings
        })
    }

    fn publish_report(
        &mut self,
        report: Arc<ReportData>,
        reservation: Option<Reservation>,
    ) -> JobFuture<'_, ReportPersistence> {
        Box::pin(async move {
            if self.report_attempted {
                return failed(PersistError::Io { os_error: None });
            }
            self.report_attempted = true;
            let index = self.pending.len();
            if report.job.dir != self._owner.directory()
                || !report.job.dir.starts_with(&report.workspace)
            {
                return failed(PersistError::Io { os_error: None });
            }
            self.enqueue_write(Request {
                execution: None,
                run: None,
                artefact: "report",
                path: PathBuf::from("report.json"),
                content: Content::Report(report.clone()),
                reservation,
                digest: None,
                spec_result: None,
            });
            self.wait().await;
            let outcome = self.pending.remove(index).result.expect("report settled");
            self.active.clear();
            match outcome.reference.path {
                Some(_) => ReportPersistence::Published {
                    path: report
                        .job
                        .dir
                        .strip_prefix(&report.workspace)
                        .expect("registered job below workspace")
                        .join("report.json"),
                    uncertain: outcome.uncertain,
                    cleanup_pending: outcome.cleanup_pending,
                },
                None => failed(outcome.reference.error.expect("failed report")),
            }
        })
    }
}
fn failed(error: PersistError) -> ReportPersistence {
    ReportPersistence::Failed {
        error,
        message: "report publication failed".into(),
    }
}
pub(crate) fn platform() -> &'static str {
    if cfg!(windows) { "windows" } else { "unix" }
}
pub(crate) fn storage_error(error: &storage::StorageError) -> PersistError {
    PersistError::Io {
        os_error: match &error.kind {
            storage::ErrorKind::Io { source, .. } => source.raw_os_error(),
            _ => None,
        },
    }
}

/// Retain the exact published length, plus a leftover temp if unlink failed.
/// An error with a retained temporary continues to count that temporary.
pub(crate) fn account(
    reservation: &mut Reservation,
    len: u64,
    result: &Result<Published, storage::StorageError>,
) {
    let retained = match result {
        Ok(published) => len * (1 + u64::from(matches!(published.cleanup, Cleanup::Pending(_)))),
        Err(error) => match &error.cleanup {
            Cleanup::Pending(path) => std::fs::metadata(path).map_or(len, |m| m.len()),
            _ => 0,
        },
    };
    reservation
        .commit(retained)
        .expect("publication stays within its reserved file and temp bounds");
}

#[cfg(all(test, target_os = "linux"))]
#[path = "../../tests/support/tiny_filesystem.rs"]
mod tiny_filesystem;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::budget::JobBudget;
    use crate::storage::job::JobLease;

    fn runtime<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }
    fn fixture() -> (PathBuf, JobOwner, Arc<JobBudget>) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "persistence-{}",
                crate::build::BuildId::generate().unwrap()
            ));
        std::fs::create_dir_all(&path).unwrap();
        let budget = Arc::new(JobBudget::new(1024));
        let owner = JobLease::create_and_lock_with_budget(&path, budget.clone()).unwrap();
        (path, owner, budget)
    }
    fn request(index: usize, budget: &Arc<JobBudget>) -> Request {
        Request {
            execution: None,
            run: None,
            artefact: "spec",
            path: format!("file{index}").into(),
            content: Content::Bytes(vec![42]),
            reservation: Some(budget.reserve(2).unwrap()),
            digest: None,
            spec_result: None,
        }
    }

    #[test]
    fn full_queue_refuses_without_blocking_then_timeout_fails_all_at_once() {
        let (path, owner, budget) = fixture();
        let gate = Arc::new(Notify::new());
        let mut service =
            JobPersistence::start(owner, Duration::from_secs(1), Some(gate.clone())).unwrap();
        for i in 0..=QUEUE {
            service.enqueue_write(request(i, &budget));
        }
        assert_eq!(
            service.pending[QUEUE]
                .result
                .as_ref()
                .unwrap()
                .reference
                .error,
            Some(PersistError::QueueFull)
        );
        assert_eq!(budget.usage().reserved, 2 * QUEUE as u64);
        let start = Instant::now();
        runtime(service.wait());
        assert!(start.elapsed() < Duration::from_secs(3));
        for pending in &service.pending[..QUEUE] {
            assert_eq!(
                pending.result.as_ref().unwrap().reference.error,
                Some(PersistError::Timeout)
            );
            assert!(pending.result.as_ref().unwrap().reference.path.is_none());
        }
        service.enqueue_write(request(QUEUE + 1, &budget));
        assert_eq!(
            service
                .pending
                .last()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .reference
                .error,
            Some(PersistError::WorkerUnavailable)
        );
        // The stalled writer keeps all queued allowances until it returns.
        assert_eq!(budget.usage().reserved, 2 * QUEUE as u64);
        gate.notify_one();
        runtime(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                while budget.usage().reserved != 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        });
        drop(service);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn late_rename_keeps_timeout_and_never_fills_the_success_cell() {
        let (path, owner, budget) = fixture();
        let gate = Arc::new(RenameGate::default());
        let mut service = JobPersistence::start(owner, Duration::from_secs(1), None).unwrap();
        service.rename_gate = Some(gate.clone());
        let cell = Arc::new(Mutex::new(None));
        let mut write = request(0, &budget);
        write.spec_result = Some(cell.clone());
        let start = Instant::now();
        service.enqueue_write(write);
        runtime(async {
            tokio::time::timeout(Duration::from_secs(4), async {
                gate.reached.notified().await;
                assert!(!path.join("file0").exists());
                service.wait().await;
                let outcome = service.pending[0].result.as_ref().unwrap();
                assert_eq!(outcome.reference.error, Some(PersistError::Timeout));
                assert!(outcome.reference.path.is_none());
                assert!(!path.join("file0").exists());
                gate.release.notify_one();
                gate.completed.notified().await;
                assert_eq!(std::fs::read(path.join("file0")).unwrap(), [42]);
                // A later wait must not turn the timed-out write into success.
                service.wait().await;
                let outcome = service.pending[0].result.as_ref().unwrap();
                assert_eq!(outcome.reference.error, Some(PersistError::Timeout));
                assert!(outcome.reference.path.is_none());
                assert!(cell.lock().unwrap().is_none());
                assert!(service.unavailable.load(Ordering::Acquire));
            })
            .await
            .expect("late rename must be bounded");
        });
        assert!(start.elapsed() < Duration::from_secs(4));
        drop(service);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn missing_worker_is_unavailable_and_budget_refusal_is_io() {
        let (path, owner, budget) = fixture();
        let mut service = JobPersistence::start(owner, DEADLINE, None).unwrap();
        let mut refused = request(0, &budget);
        refused.reservation = None;
        service.enqueue_write(refused);
        assert_eq!(
            service.pending[0].result.as_ref().unwrap().reference.error,
            Some(PersistError::Io { os_error: None })
        );
        service.sender.take();
        service.enqueue_write(request(1, &budget));
        assert_eq!(
            service.pending[1].result.as_ref().unwrap().reference.error,
            Some(PersistError::WorkerUnavailable)
        );
        drop(service);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn real_enospc_keeps_failed_references_and_worker_accepts_a_later_write() {
        let Some(filesystem) = tiny_filesystem::TinyFs::one_mib() else {
            return;
        };
        let path = filesystem.directory.join("job");
        std::fs::create_dir(&path).unwrap();
        let budget = Arc::new(JobBudget::new(1024));
        let owner = JobLease::create_and_lock_with_budget(&path, budget.clone()).unwrap();
        let weak_owner = Arc::downgrade(&owner);
        let mut service = JobPersistence::start(owner, DEADLINE, None).unwrap();
        filesystem.fill_completely();
        service.enqueue_write(request(0, &budget));
        runtime(async {
            tokio::time::timeout(DEADLINE + Duration::from_millis(100), service.wait())
                .await
                .unwrap();
        });
        let failed = service.pending[0]
            .result
            .as_ref()
            .unwrap()
            .reference
            .clone();
        assert_eq!(
            failed.error,
            Some(PersistError::Io {
                os_error: Some(libc::ENOSPC)
            })
        );
        assert!(failed.path.is_none());
        assert!(!path.join("file0").exists());
        assert!(!service.unavailable.load(Ordering::Acquire));
        std::fs::remove_file(filesystem.directory.join("filler")).unwrap();
        service.enqueue_write(request(1, &budget));
        runtime(async {
            tokio::time::timeout(DEADLINE + Duration::from_millis(100), service.wait())
                .await
                .unwrap();
        });
        let published = &service.pending[1].result.as_ref().unwrap().reference;
        assert_eq!(published.error, None);
        assert_eq!(
            published.path.as_deref(),
            Some(std::path::Path::new("file1"))
        );
        assert_eq!(std::fs::read(path.join("file1")).unwrap(), [42]);
        assert_eq!(
            service.pending[0].result.as_ref().unwrap().reference.error,
            failed.error
        );
        assert_eq!(budget.usage().reserved, 0);
        drop(service);
        runtime(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while weak_owner.upgrade().is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("persistence worker did not exit");
        });
    }
}
