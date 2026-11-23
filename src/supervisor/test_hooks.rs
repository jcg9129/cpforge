//! Deterministic supervisor setup failures, absent from production builds.

use std::cell::Cell;
use std::io;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

/// Pause before/after commit, inside the OS creation call, or before observer
/// registration. The pre_exec handshake holds no registry lock.
#[derive(Debug, Default)]
pub struct CreationGate {
    after_commit: bool,
    during: Option<super::sys::CreationPause>,
    pid: AtomicU32,
    entered: super::CancelToken,
    open: Mutex<bool>,
    wake: Condvar,
}

impl CreationGate {
    pub fn before_commit() -> Self {
        Self::default()
    }

    pub fn after_commit() -> Self {
        Self {
            after_commit: true,
            ..Self::default()
        }
    }

    /// Hold the child in pre_exec: Command::spawn is still waiting for exec.
    pub fn during_creation() -> io::Result<Self> {
        Ok(Self {
            during: Some(super::sys::CreationPause::new()?),
            ..Self::default()
        })
    }

    pub fn pid(&self) -> Option<u32> {
        match self.pid.load(Ordering::Acquire) {
            0 => None,
            pid => Some(pid),
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn before_registration(&self, pid: i32) {
        self.pid.store(pid as u32, Ordering::Release);
        self.wait();
    }

    pub(super) fn install(&self, command: &mut std::process::Command) {
        if let Some(pause) = &self.during {
            pause.install(command);
        }
    }

    pub(super) fn wait_before_commit(&self) {
        if !self.after_commit && self.during.is_none() {
            self.wait();
        }
    }

    pub(super) fn wait_after_commit(&self) {
        if self.after_commit {
            self.wait();
        }
    }

    pub async fn entered(&self) {
        if self.entered.is_cancelled() {
            return;
        }
        if let Some(pause) = &self.during {
            pause.entered().await;
            self.entered.cancel();
        } else {
            self.entered.cancelled().await;
        }
    }

    pub fn release(&self) {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if !*open {
            if let Some(pause) = &self.during {
                pause.release();
            }
            *open = true;
        }
        self.wake.notify_all();
    }

    pub(super) fn wait(&self) {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        self.entered.cancel();
        while !*open {
            open = self.wake.wait(open).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Exercise exactly the registry primitive used by the CLI force-exit path.
pub fn force_kill_active_for_test() {
    super::root::force_kill_active();
}

thread_local! {
    static PIPE_FAILURE: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Fail the one-based k-th supervisor pipe on this creation thread with EMFILE.
/// A thread-bound scope avoids affecting other jobs or concurrent tests.
#[derive(Debug)]
pub struct PipeTestGuard(PhantomData<Rc<()>>);

impl PipeTestGuard {
    pub fn fail_at(k: usize) -> Self {
        assert!(k > 0, "pipe ordinals are one-based");
        PIPE_FAILURE.with(|failure| {
            assert!(failure.get().is_none(), "nested pipe failure scope");
            failure.set(Some(k));
        });
        Self(PhantomData)
    }
}

impl Drop for PipeTestGuard {
    fn drop(&mut self) {
        PIPE_FAILURE.with(|failure| failure.set(None));
    }
}

pub(super) fn before_pipe() -> io::Result<()> {
    PIPE_FAILURE.with(|failure| match failure.get() {
        Some(1) => {
            failure.set(Some(0));
            Err(io::Error::from_raw_os_error(libc::EMFILE))
        }
        Some(0) | None => Ok(()),
        Some(k) => {
            failure.set(Some(k - 1));
            Ok(())
        }
    })
}

/// In-memory acceptance timestamps; never serialized into records or reports.
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum Milestone {
    Transferred,
    RootReaped,
    CleanupResult,
    DrainStart,
    ReadersStopped,
    CaptureFrozen,
    RecordFinalized,
    RecordPersisted,
    ResourcesReleased,
}

#[derive(Debug, Default)]
pub struct Milestones {
    times: Mutex<[Option<Instant>; 9]>,
    exit_observations: AtomicU32,
}
impl Milestones {
    pub fn exit_observations(&self) -> u32 {
        self.exit_observations.load(Ordering::Acquire)
    }
    pub(in crate::supervisor) fn observed_exit(&self) {
        self.exit_observations.fetch_add(1, Ordering::AcqRel);
    }
    pub fn get(&self, milestone: Milestone) -> Option<Instant> {
        self.times.lock().unwrap_or_else(|e| e.into_inner())[milestone as usize]
    }
    pub(crate) fn mark(&self, milestone: Milestone) {
        self.mark_at(milestone, Instant::now());
    }
    pub(in crate::supervisor) fn mark_at(&self, milestone: Milestone, at: Instant) {
        self.times.lock().unwrap_or_else(|e| e.into_inner())[milestone as usize].get_or_insert(at);
    }
}

// Independent of admission/credit accounting: track the owning bundles for
// process identities, capture state, relays, stdin pumps and transcript files.
static OWNED_RESOURCES: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub(in crate::supervisor) struct OwnedResource;
impl OwnedResource {
    pub(in crate::supervisor) fn new() -> Self {
        OWNED_RESOURCES.fetch_add(1, Ordering::AcqRel);
        Self
    }
}
impl Drop for OwnedResource {
    fn drop(&mut self) {
        OWNED_RESOURCES.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(in crate::supervisor) fn owned_resources() -> usize {
    OWNED_RESOURCES.load(Ordering::Acquire)
}
