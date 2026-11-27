//! One bounded persistence worker per leased job. Deadlines govern observation,
//! not cancellation of filesystem calls. A timed-out worker keeps its resources
//! until the syscall returns and never starts another queued write. Dropping the
//! service hands a worker that may still be in a write to process-wide cleanup,
//! without taking a run slot; an idle worker simply exits as before.
mod clock;
mod queue;
#[cfg(test)]
pub(crate) mod scripted;

use clock::Clock;
use queue::{Credit, Pool, Publication};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::{Notify, oneshot};

use super::engine::*;
use crate::report::wire;
use crate::storage::budget::Reservation;
use crate::storage::job::JobOwner;
use crate::storage::{self, Cleanup, Durability, Published, WorkspaceGuard};

const DEADLINE: Duration = Duration::from_secs(5);
// Even zero-length artefacts cost 8 KiB. This channel includes one slot
// reserved exclusively for the report; memory is bounded by the credit pool.
const QUEUE_SLOTS: usize = queue::CAPACITY / queue::OVERHEAD + 1;

#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct PersistenceStats {
    pub peak_credits: usize,
    pub credit_wait: Duration,
    pub service_cycles: Vec<Duration>,
    pub last_write: Option<PathBuf>,
    pub settlement_elapsed: Option<Duration>,
    pub record_idle_at_enqueue: Vec<bool>,
}

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
#[cfg(feature = "storage-test-hooks")]
thread_local! {
    static RENAME_GATE: std::cell::RefCell<Option<Arc<RenameGate>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(feature = "storage-test-hooks")]
#[derive(Debug, Default)]
#[doc(hidden)]
pub struct RenameGate {
    pub reached: Notify,
    pub release: Notify,
    pub completed: Notify,
}

#[cfg(feature = "storage-test-hooks")]
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
    credit: Option<Credit>,
    publication: Publication,
    deadline: Instant,
    sender: oneshot::Sender<Outcome>,
    digest: Option<[u8; 32]>,
    spec_result: Option<Arc<Mutex<Option<ArtifactRef>>>>,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    held: Option<HeldArtifact>,
    #[cfg(test)]
    gate: Option<Arc<Notify>>,
    #[cfg(feature = "storage-test-hooks")]
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

#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Debug, Default)]
struct HeldArtifacts {
    count: std::sync::atomic::AtomicUsize,
    reserved: std::sync::atomic::AtomicU64,
    charges: std::sync::atomic::AtomicUsize,
}
#[cfg(any(test, feature = "storage-test-hooks"))]
#[derive(Debug)]
struct HeldArtifact {
    held: Arc<HeldArtifacts>,
    reserved: u64,
    charge: usize,
}
#[cfg(any(test, feature = "storage-test-hooks"))]
impl HeldArtifact {
    fn new(held: Arc<HeldArtifacts>, reserved: u64, charge: usize) -> Self {
        held.count.fetch_add(1, Ordering::AcqRel);
        held.reserved.fetch_add(reserved, Ordering::AcqRel);
        held.charges.fetch_add(charge, Ordering::AcqRel);
        Self {
            held,
            reserved,
            charge,
        }
    }
}
#[cfg(any(test, feature = "storage-test-hooks"))]
impl Drop for HeldArtifact {
    fn drop(&mut self) {
        self.held.count.fetch_sub(1, Ordering::AcqRel);
        self.held
            .reserved
            .fetch_sub(self.reserved, Ordering::AcqRel);
        self.held.charges.fetch_sub(self.charge, Ordering::AcqRel);
    }
}
struct LateWrite {
    reservation: Reservation,
    retained: u64,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    _held: Option<HeldArtifact>,
}

// The thread's return value owns its resources, so they outlive the thread
// itself. Queued writes in the receiver retain their metadata allowances.
struct WorkerResources {
    _credit: crate::supervisor::Credits,
    owner: JobOwner,
    receiver: mpsc::Receiver<Write>,
    late: Option<LateWrite>,
}
impl Drop for WorkerResources {
    // This may run on the cleanup polling thread (or, for a detached worker, in
    // its thread-exit path), so it must never panic or block. The measured
    // length is within the reservation (checked before publication); clamping
    // keeps that invariant without an assertion here.
    fn drop(&mut self) {
        if let Some(late) = &mut self.late {
            let retained = late.retained.min(late.reservation.bytes());
            let _ = late.reservation.commit(retained);
        }
    }
}

/// Slot-free ownership of one persistence worker thread.
#[derive(Debug)]
pub(crate) struct PersistenceWorker {
    thread: Option<JoinHandle<WorkerResources>>,
}
impl PersistenceWorker {
    /// As for transferred sink writers: join only a finished thread, so this
    /// never waits. Dropping the join result releases the credit, lease clone
    /// and metadata reservations; no publication or metadata I/O runs here.
    pub(crate) fn resources_released(&mut self) -> bool {
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.thread.take().expect("finished worker").join();
        }
        self.thread.is_none()
    }
}

