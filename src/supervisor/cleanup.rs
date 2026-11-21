//! Platform-neutral admission, completion cells and the single cleanup poller.

use super::{NotStarted, RootOutcome};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use std::{fmt, io};
use tokio::sync::Notify;

const CLEANUP_SLOTS: usize = 4;
const THREAD_CREDITS: usize = 32;
static BOUNDS: OnceLock<Arc<GlobalBounds>> = OnceLock::new();

/// Process-wide thread budget, including the cleanup service's lifetime credit.
#[derive(Debug)]
pub struct ThreadCredits {
    used: Arc<AtomicUsize>,
}

impl ThreadCredits {
    pub fn try_reserve(&self, n: usize) -> Option<Credits> {
        let mut used = self.used.load(Ordering::Acquire);
        loop {
            let total = used
                .checked_add(n)
                .filter(|&total| total <= THREAD_CREDITS)?;
            match self
                .used
                .compare_exchange_weak(used, total, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => used = current,
            }
        }
        Some(Credits {
            used: self.used.clone(),
            n,
        })
    }
}

/// Returns its thread credits when dropped.
#[derive(Debug)]
pub struct Credits {
    used: Arc<AtomicUsize>,
    n: usize,
}

impl Credits {
    pub(super) fn split(mut self) -> Vec<Credits> {
        let parts = (0..self.n)
            .map(|_| Credits {
                used: self.used.clone(),
                n: 1,
            })
            .collect();
        self.n = 0;
        parts
    }
}

impl Drop for Credits {
    fn drop(&mut self) {
        self.used.fetch_sub(self.n, Ordering::AcqRel);
    }
}

/// Created by init_process; capacities are fixed for the process lifetime.
pub struct GlobalBounds {
    pub thread_credits: ThreadCredits,
    slots: [Slot; CLEANUP_SLOTS],
    slots_in_use: AtomicUsize,
    reserved_mask: AtomicUsize,
    pending: AtomicUsize,
    next_key: AtomicU64,
    wake_lock: Mutex<()>,
    wake: Condvar,
}

pub fn global_bounds() -> Option<&'static GlobalBounds> {
    BOUNDS.get().map(Arc::as_ref)
}

// Ownership protocol: admission CASes the bitmask before installing the cell;
// only that owner may access it until it publishes transferred. Thereafter only
// the service accesses it. Release clears transferred and the cell before making
// its reservation bit clear. Thus no slot mutex is ever contended, including admission.
#[derive(Debug)]
struct Slot {
    transferred: AtomicBool,
    cell: Mutex<Option<Arc<RunCell>>>,
}

struct RunCell {
    key: u64,
    result: OnceLock<Option<RootOutcome>>,
    ready: Notify,
    transfer: OnceLock<Mutex<Option<Transfer>>>,
    resources: Mutex<Option<Box<dyn Send>>>,
    retained_bytes: OnceLock<Arc<AtomicU64>>,
}

// Keep the transfer in its reserved cell: boxing here would allocate on Drop.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum Transfer {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Root(super::root::unix::Transfer),
    FailedCaptures(super::capture::FailedCaptures),
}

// Public Debug implementations must not borrow the exclusive slot/transfer
// mutexes: another thread may format a ticket while the service is polling.
impl fmt::Debug for GlobalBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlobalBounds")
            .field("slots_in_use", &self.slots_in_use.load(Ordering::Acquire))
            .field("thread_credits", &self.thread_credits)
            .field("service_pending", &self.pending.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for RunCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunCell")
            .field("key", &self.key)
            .field("complete", &self.result.get().is_some())
            .finish_non_exhaustive()
    }
}

/// One reserved cleanup slot and its preallocated transfer/result entry.
#[derive(Debug)]
pub struct Admission {
    bounds: &'static GlobalBounds,
    index: usize,
    cell: Arc<RunCell>,
    active: bool,
    worker_credits: Option<Vec<Credits>>,
}

/// Atomically reserves one entry without waiting for a lock or free capacity.
pub fn admit() -> Result<Admission, NotStarted> {
    Ok(reserve(1)?.pop().expect("one admission"))
}

/// A single CAS reserves both slots; a failed pair reserves neither.
pub fn admit_pair() -> Result<[Admission; 2], NotStarted> {
    let mut entries = reserve(2)?;
    let second = entries.pop().expect("pair");
    Ok([entries.pop().expect("pair"), second])
}

