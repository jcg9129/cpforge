//! Darwin mechanisms for the shared Unix ownership and deadline policy.
use super::{Note, ObserverKind, Root, SpawnOptions, SupervisorError};
use crate::supervisor::sys::macos;
use std::io;
use std::time::Duration;

pub(in crate::supervisor) use macos::{
    CleanupScan, Observer, cleanup_scan, exit_check, init_process, kill_group, observe,
    prepare_limits, reap, spawn,
};

// Keep the complete native interface available. Its connected-run callers in
// the shared core (session root stop) are Linux-only.
#[allow(unused_imports)]
pub(in crate::supervisor) use macos::{kill_root, observe_status};

pub(super) fn register_observer(root: &mut Root, pid: i32, opts: &SpawnOptions) {
    if opts.force_polling_observer {
        root.outcome().notes.push(Note::ObserverFallback);
        return;
    }
    install_observer(root, macos::observer(pid));
}

// Separate result handling allows deterministic receipt-error injection without
// process-global hooks or changing another concurrent run's registration.
fn install_observer(root: &mut Root, result: io::Result<Observer>) {
    match result {
        Ok(observer) => {
            root.observer = Some(observer);
            root.outcome().timing.observer = ObserverKind::Kqueue;
        }
        Err(error) => fallback(root, error),
    }
}

fn fallback(root: &mut Root, error: io::Error) {
    // As on Linux, a registration or reactor error is reported as Register
    // only; ObserverFallback is reserved for a deliberate polling selection.
    super::push_error(
        root.outcome(),
        SupervisorError::Register(error.raw_os_error().unwrap_or(0)),
    );
    root.outcome().timing.observer = ObserverKind::Polling;
    root.observer = None;
}

