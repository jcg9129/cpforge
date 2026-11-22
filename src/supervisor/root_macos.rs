//! Darwin mechanisms for the shared Unix ownership and deadline policy.
use super::{Note, ObserverKind, Root, SpawnOptions, SupervisorError};
use crate::supervisor::sys::macos;
use std::io;
use std::time::Duration;

pub(in crate::supervisor) use macos::{
    CleanupScan, Observer, PreparedSpawn, cleanup_scan, exit_check, init_process, kill_group,
    kill_root, observe, observe_status, prepare_limits, prepare_spawn, reap, spawn,
};

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
            assert_eq!(outcome.termination.cleanup, Cleanup::Confirmed);
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
    fn session_exit_latch_and_zero_wait4_retries_preserve_ownership() {
        test(async {
            let mut root = Box::new(root(&["hold"], false));
            let pid = root.pid() as i32;
            let wall = Duration::from_secs(10);
            root.session_start();
            root.session_stop(None, 0).unwrap();
            root.session_ready().await;
            // NOTE_EXIT can precede waitid's status publication too. Keep the
            // root pinned and retry without requiring another queue event.
            while root.session_observe(wall).unwrap().is_none() {
                tokio::task::yield_now().await;
            }
            macos::inject_reap_zero(pid, 3);
            let start = Instant::now();
            let outcome = unix::session_finish(root, wall, CancelToken::new()).await;
            assert!(matches!(
                outcome.status,
                Termination::Signalled { signal: 9, .. }
            ));
            assert_eq!(outcome.termination.cleanup, Cleanup::Confirmed);
            assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
            assert!(start.elapsed() >= Duration::from_millis(7)); // 1 + 2 + 4 ms
            assert!(!outcome.notes.contains(&Note::Transferred));
            assert_eq!(outcome.stop_trigger, None);
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
    struct Group {
        root: Option<Root>,
        pid: i32,
        child: i32,
        directory: std::path::PathBuf,
    }

    impl Group {
        async fn new(zombie: bool) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!(
                    "darwin-group-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed),
                ));
            std::fs::create_dir_all(&directory).unwrap();
            let child_script = if zombie {
                "exit:0".to_owned()
            } else {
                format!("wait-file:{},exit:0", directory.join("child").display())
            };
            let root = root(
                &[
                    &format!("spawn:1:{child_script}"),
                    &format!("wait-file:{}", directory.join("root").display()),
                    "exit:0",
                ],
                false,
            );
            let pid = root.pid() as i32;
            // Install the guard before waiting for readiness, including on panic.
            let mut group = Self {
                root: Some(root),
                pid,
                child: 0,
                directory,
            };
            loop {
                if let Some(info) = macos::test_members(pid).into_iter().find(|info| {
                    info.pbi_ppid == pid as u32 && (info.pbi_status == libc::SZOMB) == zombie
                }) {
                    group.child = info.pbi_pid as i32;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            group
        }

        fn release_root(&self) {
            std::fs::write(self.directory.join("root"), b"").unwrap();
        }

        async fn wait_root_exit(&self) {
            self.release_root();
            while !macos::observe(self.pid).unwrap() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }

        async fn finish(mut self) {
            std::fs::write(self.directory.join("child"), b"").unwrap();
            self.release_root();
            drop(self.root.take());
            loop {
                if macos::test_members(self.pid)
                    .iter()
                    .all(|info| info.pbi_status == libc::SZOMB)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            // A skipped kill never strands a fixture: both gates also self-release
            // during unwinding. Keep files until the processes have seen them.
            if self.directory.exists() {
                let _ = std::fs::write(self.directory.join("child"), b"");
                let _ = std::fs::write(self.directory.join("root"), b"");
            }
        }
    }

    #[test]
    fn group_kill_removes_a_live_descendant_before_confirmation() {
        test(async {
            let mut group = Group::new(false).await;
            group.wait_root_exit().await;
            assert!(macos::test_members(group.pid).iter().any(|info| {
                info.pbi_pid == group.child as u32 && info.pbi_status != libc::SZOMB
            }));
            // Read the live member once, then let the real post-exit group kill
            // and repeated scans establish confirmation while the root is pinned.
            let scan = group.root.as_mut().unwrap().scan.as_mut().unwrap();
            assert_eq!(
                scan.poll(Instant::now() + Duration::from_secs(1), None),
                None
            );
            let outcome = unix::wait(Box::new(group.root.take().unwrap()), controls()).await;
            assert_eq!(outcome.status, Termination::Exited { code: 0 });
            assert_eq!(outcome.termination.cleanup, Cleanup::Confirmed);
            group.finish().await;
        });
    }

    #[test]
    fn one_run_with_a_skipped_kill_reports_one_live_survivor() {
        test(async {
            let mut group = Group::new(false).await;
            group.wait_root_exit().await;
            // Per-lease injection, retained on transfer and absent from release
            // builds. Other runs and the cleanup service have no global switch.
            group
                .root
                .as_mut()
                .unwrap()
                .lease
                .as_mut()
                .unwrap()
                .skip_kill = true;
            let start = Instant::now();
            let outcome = unix::wait(Box::new(group.root.take().unwrap()), controls()).await;
            assert_eq!(
                outcome.termination.cleanup,
                Cleanup::Incomplete { survivors: 1 }
            );
            assert!(start.elapsed() >= Duration::from_secs(1));
            assert!(macos::test_members(group.pid).iter().any(|info| {
                info.pbi_pid == group.child as u32 && info.pbi_status != libc::SZOMB
            }));
            group.finish().await;
        });
    }

    #[test]
    fn zombie_lookup_includes_an_unreaped_descendant_but_does_not_count_it_live() {
        test(async {
            let mut group = Group::new(true).await;
            // Its parent is still alive and has deliberately not waited. arg=1
            // must return the full zombie record, rather than arg=0's ESRCH.
            let info = macos::test_members(group.pid)
                .into_iter()
                .find(|info| info.pbi_pid == group.child as u32)
                .unwrap();
            let (status, pgid) = (info.pbi_status, info.pbi_pgid);
            assert_eq!(status, libc::SZOMB);
            assert_eq!(pgid, group.pid as u32);
            let root = group.root.as_mut().unwrap();
            root.lease.as_mut().unwrap().skip_kill = true;
            root.lease.as_ref().unwrap().kill().unwrap();
            let scan = root.scan.as_mut().unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            let cleanup = loop {
                if let Some(cleanup) = scan.poll(deadline, None) {
                    break cleanup;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            };
            assert_eq!(cleanup, Cleanup::Confirmed);
            root.lease.as_mut().unwrap().skip_kill = false;
            group.release_root();
            let outcome = unix::wait(Box::new(group.root.take().unwrap()), controls()).await;
            assert_eq!(outcome.termination.cleanup, Cleanup::Confirmed);
            group.finish().await;
        });
    }

    #[test]
    fn libproc_failures_are_unverified_and_keep_measurements_partial() {
        test(async {
            use crate::supervisor::Completeness;
            for reason in [
                UnverifiedReason::Io,
                UnverifiedReason::InfoFailed,
                UnverifiedReason::Truncated,
            ] {
                let mut group = Group::new(false).await;
                group.wait_root_exit().await;
                // Keep a real member present until pidinfo sees the injected
                // short return. The fixture self-releases after the outcome.
                let root = group.root.as_mut().unwrap();
                root.lease.as_mut().unwrap().skip_kill = true;
                let scan = root.scan.as_mut().unwrap();
                match reason {
                    UnverifiedReason::Io => scan.inject_list_error(libc::EACCES),
                    UnverifiedReason::InfoFailed => scan.inject_short_info(),
                    UnverifiedReason::Truncated => scan.inject_full_lists(),
                    _ => unreachable!(),
                }
                let outcome = unix::wait(Box::new(group.root.take().unwrap()), controls()).await;
                assert_eq!(outcome.termination.cleanup, Cleanup::Unverified { reason });
                assert!(outcome.measurements.iter().all(|measurement| {
                    measurement.value.is_some() && measurement.completeness == Completeness::Partial
                }));
                group.finish().await;
            }
        });
    }

    #[test]
    fn transferred_slow_scan_yields_so_another_run_can_finish() {
        test(async {
            let mut slow = root(&["hold"], true);
            let slow_ticket = slow.ticket();
            // Delay is scheduled, never slept on by the cleanup service.
            slow.scan
                .as_mut()
                .unwrap()
                .set_entry_delay(Duration::from_secs(2));
            drop(slow);
            let fast = root(&["exit:0"], false);
            let fast_ticket = fast.ticket();
            drop(fast);
            let outcome = fast_ticket.outcome().await.unwrap();
            assert_eq!(outcome.termination.cleanup, Cleanup::Confirmed);
            // The first slot's scan is still pending when the second slot has
            // completed. One run cannot occupy the poller until its deadline.
            assert!(crate::supervisor::debug_counters().slots_in_use >= 1);
            let transferred = slow_ticket.outcome().await.unwrap();
            // Logical publication can precede the physical scan cutoff (§3.3).
            assert_ne!(transferred.termination.cleanup, Cleanup::Confirmed);
        });
    }
}