fn reserve(count: usize) -> Result<Vec<Admission>, NotStarted> {
    let bounds = global_bounds()
        .ok_or_else(|| NotStarted::Setup(io::Error::other("supervisor not initialised")))?;
    let mut old = bounds.reserved_mask.load(Ordering::Acquire);
    let mask = loop {
        let mut mask = 0usize;
        for index in 0..CLEANUP_SLOTS {
            if old & (1 << index) == 0 && mask.count_ones() < count as u32 {
                mask |= 1usize << index;
            }
        }
        if mask.count_ones() != count as u32 {
            return Err(NotStarted::CleanupCapacity);
        }
        match bounds.reserved_mask.compare_exchange_weak(
            old,
            old | mask,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break mask,
            Err(current) => old = current,
        }
    };
    let mut admissions = Vec::with_capacity(count);
    for (index, slot) in bounds.slots.iter().enumerate() {
        if mask & (1 << index) == 0 {
            continue;
        }
        let mut entry = slot.cell.try_lock().expect("exclusive slot owner");
        let cell = Arc::new(RunCell {
            key: bounds.next_key.fetch_add(1, Ordering::Relaxed),
            result: OnceLock::new(),
            ready: Notify::new(),
            transfer: OnceLock::new(),
            resources: Mutex::new(None),
            retained_bytes: OnceLock::new(),
        });
        *entry = Some(cell.clone());
        bounds.slots_in_use.fetch_add(1, Ordering::AcqRel);
        admissions.push(Admission {
            bounds,
            index,
            cell,
            active: true,
            worker_credits: None,
        });
    }
    Ok(admissions)
}

/// Reserve all file-sink credits for a pair before creating either worker set.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn reserve_pair_workers(
    admissions: &mut [Admission; 2],
    counts: [usize; 2],
) -> Result<(), NotStarted> {
    let mut credits = admissions[0]
        .bounds
        .thread_credits
        .try_reserve(counts[0] + counts[1])
        .ok_or(NotStarted::Worker)?
        .split();
    for (admission, count) in admissions.iter_mut().zip(counts) {
        admission.worker_credits = Some(
            (0..count)
                .map(|_| credits.pop().expect("pair credits reserved"))
                .collect(),
        );
    }
    Ok(())
}

impl Admission {
    /// Transfer job-owned resource guards with this admission, through failed
    /// creation and stalled captures, until physical resources are released.
    /// File sinks add each successful write to `retained_bytes`. The guards
    /// may be dropped on the cleanup polling thread, so their `Drop` must be
    /// short and must never block. Call at most once, before spawning.
    pub fn hold_resources(
        &mut self,
        resources: impl Send + 'static,
        retained_bytes: Arc<AtomicU64>,
    ) {
        self.cell
            .retained_bytes
            .set(retained_bytes)
            .expect("one byte counter per admission");
        let mut held = self
            .cell
            .resources
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(held.is_none(), "one resource owner per admission");
        *held = Some(Box::new(resources));
    }

    pub(super) fn retained_counter(&self) -> Option<Arc<AtomicU64>> {
        self.cell.retained_bytes.get().cloned()
    }

