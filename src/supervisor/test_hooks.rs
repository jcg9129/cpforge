//! Deterministic supervisor setup failures, absent from production builds.

use std::cell::Cell;
use std::io;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Condvar, Mutex};

/// Pause immediately before the OS creation call, without holding registry or
/// platform creation locks. The test observes entry before cancelling/forcing.
#[derive(Debug, Default)]
pub struct CreationGate {
    entered: super::CancelToken,
    open: Mutex<bool>,
    wake: Condvar,
}

impl CreationGate {
    pub async fn entered(&self) {
        self.entered.cancelled().await;
    }

    pub fn release(&self) {
        *self.open.lock().unwrap_or_else(|e| e.into_inner()) = true;
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