/// Construct from the same ready job passed to the engine. Construction never
/// starts a process or needs a runtime. Settlement needs Tokio's time driver.
#[derive(Debug)]
pub struct JobPersistence {
    _owner: JobOwner,
    sender: Option<mpsc::SyncSender<Write>>,
    worker: Option<PersistenceWorker>,
    unavailable: Arc<AtomicBool>,
    pub(crate) pool: Arc<Pool>,
    pub(crate) clock: Clock,
    cancel: crate::supervisor::CancelToken,
    pending: Vec<Pending>,
    iteration_start: Option<usize>,
    iteration_runs: Vec<(stress::IterationRunId, Option<[u8; 32]>)>,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    held: Arc<HeldArtifacts>,
    active: Vec<usize>,
    specs: BTreeMap<u32, Arc<Mutex<Option<ArtifactRef>>>>,
    bound: Duration,
    report_attempted: bool,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    settlement_start: Option<Instant>,
    #[cfg(test)]
    report_gate: Option<Arc<Notify>>,
    #[cfg(test)]
    pub(crate) script: Arc<Mutex<Option<Arc<scripted::ScriptedWriter>>>>,
    #[cfg(feature = "storage-test-hooks")]
    rename_gate: Option<Arc<RenameGate>>,
}
impl JobPersistence {
    #[cfg(any(test, feature = "storage-test-hooks"))]
    #[doc(hidden)]
    pub fn stats(&self) -> PersistenceStats {
        let mut stats = self.pool.stats();
        stats.settlement_elapsed = self
            .settlement_start
            .map(|start| self.clock.now().saturating_duration_since(start));
        stats
    }

    #[cfg(test)]
    pub(crate) fn controlled(ready: &super::ReadyJob<'_>, gate: Arc<Notify>) -> Self {
        Self::start_clock(
            ready.owner.clone(),
            DEADLINE,
            Some(gate),
            None,
            Clock::controlled(),
        )
        .unwrap()
    }

    #[cfg(test)]
    pub(crate) fn scripted(
        ready: &super::ReadyJob<'_>,
    ) -> (Self, JobFuture<'static, ()>, Arc<scripted::ScriptedWriter>) {
        let clock = Clock::controlled();
        let service =
            Self::start_clock(ready.owner.clone(), DEADLINE, None, None, clock.clone()).unwrap();
        let (writer, mut events) = scripted::ScriptedWriter::new(true);
        *service.script.lock().unwrap() = Some(writer.clone());
        let driver = async move {
            while let Some(write) = events.recv().await {
                clock.advance(Duration::from_millis(4));
                let report = write.path == std::path::Path::new("report.json");
                let _ = write.release.send(());
                if report {
                    break;
                }
            }
        };
        (service, Box::pin(driver), writer)
    }

    #[cfg(test)]
    pub(crate) fn controlled_writer(
        ready: &super::ReadyJob<'_>,
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<scripted::WriteReturn>,
    ) {
        let service = Self::start_clock(
            ready.owner.clone(),
            DEADLINE,
            None,
            None,
            Clock::controlled(),
        )
        .unwrap();
        let (writer, events) = scripted::ScriptedWriter::new(false);
        *service.script.lock().unwrap() = Some(writer);
        (service, events)
    }

    #[cfg(test)]
    pub(crate) fn join_finished_writer(&mut self) -> bool {
        self.worker
            .as_mut()
            .is_none_or(PersistenceWorker::resources_released)
    }

    /// Reserves the worker's thread credit and creates the worker. Failure
    /// returns the credit; the job must not start (execution contract §3.3).
    pub fn new(ready: &super::ReadyJob<'_>) -> std::io::Result<Self> {
        Self::start(ready.owner.clone(), DEADLINE, None, None)
    }

    /// Uses the credit reserved together with the creation thread's, if given.
    pub(crate) fn for_execution(
        ready: &super::ReadyExecutionJob<'_>,
        credit: Option<crate::supervisor::Credits>,
    ) -> std::io::Result<Self> {
        Self::start(ready.owner.clone(), DEADLINE, None, credit)
    }

    #[cfg(any(test, feature = "storage-test-hooks"))]
    #[doc(hidden)]
    pub fn stalled(ready: &super::ReadyJob<'_>, bound: Duration, gate: Arc<Notify>) -> Self {
        Self::start(ready.owner.clone(), bound, Some(gate), None).expect("persistence worker")
    }

    #[cfg(test)]
    pub(crate) fn stalled_report(
        ready: &super::ReadyJob<'_>,
        bound: Duration,
        gate: Arc<Notify>,
    ) -> Self {
        let mut service =
            Self::start(ready.owner.clone(), bound, None, None).expect("persistence worker");
        service.report_gate = Some(gate);
        service
    }

    #[cfg(feature = "storage-test-hooks")]
    #[doc(hidden)]
    pub fn stalled_rename(
        ready: &super::ReadyJob<'_>,
        bound: Duration,
        gate: Arc<RenameGate>,
    ) -> Self {
        let mut service = Self::start(ready.owner.clone(), bound, None, None).unwrap();
        service.rename_gate = Some(gate);
        service
    }

