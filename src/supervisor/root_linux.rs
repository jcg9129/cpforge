//! Linux mechanisms for the Unix root lifecycle. Native operations stay in sys.

use super::{Note, ObserverKind, Root, SpawnOptions, SupervisorError, UnverifiedReason};
use crate::supervisor::sys;
use std::io;
use std::time::Duration;

pub(in crate::supervisor) use sys::{
    CleanupScan, Observer, exit_check, init_process, kill_group, kill_root, observe,
    observe_status, prepare_limits, reap, spawn,
};

// Other OS verifiers need the root pid even without an observer. Linux keeps
// using only the pidfd's procfs numbering, never the caller's numeric pid.
pub(super) fn cleanup_scan(
    _pid: i32,
    observer: Option<&Observer>,
) -> Result<CleanupScan, UnverifiedReason> {
    sys::cleanup_scan(observer)
}

pub(super) fn register_observer(root: &mut Root, pid: i32, opts: &SpawnOptions) {
    if opts.force_polling_observer {
        root.outcome().notes.push(Note::ObserverFallback);
    } else {
        match sys::observer(pid) {
            Ok(observer) => {
                root.observer = Some(observer);
                root.outcome().timing.observer = ObserverKind::Pidfd;
            }
            Err(error) => {
                let errno = error.raw_os_error().unwrap_or(0);
                if matches!(errno, libc::ENOSYS | libc::EPERM) {
                    root.outcome().notes.push(Note::ObserverFallback);
                } else {
                    root.outcome().errors.push(SupervisorError::Register(errno));
                }
            }
        }
    }
}

pub(super) async fn observe_root(root: &mut Root, pid: i32) -> io::Result<()> {
    if root.opts.observer_never_ready {
        std::future::pending::<()>().await;
    }
    if let Some(gate) = root.opts.exit_observation_gate.as_ref() {
        gate.cancelled().await;
    }
    if let Some(observer) = root.observer.as_ref() {
        loop {
            match observer.readable().await {
                Ok(mut ready) => {
                    // Tokio readiness may be spurious. Confirm without consuming
                    // status before permitting a post-exit containment kill.
                    if sys::observe(pid)? {
                        return Ok(());
                    }
                    ready.clear_ready();
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    // A reactor error does not consume status. Keep owning the
                    // root and switch to the same non-consuming polling path.
                    root.outcome()
                        .errors
                        .push(SupervisorError::Register(error.raw_os_error().unwrap_or(0)));
                    root.outcome().timing.observer = ObserverKind::Polling;
                    root.observer = None;
                    break;
                }
            }
        }
    }
    let mut backoff = 1;
    loop {
        if sys::observe(pid)? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(backoff)).await;
        backoff = (backoff * 2).min(10);
    }
}