    pub(super) fn worker_credits(&mut self, count: usize) -> Result<Vec<Credits>, NotStarted> {
        if let Some(credits) = self.worker_credits.take() {
            assert_eq!(credits.len(), count);
            return Ok(credits);
        }
        self.bounds
            .thread_credits
            .try_reserve(count)
            .map(Credits::split)
            .ok_or(NotStarted::Worker)
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn key(&self) -> u64 {
        self.cell.key
    }

    /// Tickets may also be obtained before spawning. Dropping an unused
    /// admission (including failed creation) completes these tickets with None.
    pub fn ticket(&self) -> TransferTicket {
        TransferTicket {
            cell: self.cell.clone(),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn transfer(self, record: super::root::unix::Transfer) {
        debug_assert_eq!(record.key(), self.cell.key);
        self.publish_transfer(Transfer::Root(record));
    }

    pub(super) fn transfer_failed_captures(self, captures: super::capture::FailedCaptures) {
        self.publish_transfer(Transfer::FailedCaptures(captures));
    }

    fn publish_transfer(mut self, record: Transfer) {
        // Single publisher: set never contends with another initializer. The
        // poller uses get, which never waits for initialization. All storage,
        // including outcome vector capacity, already belongs to this run.
        self.bounds.pending.fetch_add(1, Ordering::AcqRel);
        self.cell
            .transfer
            .set(Mutex::new(Some(record)))
            .expect("one transfer per admission");
        self.active = false;
        self.bounds.slots[self.index]
            .transferred
            .store(true, Ordering::Release);
        self.bounds.wake.notify_one();
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if self.active {
            self.bounds.release(self.index, &self.cell, None);
        }
    }
}

/// Cloneable handle to completion, independent of the cleanup slot lifetime.
#[derive(Clone, Debug)]
pub struct TransferTicket {
    cell: Arc<RunCell>,
}

impl TransferTicket {
    /// None means normal completion or an admission that never spawned.
    /// Cancelling this future or dropping every ticket does not cancel cleanup.
    pub async fn outcome(self) -> Option<RootOutcome> {
        loop {
            let ready = self.cell.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if let Some(result) = self.cell.result.get() {
                return result.clone();
            }
            ready.await;
        }
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub slots_in_use: usize,
    pub credits_in_use: usize,
    pub service_pending: usize,
}

#[doc(hidden)]
pub fn debug_counters() -> Counters {
    global_bounds().map_or_else(Counters::default, |bounds| Counters {
        slots_in_use: bounds.slots_in_use.load(Ordering::Acquire),
        credits_in_use: bounds.thread_credits.used.load(Ordering::Acquire),
        service_pending: bounds.pending.load(Ordering::Acquire),
    })
}

pub(super) fn init() -> io::Result<()> {
    // Both process setup and job persistence can request these shared bounds.
    // Do not publish partially started bounds after a thread creation failure.
    static INIT: Mutex<()> = Mutex::new(());
    let _init = INIT.lock().unwrap_or_else(|error| error.into_inner());
    if BOUNDS.get().is_none() {
        let bounds = Arc::new(GlobalBounds {
            thread_credits: ThreadCredits {
                used: Arc::new(AtomicUsize::new(0)),
            },
            slots: std::array::from_fn(|_| Slot {
                transferred: AtomicBool::new(false),
                cell: Mutex::new(None),
            }),
            slots_in_use: AtomicUsize::new(0),
            reserved_mask: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            next_key: AtomicU64::new(0),
            wake_lock: Mutex::new(()),
            wake: Condvar::new(),
        });
        let credit = bounds
            .thread_credits
            .try_reserve(1)
            .expect("initial credit");
        let service = bounds.clone();
        std::thread::Builder::new()
            .name("cpf-cleanup".into())
            .spawn(move || {
                let _credit = credit;
                service.poll();
            })?;
        BOUNDS.set(bounds).expect("initialization serialized");
    }
    Ok(())
}

impl GlobalBounds {
    fn release(&self, index: usize, cell: &RunCell, outcome: Option<RootOutcome>) {
        let resources = cell
            .resources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        drop(resources);
        let slot = &self.slots[index];
        let mut entry = slot.cell.try_lock().expect("exclusive slot owner");
        debug_assert_eq!(entry.as_ref().map(|cell| cell.key), Some(cell.key));
        *entry = None;
        slot.transferred.store(false, Ordering::Release);
        drop(entry);
        self.slots_in_use.fetch_sub(1, Ordering::AcqRel);
        self.reserved_mask
            .fetch_and(!(1 << index), Ordering::Release);
        if cell.result.get().is_none() {
            cell.result.set(outcome).expect("one finalizer per run");
        }
        cell.ready.notify_waiters();
    }

    fn poll(&self) -> ! {
        let mut backoff = 1;
        loop {
            // Clone entries only while holding their short registry locks. The
            // transfer publisher never takes these locks, nor any service lock.
            for (index, slot) in self.slots.iter().enumerate() {
                if !slot.transferred.load(Ordering::Acquire) {
                    continue;
                }
                let cell = slot
                    .cell
                    .try_lock()
                    .expect("service owns transferred slot")
                    .clone();
                let Some(cell) = cell else { continue };
                let Some(transfer) = cell.transfer.get() else {
                    continue;
                };
                let mut transfer = transfer
                    .try_lock()
                    .expect("only the service polls transfers");
                let Some(record) = transfer.as_mut() else {
                    continue;
                };
                let released = match record {
                    Transfer::FailedCaptures(captures) => captures.resources_released(),
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    Transfer::Root(record) => {
                        let released = record.poll();
                        if let Some(outcome) = record.logical_outcome() {
                            cell.result
                                .set(Some(outcome))
                                .expect("one logical finalizer");
                            cell.ready.notify_waiters();
                        }
                        released
                    }
                };
                if released {
                    let outcome = match transfer.take().expect("poller owns record") {
                        Transfer::FailedCaptures(_) => None,
                        #[cfg(any(target_os = "linux", target_os = "macos"))]
                        Transfer::Root(mut record) => {
                            record.close_observer();
                            Some(record.outcome)
                        }
                    };
                    self.pending.fetch_sub(1, Ordering::AcqRel);
                    self.release(index, &cell, outcome);
                }
            }
            let pending = self.pending.load(Ordering::Acquire) != 0;
            let delay = if pending { backoff } else { 10 };
            let lock = self.wake_lock.lock().unwrap_or_else(|e| e.into_inner());
            // Timed idle waits also close the notify-before-wait race without
            // making a transferring destructor acquire the service mutex.
            let _ = self.wake.wait_timeout(lock, Duration::from_millis(delay));
            backoff = if pending { (backoff * 2).min(10) } else { 1 };
        }
    }
}