    fn start(
        owner: JobOwner,
        bound: Duration,
        gate: Option<Arc<Notify>>,
        credit: Option<crate::supervisor::Credits>,
    ) -> std::io::Result<Self> {
        Self::start_clock(owner, bound, gate, credit, Clock::default())
    }

    fn start_clock(
        owner: JobOwner,
        bound: Duration,
        gate: Option<Arc<Notify>>,
        credit: Option<crate::supervisor::Credits>,
        clock: Clock,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Write>(QUEUE_SLOTS);
        let pool = Pool::new(clock.clone());
        let unavailable = pool.unavailable.clone();
        let worker_pool = pool.clone();
        let worker_clock = clock.clone();
        #[cfg(test)]
        let script: Arc<Mutex<Option<Arc<scripted::ScriptedWriter>>>> = Arc::default();
        #[cfg(test)]
        let worker_script = script.clone();
        let stopped = unavailable.clone();
        let credit = match credit {
            Some(credit) => credit,
            None => crate::supervisor::persistence_credit()?,
        };
        let mut resources = WorkerResources {
            _credit: credit,
            owner: owner.clone(),
            receiver,
            late: None,
        };
        let thread = std::thread::Builder::new()
            .name("cpf-persistence".into())
            .spawn(move || {
                let _close = queue::CloseOnExit(worker_pool.clone());
                // A blocking test gate models an uninterruptible writer. It is
                // released by the test; production never installs one.
                if let Some(gate) = gate {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                        .expect("test gate runtime");
                    runtime.block_on(gate.notified());
                }
                while let Ok(mut write) = resources.receiver.recv() {
                    #[cfg(test)]
                    let script = worker_script.lock().unwrap().clone();
                    #[cfg(test)]
                    if let Some(gate) = write.gate.take() {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_time()
                            .build()
                            .unwrap();
                        runtime.block_on(gate.notified());
                    }
                    if stopped.load(Ordering::Acquire) || worker_clock.now() >= write.deadline {
                        if worker_clock.now() >= write.deadline {
                            worker_pool.timeout();
                        } else {
                            worker_pool.close();
                        }
                        resources.late = Some(LateWrite {
                            reservation: write.reservation,
                            retained: 0,
                            #[cfg(any(test, feature = "storage-test-hooks"))]
                            _held: write.held.take(),
                        });
                        break;
                    }
                    #[cfg(all(
                        feature = "storage-test-hooks",
                        any(target_os = "linux", target_os = "macos")
                    ))]
                    let milestones = match &write.content {
                        Content::Record { record, .. } => match record.as_ref() {
                            ExecutionRecord::Started(root) => root.milestones.clone(),
                            ExecutionRecord::NotStarted(_) | ExecutionRecord::Transferred => None,
                        },
                        _ => None,
                    };
                    crate::storage::crash::barrier("persist.write");
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
                    let mut retained = 0;
                    let result = match bytes {
                        Ok(bytes)
                            if (bytes.len() as u64)
                                .checked_mul(2)
                                .is_some_and(|needed| needed <= write.reservation.bytes()) =>
                        {
                            let guard = WorkspaceGuard::leased(&resources.owner);
                            if let Some(parent) =
                                write.path.parent().filter(|p| !p.as_os_str().is_empty())
                            {
                                guard
                                    .created
                                    .borrow_mut()
                                    .push(resources.owner.directory().join(parent));
                            }
                            #[cfg(feature = "storage-test-hooks")]
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
                            #[cfg(test)]
                            let published =
                                if let Some(script) = script.as_ref().filter(|s| s.virtual_files) {
                                    Ok(script.publish(&write.path, &bytes))
                                } else {
                                    storage::publish_file_noclobber(&guard, &write.path, &bytes)
                                };
                            #[cfg(not(test))]
                            let published =
                                storage::publish_file_noclobber(&guard, &write.path, &bytes);
                            #[cfg(feature = "storage-test-hooks")]
                            RENAME_GATE.with(|cell| *cell.borrow_mut() = None);
                            retained = publication_bytes(bytes.len() as u64, &published);
                            match published {
                                Ok(published) => Outcome {
                                    reference: ArtifactRef {
                                        digest: write.digest,
                                        path: Some(write.path.clone()),
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
                    #[cfg(test)]
                    if let Some(script) = script {
                        script.returning(&write.path);
                    }
                    #[cfg(any(test, feature = "storage-test-hooks"))]
                    Pool::written(&worker_pool, &write.path);
                    // A completion after its deadline is never a publication observation.
                    let late = write.publication.returned();
                    // Queue headroom returns at write return, before any disk
                    // reservation settlement, including a late completion.
                    drop(write.credit.take());
                    if late {
                        resources.late = Some(LateWrite {
                            reservation: write.reservation,
                            retained,
                            #[cfg(any(test, feature = "storage-test-hooks"))]
                            _held: write.held.take(),
                        });
                        #[cfg(feature = "storage-test-hooks")]
                        if let Some(gate) = &write.rename_gate {
                            gate.completed.notify_one();
                        }
                        break;
                    }
                    write
                        .reservation
                        .commit(retained)
                        .expect("publication stays within its reserved file and temp bounds");
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
                    #[cfg(any(test, feature = "storage-test-hooks"))]
                    drop(write.held.take());
                    let _ = write.sender.send(result);
                }
                resources
            })?;
        Ok(Self {
            _owner: owner,
            sender: Some(sender),
            worker: Some(PersistenceWorker {
                thread: Some(thread),
            }),
            unavailable,
            pool,
            clock,
            cancel: crate::supervisor::CancelToken::new(),
            pending: Vec::new(),
            iteration_start: None,
            iteration_runs: Vec::new(),
            #[cfg(any(test, feature = "storage-test-hooks"))]
            held: Arc::default(),
            active: Vec::new(),
            specs: BTreeMap::new(),
            bound,
            report_attempted: false,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            settlement_start: None,
            #[cfg(test)]
            report_gate: None,
            #[cfg(test)]
            script,
            #[cfg(feature = "storage-test-hooks")]
            rename_gate: None,
        })
    }

    fn expire(&mut self) {
        let now = self.clock.now();
        let mut timeout = self.pool.timed_out.load(Ordering::Acquire);
        let mut exited = self.unavailable.load(Ordering::Acquire);
        for index in std::mem::take(&mut self.active) {
            let pending = &mut self.pending[index];
            if pending.result.is_some() {
                continue;
            }
            match pending.receiver.try_recv() {
                Ok(result) => pending.result = Some(result),
                Err(oneshot::error::TryRecvError::Empty) => timeout |= now >= pending.deadline,
                Err(oneshot::error::TryRecvError::Closed) => {
                    timeout |=
                        self.pool.timed_out.load(Ordering::Acquire) || now >= pending.deadline;
                    exited = true;
                }
            }
            if pending.result.is_none() {
                self.active.push(index);
            }
        }
        if timeout || exited {
            let error = if timeout {
                self.pool.timeout();
                PersistError::Timeout
            } else {
                self.pool.close();
                PersistError::WorkerUnavailable
            };
            self.sender.take();
            for pending in &mut self.pending {
                if pending.result.is_none() {
                    pending.result = Some(Outcome::failed(error.clone(), pending.digest));
                }
            }
            self.active.clear();
        }
    }

    fn enqueue_write(&mut self, request: Request) -> impl std::future::Future<Output = ()> + '_ {
        let call = self.clock.now();
        #[cfg(any(test, feature = "storage-test-hooks"))]
        if request.artefact == "record" {
            self.pool.record_enqueue();
        }
        self.enqueue_at(request, call)
    }

    async fn enqueue_at(&mut self, request: Request, call: Instant) {
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
        let deadline = call + self.bound;
        let (sender, receiver) = oneshot::channel();
        let spec_cell = spec_result.clone();
        let charge = match &content {
            Content::Report(_) => 0,
            Content::Bytes(bytes) => queue::OVERHEAD + bytes.len(),
            Content::Record { .. } => queue::OVERHEAD + 64 * 1024,
        };
        // Keep a sender while waiting: a grant is acceptance, even if the
        // producer resumes only after another publication has timed out. Its
        // unwritten reservation must then travel with the worker through join.
        let queue_sender = self.sender.clone();
        let credit = if queue_sender.is_none() || self.unavailable.load(Ordering::Acquire) {
            Err(PersistError::WorkerUnavailable)
        } else if charge == 0 || reservation.is_none() {
            Ok(None)
        } else {
            self.pool
                .acquire(charge, call, &self.cancel)
                .await
                .map(Some)
        };
        self.expire();
        let result = if let Err(error) = &credit {
            // A refusal is already settled; later unavailability cannot change it.
            Some(Outcome::failed(error.clone(), digest))
        } else if charge == 0 && self.unavailable.load(Ordering::Acquire) {
            Some(Outcome::failed(PersistError::WorkerUnavailable, digest))
        } else if let Some(reservation) = reservation {
            if self.clock.now() >= deadline {
                self.pool.timeout();
            }
            let settled = self.unavailable.load(Ordering::Acquire).then(|| {
                Outcome::failed(
                    if self.pool.timed_out.load(Ordering::Acquire) {
                        PersistError::Timeout
                    } else {
                        PersistError::WorkerUnavailable
                    },
                    digest,
                )
            });
            let write = Write {
                path,
                content,
                #[cfg(any(test, feature = "storage-test-hooks"))]
                held: Some(HeldArtifact::new(
                    self.held.clone(),
                    reservation.bytes(),
                    charge,
                )),
                reservation,
                credit: credit.expect("credit granted"),
                publication: self.pool.publication(deadline),
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
                #[cfg(feature = "storage-test-hooks")]
                rename_gate: self.rename_gate.take(),
            };
            match &queue_sender {
                Some(sender) => match sender.try_send(write) {
                    Ok(()) => settled,
                    Err(mpsc::TrySendError::Full(_)) => {
                        // Credits bound the ordinary queue and the extra slot
                        // is report-only, so Full indicates a broken worker.
                        self.pool.close();
                        Some(Outcome::failed(PersistError::WorkerUnavailable, digest))
                    }
                    Err(_) => {
                        self.pool.close();
                        Some(Outcome::failed(PersistError::WorkerUnavailable, digest))
                    }
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
        let pool = self.pool.clone();
        loop {
            // Register before inspecting state: a worker exit or timeout
            // signalled between the inspection and the select is not lost.
            let changed = pool.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.expire();
            let Some(pending) = self
                .pending
                .iter_mut()
                .filter(|p| p.result.is_none())
                .min_by_key(|p| p.deadline)
            else {
                break;
            };
            let deadline = pending.deadline;
            tokio::select! {
                biased;
                result = &mut pending.receiver => match result {
                    Ok(result) => pending.result = Some(result),
                    Err(_) => self.pool.close(),
                },
                _ = self.clock.until(deadline) => self.pool.timeout(),
                _ = changed => {},
            }
        }
    }
}
impl Drop for JobPersistence {
    fn drop(&mut self) {
        // Snapshot idleness before closing: the healthy thread may exit and
        // close its pool as soon as the last sender is dropped.
        let idle = !self.unavailable.load(Ordering::Acquire)
            && self.pending.iter().all(|pending| pending.result.is_some());
        self.sender.take();
        let Some(mut worker) = self.worker.take() else {
            return;
        };
        // Healthy and idle: every accepted write has delivered its result, so
        // the closed queue ends the thread without further I/O. Its join result
        // (credit, lease clone) drops on its own exit.
        if idle || worker.resources_released() {
            return;
        }
        // Timed out, stalled or still writing: the thread keeps its credit,
        // lease clone and metadata reservations under cleanup's ownership.
        crate::supervisor::transfer_persistence(worker);
    }
}
impl Persistence for JobPersistence {
    fn configure(&mut self, planned: u32, cancel: crate::supervisor::CancelToken) {
        self.pool.configure(planned);
        self.cancel = cancel;
    }

    fn enqueue(&mut self, mut artifact: PendingArtifacts) -> JobFuture<'_, ()> {
        let call = self.clock.now();
        #[cfg(any(test, feature = "storage-test-hooks"))]
        if artifact.finalized && artifact.record.is_some() {
            self.pool.record_enqueue();
        }
        Box::pin(async move {
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
                self.enqueue_at(
                    Request {
                        execution: Some(artifact.execution),
                        run: Some(run),
                        artefact: "spec",
                        path: prefix.join("spec.json"),
                        content: Content::Bytes(bytes.unwrap_or_default()),
                        reservation: allowance,
                        digest,
                        spec_result: Some(cell),
                    },
                    call,
                )
                .await;
            } else {
                let Some(record) = artifact.record else {
                    return;
                };
                let cleanup = match record.as_ref() {
                    ExecutionRecord::Started(root) => format!(
                        "record finalized\ncleanup: {:?}\nkill attempts: {}\n",
                        root.termination.cleanup, root.kill_attempts
                    ),
                    ExecutionRecord::NotStarted(_) => {
                        "record finalized; process not started\n".into()
                    }
                    ExecutionRecord::Transferred => {
                        "record finalized; cancelled during creation; cleanup pending\n".into()
                    }
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
                self.enqueue_at(
                    Request {
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
                    },
                    call,
                )
                .await;
                self.enqueue_write(Request {
                    execution: Some(artifact.execution),
                    run: Some(run),
                    artefact: "cleanup_log",
                    path: prefix.join("cleanup.log"),
                    content: Content::Bytes(cleanup.into_bytes()),
                    reservation: cleanup_allowance,
                    digest: None,
                    spec_result: None,
                })
                .await;
            }
        })
    }

    fn enqueue_iteration(&mut self, mut artifact: stress::IterationArtifacts) -> JobFuture<'_, ()> {
        Box::pin(async move {
            self.iteration_start.get_or_insert(self.pending.len());
            self.iteration_runs.push((artifact.id, artifact.digest));
            let prefix = PathBuf::from("iterations").join(artifact.id.to_string());
            let writable = artifact.writable && Arc::ptr_eq(&artifact.owner, &self._owner);
            let mut metadata = artifact.metadata.take().filter(|_| writable);
            // Retain the entire MiB until its three writes have settled. The
            // spec gets the unused headroom; record/log each need 2 x 64 KiB.
            let record_allowance = metadata.as_mut().and_then(|r| r.split(128 * 1024));
            let cleanup_allowance = metadata.as_mut().and_then(|r| r.split(128 * 1024));
            let cell = Arc::new(Mutex::new(None));
            let spec_allowance = metadata.filter(|_| artifact.encoded_spec.is_some());
            let encoded_spec = artifact.encoded_spec.take().unwrap_or_default();
            self.enqueue_write(Request {
                execution: None,
                run: None,
                artefact: "spec",
                path: prefix.join("spec.json"),
                content: Content::Bytes(encoded_spec),
                reservation: spec_allowance,
                digest: artifact.digest,
                spec_result: Some(cell.clone()),
            })
            .await;
            self.enqueue_write(Request {
                execution: None,
                run: None,
                artefact: "record",
                path: prefix.join("record.json"),
                content: Content::Record {
                    record: artifact.record.clone(),
                    digest: artifact.digest,
                    spec: cell,
                },
                reservation: record_allowance,
                digest: None,
                spec_result: None,
            })
            .await;
            let cleanup = match artifact.record.as_ref() {
                ExecutionRecord::Started(root) => format!(
                    "record finalized\ncleanup: {:?}\nkill attempts: {}\n",
                    root.termination.cleanup, root.kill_attempts
                ),
                ExecutionRecord::NotStarted(_) => "record finalized; process not started\n".into(),
                ExecutionRecord::Transferred => {
                    "record finalized; cancelled during creation; cleanup pending\n".into()
                }
            };
            self.enqueue_write(Request {
                execution: None,
                run: None,
                artefact: "cleanup_log",
                path: prefix.join("cleanup.log"),
                content: Content::Bytes(cleanup.into_bytes()),
                reservation: cleanup_allowance,
                digest: None,
                spec_result: None,
            })
            .await;
        })
    }

    fn settle_iteration(&mut self) -> JobFuture<'_, Vec<stress::SettledRun>> {
        Box::pin(async move {
            let Some(start) = self.iteration_start.take() else {
                return Vec::new();
            };
            self.wait().await;
            let mut pending = self.pending.drain(start..);
            let mut settled = Vec::with_capacity(self.iteration_runs.len());
            for (id, digest) in self.iteration_runs.drain(..) {
                let mut run = stress::SettledRun::unavailable(id, digest);
                for (index, reference) in [&mut run.spec, &mut run.record, &mut run.cleanup]
                    .into_iter()
                    .enumerate()
                {
                    if let Some(result) = pending.next().and_then(|p| p.result) {
                        *reference = result.reference;
                        run.uncertain[index] = result.uncertain;
                        run.cleanup_pending[index] = result.cleanup_pending;
                    }
                }
                settled.push(run);
            }
            self.active.retain(|index| *index < start);
            settled
        })
    }

    #[cfg(any(test, feature = "storage-test-hooks"))]
    fn iteration_debug(&self) -> stress::PersistenceDebug {
        let held = self.held.count.load(Ordering::Acquire);
        let tail = self.unavailable.load(Ordering::Acquire);
        stress::PersistenceDebug {
            pending_cells: self
                .iteration_start
                .map_or(0, |start| self.pending.len().saturating_sub(start)),
            queued_artifacts: if tail { 0 } else { held },
            tail_artifacts: if tail { held } else { 0 },
            tail_reserved: if tail {
                self.held.reserved.load(Ordering::Acquire)
            } else {
                0
            },
            tail_charges: if tail {
                self.held.charges.load(Ordering::Acquire)
            } else {
                0
            },
        }
    }

    fn settle<'a>(
        &'a mut self,
        executions: &'a mut [ExecutionReport],
    ) -> JobFuture<'a, Vec<Warning>> {
        Box::pin(async move {
            #[cfg(any(test, feature = "storage-test-hooks"))]
            {
                self.settlement_start = Some(self.clock.now());
            }
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
            // Job settlement drained every cell, a retiring iteration's too.
            self.iteration_start = None;
            self.iteration_runs.clear();
            warnings
        })
    }

    fn publish_report(
        &mut self,
        report: Arc<ReportData>,
        reservation: Option<Reservation>,
    ) -> JobFuture<'_, ReportPersistence> {
        let call = self.clock.now();
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
            self.enqueue_at(
                Request {
                    execution: None,
                    run: None,
                    artefact: "report",
                    path: PathBuf::from("report.json"),
                    content: Content::Report(report.clone()),
                    reservation,
                    digest: None,
                    spec_result: None,
                },
                call,
            )
            .await;
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
    reservation
        .commit(publication_bytes(len, result))
        .expect("publication stays within its reserved file and temp bounds");
}