async fn wait_observer(observer: &Observer, pid: i32) -> io::Result<()> {
    // Includes the post-registration probe and ESRCH latch; neither requires a
    // readable queue. Consumed NOTE_EXIT events remain latched across retries.
    if macos::queue_check(observer, pid)? {
        return Ok(());
    }
    loop {
        let mut ready = match observer.fd.readable().await {
            Ok(ready) => ready,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if macos::queue_check(observer, pid)? {
            return Ok(());
        }
        ready.clear_ready();
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
        match wait_observer(observer, pid).await {
            Ok(()) => return Ok(()),
            Err(error) => fallback(root, error),
        }
    }
    let mut backoff = 1;
    loop {
        if observe(pid)? {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(backoff)).await;
        backoff = (backoff * 2).min(10);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::root::unix;
    use crate::supervisor::{
        CancelToken, Cleanup, MetricKind, OutputSpec, ProgramSpec, RunControls, StdinSpec,
        Termination, UnverifiedReason,
    };
    use std::future::Future;
    use std::time::Instant;

    fn test(body: impl Future<Output = ()>) {
        let _lock = unix::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        crate::supervisor::init_process().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(20), async {
                    body.await;
                    while crate::supervisor::debug_counters().slots_in_use != 0 {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .expect("Darwin lifecycle test timed out");
            });
    }

    fn program(steps: &[&str]) -> ProgramSpec {
        ProgramSpec {
            program: cpf_fixture::harness::fixture_path(),
            args: steps.iter().map(Into::into).collect(),
            cwd: std::env::current_dir().unwrap(),
            env: Vec::new(),
            stdin: StdinSpec::Null,
            stdout: OutputSpec::Null,
            stderr: OutputSpec::Null,
            limits: Vec::new(),
            measure: vec![MetricKind::PeakRss, MetricKind::CpuTime],
        }
    }

    fn root(steps: &[&str], polling: bool) -> Root {
        unix::spawn(
            program(steps),
            crate::supervisor::admit().unwrap(),
            &SpawnOptions {
                force_polling_observer: polling,
                ..SpawnOptions::default()
            },
        )
        .unwrap()
    }

    fn controls() -> RunControls {
        RunControls {
            wall_deadline: Duration::from_secs(10),
            cancel: CancelToken::new(),
            events: None,
        }
    }

    #[test]
    fn exit_before_registration_takes_esrch_path_and_retains_status() {
        test(async {
            let mut root = root(&["exit:7"], true);
            let pid = root.pid() as i32;
            while !observe(pid).unwrap() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            // Register against the already exited, still-owned child. XNU reports
            // ESRCH; observer latches it and never waits for a historical edge.
            register_observer(&mut root, pid, &SpawnOptions::default());
            assert_eq!(root.outcome().timing.observer, ObserverKind::Kqueue);
            assert!(root.observer.as_ref().unwrap().registration_exited);
            assert!(exit_check(root.observer.as_ref(), pid).unwrap());
            assert!(exit_check(root.observer.as_ref(), pid).unwrap());
            assert_eq!(
                observe_status(pid).unwrap(),
                Some(Termination::Exited { code: 7 })
            );
            let outcome = unix::wait(Box::new(root), controls()).await;
            assert_eq!(outcome.status, Termination::Exited { code: 7 });
            assert_eq!(
                outcome.termination.cleanup,
                Cleanup::Unverified {
                    reason: UnverifiedReason::Unsupported
                }
            );
        });
    }

    #[test]
    fn reactor_wakes_on_the_run_queue_and_exit_remains_latched() {
        test(async {
            let mut root = root(&["hold"], false);
            let pid = root.pid() as i32;
            // Live registration succeeded; this cannot be the post-probe path.
            assert_eq!(root.outcome().timing.observer, ObserverKind::Kqueue);
            let observer = root.observer.as_ref().unwrap();
            assert!(!exit_check(Some(observer), pid).unwrap());
            let mut ready = Box::pin(observer.fd.readable());
            std::future::poll_fn(|cx| {
                assert!(ready.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            // The test owns this unreaped root and holds its signalling lease.
            root.lease.as_ref().unwrap().kill().unwrap();
            let mut notification = ready.await.unwrap();
            assert!(exit_check(Some(observer), pid).unwrap());
            notification.clear_ready();
            assert!(exit_check(Some(observer), pid).unwrap());
            drop(notification);
            let outcome = unix::wait(Box::new(root), controls()).await;
            assert!(matches!(
                outcome.status,
                Termination::Signalled { signal: 9, .. }
            ));
        });
    }

    #[test]
    fn registration_error_falls_back_and_finite_zero_reaps_back_off() {
        test(async {
            let mut root = root(&["exit:3"], true);
            let pid = root.pid() as i32;
            install_observer(&mut root, Err(io::Error::from_raw_os_error(libc::EACCES)));
            assert_eq!(root.outcome().timing.observer, ObserverKind::Polling);
            assert!(
                root.outcome()
                    .errors
                    .contains(&SupervisorError::Register(libc::EACCES))
            );
            macos::inject_reap_zero(pid, 3);
            let start = Instant::now();
            let outcome = unix::wait(Box::new(root), controls()).await;
            assert_eq!(outcome.status, Termination::Exited { code: 3 });
            // A zero wait4 is retried without an error on macOS (§4.2).
            assert!(!outcome.errors.contains(&SupervisorError::Reap));
            assert!(!outcome.errors.contains(&SupervisorError::ReapTimeout));
            assert!(start.elapsed() >= Duration::from_millis(7)); // 1 + 2 + 4 ms
            assert!(
                outcome
                    .measurements
                    .iter()
                    .all(|measurement| measurement.value.is_some())
            );
        });
    }

    #[test]
    fn zero_wait4_after_note_exit_retries_without_another_notification() {
        test(async {
            let mut root = root(&["hold"], false);
            let pid = root.pid() as i32;
            root.lease.as_ref().unwrap().kill().unwrap();
            observe_root(&mut root, pid).await.unwrap();
            assert_eq!(root.outcome().timing.observer, ObserverKind::Kqueue);
            macos::inject_reap_zero(pid, 3);
            let start = Instant::now();
            let outcome = unix::wait(Box::new(root), controls()).await;
            assert!(matches!(
                outcome.status,
                Termination::Signalled { signal: 9, .. }
            ));
            assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
            assert!(start.elapsed() >= Duration::from_millis(7));
            assert!(!outcome.notes.contains(&Note::Transferred));
        });
    }

    #[test]
    fn polling_reinitializes_no_event_results_after_an_exit() {
        test(async {
            let exited = root(&["exit:0"], true);
            while !observe(exited.pid() as i32).unwrap() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            let live = root(&["hold"], true);
            for _ in 0..20 {
                assert!(!observe(live.pid() as i32).unwrap());
            }
            drop(live);
            assert_eq!(
                unix::wait(Box::new(exited), controls()).await.status,
                Termination::Exited { code: 0 }
            );
        });
    }
}