// Measure retained files on the publishing thread, never cleanup's poller.
fn publication_bytes(len: u64, result: &Result<Published, storage::StorageError>) -> u64 {
    match result {
        Ok(published) => len * (1 + u64::from(matches!(published.cleanup, Cleanup::Pending(_)))),
        Err(error) => match &error.cleanup {
            Cleanup::Pending(path) => std::fs::metadata(path).map_or(len, |m| m.len()),
            _ => 0,
        },
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[path = "../../tests/support/tiny_filesystem.rs"]
mod tiny_filesystem;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::budget::JobBudget;
    use crate::storage::job::JobLease;

    pub(super) fn runtime<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(15), future)
                    .await
                    .expect("persistence fixture timed out")
            })
    }
    pub(super) fn wait_for_worker(owner: &std::sync::Weak<JobLease>, budget: &JobBudget) {
        runtime(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                // Arc reaches strong_count == 0 before JobLease::drop runs.
                // That destructor calls owner_released(), which clears retained
                // bytes; a failed Weak::upgrade alone does not observe its end.
                while owner.upgrade().is_some() || budget.usage().retained != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("cleanup did not release the persistence worker");
        });
    }
    pub(super) fn fixture() -> (PathBuf, JobOwner, Arc<JobBudget>) {
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
    pub(super) fn request(index: usize, budget: &Arc<JobBudget>) -> Request {
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

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn late_rename_keeps_timeout_and_never_fills_the_success_cell() {
        let (path, owner, budget) = fixture();
        let weak_owner = Arc::downgrade(&owner);
        let gate = Arc::new(RenameGate::default());
        let mut service = JobPersistence::start(owner, Duration::from_secs(1), None, None).unwrap();
        service.rename_gate = Some(gate.clone());
        let cell = Arc::new(Mutex::new(None));
        let mut write = request(0, &budget);
        write.spec_result = Some(cell.clone());
        let start = Instant::now();
        runtime(service.enqueue_write(write));
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
        wait_for_worker(&weak_owner, &budget);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn missing_worker_is_unavailable_and_budget_refusal_is_io() {
        let (path, owner, budget) = fixture();
        let weak_owner = Arc::downgrade(&owner);
        let mut service = JobPersistence::start(owner, DEADLINE, None, None).unwrap();
        let mut refused = request(0, &budget);
        refused.reservation = None;
        runtime(service.enqueue_write(refused));
        assert_eq!(
            service.pending[0].result.as_ref().unwrap().reference.error,
            Some(PersistError::Io { os_error: None })
        );
        service.sender.take();
        runtime(service.enqueue_write(request(1, &budget)));
        assert_eq!(
            service.pending[1].result.as_ref().unwrap().reference.error,
            Some(PersistError::WorkerUnavailable)
        );
        drop(service);
        wait_for_worker(&weak_owner, &budget);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        let mut service = JobPersistence::start(owner, DEADLINE, None, None).unwrap();
        filesystem.fill_completely();
        runtime(service.enqueue_write(request(0, &budget)));
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
        runtime(service.enqueue_write(request(1, &budget)));
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

#[cfg(test)]
mod controlled_tests {
    use super::tests::{fixture, request, runtime, wait_for_worker};
    use super::*;
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    #[test]
    fn credit_wait_counts_toward_call_origin_publication_deadline() {
        let (path, owner, budget) = fixture();
        let weak = Arc::downgrade(&owner);
        let clock = Clock::controlled();
        let mut service =
            JobPersistence::start_clock(owner, DEADLINE, None, None, clock.clone()).unwrap();
        runtime(async {
            let full = service
                .pool
                .acquire(queue::CAPACITY, clock.now(), &service.cancel)
                .await
                .unwrap();
            let call = clock.now();
            let mut enqueue = Box::pin(service.enqueue_write(request(0, &budget)));
            clock.advance(Duration::from_millis(200));
            poll_fn(|cx| {
                assert!(enqueue.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            clock.advance(Duration::from_millis(500));
            drop(full);
            enqueue.await;
            assert_eq!(service.pending[0].deadline, call + DEADLINE);
            service.wait().await;
            assert!(
                service.pending[0]
                    .result
                    .as_ref()
                    .unwrap()
                    .reference
                    .path
                    .is_some()
            );
        });
        drop(service);
        wait_for_worker(&weak, &budget);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn delayed_credit_grant_stays_accepted_and_keeps_its_reservation_until_join() {
        let (path, owner, budget) = fixture();
        let weak = Arc::downgrade(&owner);
        let clock = Clock::controlled();
        let gate = Arc::new(Notify::new());
        let mut service =
            JobPersistence::start_clock(owner, DEADLINE, Some(gate.clone()), None, clock.clone())
                .unwrap();
        runtime(async {
            let full = service
                .pool
                .acquire(queue::CAPACITY, clock.now(), &service.cancel)
                .await
                .unwrap();
            let mut enqueue = Box::pin(service.enqueue_write(request(0, &budget)));
            poll_fn(|cx| {
                assert!(enqueue.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            clock.advance(Duration::from_millis(500));
            drop(full); // accepted here, while its producer is descheduled
            clock.advance(Duration::from_millis(4500));
            enqueue.await;
            assert_eq!(
                service.pending[0].result.as_ref().unwrap().reference.error,
                Some(PersistError::Timeout)
            );
            assert_eq!(budget.usage().reserved, 2);
            assert!(!path.join("file0").exists());
            drop(service);
            assert_eq!(budget.usage().reserved, 2);
            gate.notify_one();
            tokio::time::timeout(Duration::from_secs(5), async {
                while budget.usage().reserved != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
        wait_for_worker(&weak, &budget);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn refusal_before_a_publication_timeout_remains_queue_full_when_polled_late() {
        let (path, owner, budget) = fixture();
        let weak = Arc::downgrade(&owner);
        let clock = Clock::controlled();
        let gate = Arc::new(Notify::new());
        let mut service =
            JobPersistence::start_clock(owner, DEADLINE, Some(gate.clone()), None, clock.clone())
                .unwrap();
        runtime(async {
            service.enqueue_write(request(0, &budget)).await;
            let full = service
                .pool
                .acquire(
                    queue::CAPACITY - queue::OVERHEAD - 1,
                    clock.now(),
                    &service.cancel,
                )
                .await
                .unwrap();
            let mut enqueue = Box::pin(service.enqueue_write(request(1, &budget)));
            poll_fn(|cx| {
                assert!(enqueue.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            clock.advance(Duration::from_secs(6));
            enqueue.await;
            assert_eq!(
                service.pending[0].result.as_ref().unwrap().reference.error,
                Some(PersistError::Timeout)
            );
            assert_eq!(
                service.pending[1].result.as_ref().unwrap().reference.error,
                Some(PersistError::QueueFull)
            );
            assert_eq!(service.stats().credit_wait, Duration::from_secs(1));
            assert_eq!(budget.usage().reserved, 2);
            drop(service);
            drop(full);
            gate.notify_one();
        });
        wait_for_worker(&weak, &budget);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn normal_return_commits_on_worker_and_releases_credits_at_return() {
        let (path, owner, budget) = fixture();
        let weak = Arc::downgrade(&owner);
        let clock = Clock::controlled();
        let mut service =
            JobPersistence::start_clock(owner.clone(), DEADLINE, None, None, clock.clone())
                .unwrap();
        let (writer, mut events) = scripted::ScriptedWriter::new(false);
        *service.script.lock().unwrap() = Some(writer);
        runtime(async {
            service.enqueue_write(request(0, &budget)).await;
            let write = events.recv().await.unwrap();
            assert_eq!(write.path, PathBuf::from("file0"));
            assert_eq!(budget.usage().retained, 0);
            assert_eq!(budget.usage().reserved, 2);
            clock.advance(DEADLINE - Duration::from_millis(1));
            write.release.send(()).unwrap();
            service.wait().await;
            assert_eq!(budget.usage().retained, 1);
            assert_eq!(budget.usage().reserved, 0);
            assert_eq!(service.pool.stats().peak_credits, queue::OVERHEAD + 1);
            assert_eq!(std::fs::read(path.join("file0")).unwrap(), [42]);
        });
        drop(service);
        assert_eq!(budget.usage().retained, 1);
        drop(owner);
        wait_for_worker(&weak, &budget);
        assert_eq!(budget.usage().retained, 0);
        std::fs::remove_dir_all(path).unwrap();
    }

    fn late_return(hand_over: bool) {
        let (path, owner, budget) = fixture();
        let weak = Arc::downgrade(&owner);
        let clock = Clock::controlled();
        let mut service =
            JobPersistence::start_clock(owner.clone(), DEADLINE, None, None, clock.clone())
                .unwrap();
        let pool = service.pool.clone();
        let (writer, mut events) = scripted::ScriptedWriter::new(false);
        *service.script.lock().unwrap() = Some(writer);
        runtime(async {
            service.enqueue_write(request(0, &budget)).await;
            let write = events.recv().await.unwrap();
            service.enqueue_write(request(1, &budget)).await;
            clock.advance(DEADLINE);
            service.wait().await;
            for pending in &service.pending {
                assert_eq!(
                    pending.result.as_ref().unwrap().reference.error,
                    Some(PersistError::Timeout)
                );
            }
            assert_eq!(budget.usage().reserved, 4);
            assert_eq!(budget.usage().retained, 0);
            if hand_over {
                drop(service);
                write.release.send(()).unwrap();
                // Cleanup joins only after return, commits the late measured
                // byte and releases the queued, unwritten reservation.
                tokio::time::timeout(Duration::from_secs(5), async {
                    while budget.usage().reserved != 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            } else {
                let mut worker = service.worker.take().unwrap();
                drop(service);
                write.release.send(()).unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !worker.thread.as_ref().unwrap().is_finished() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(
                    budget.usage().retained,
                    0,
                    "worker must not commit a late return"
                );
                assert_eq!(budget.usage().reserved, 4);
                assert!(worker.resources_released());
                assert!(worker.resources_released(), "joining twice is harmless");
            }
            assert_eq!(budget.usage().retained, 1);
            assert_eq!(budget.usage().reserved, 0);
            assert!(pool.unavailable.load(Ordering::Acquire));
            assert_eq!(std::fs::read(path.join("file0")).unwrap(), [42]);
            assert!(!path.join("file1").exists());
            assert_eq!(pool.stats().peak_credits, 2 * (queue::OVERHEAD + 1));
            let error = pool
                .acquire(
                    queue::OVERHEAD,
                    clock.now(),
                    &crate::supervisor::CancelToken::new(),
                )
                .await
                .unwrap_err();
            assert_eq!(error, PersistError::WorkerUnavailable);
        });
        drop(owner);
        wait_for_worker(&weak, &budget);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn late_return_is_committed_once_by_job_join_owner() {
        late_return(false);
    }
    #[test]
    fn late_handover_is_committed_once_by_cleanup_join_owner() {
        late_return(true);
    }
}

#[cfg(test)]
#[path = "persist/stress_tests.rs"]
mod stress_tests;
