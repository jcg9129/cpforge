#![cfg(any(target_os = "linux", target_os = "macos"))]
use cpf_fixture::harness::fixture_path;
use cpforge::supervisor::session::*;
use cpforge::supervisor::*;
use std::future::Future;
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;

static LOCK: Mutex<()> = Mutex::new(());
fn test(body: impl Future<Output = ()>) {
    test_with_timeout(Duration::from_secs(20), body);
}
fn test_with_timeout(timeout: Duration, body: impl Future<Output = ()>) {
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    init_process().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(timeout, async {
                body.await;
                quiescent().await;
            })
            .await
            .expect("session test exceeded hard timeout");
        });
}
async fn quiescent() {
    let until = tokio::time::Instant::now() + Duration::from_secs(2);
    while debug_counters().slots_in_use != 0
        || debug_counters().service_pending != 0
        || debug_counters().credits_in_use != 1
    {
        assert!(
            tokio::time::Instant::now() < until,
            "resources not released: {:?}",
            debug_counters()
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
// Poll an observable readiness condition with a hard bound; elapsed time is
// never used as evidence that a fixture reached a particular step.
async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("fixture readiness timed out");
}

fn fd_count() -> usize {
    #[cfg(target_os = "linux")]
    let path = "/proc/self/fd";
    #[cfg(target_os = "macos")]
    let path = "/dev/fd";
    std::fs::read_dir(path).unwrap().count()
}

#[cfg(target_os = "linux")]
fn thread_count() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

#[cfg(target_os = "macos")]
fn thread_count() -> usize {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    // SAFETY: the buffer fits the complete record; errno is thread-local.
    let (count, errno) = unsafe {
        *libc::__error() = 0;
        let count = libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        );
        (count, *libc::__error())
    };
    assert_eq!(
        count, size,
        "proc_pidinfo(PROC_PIDTASKINFO) failed: errno {errno}"
    );
    // SAFETY: proc_pidinfo returned the entire initialized record.
    unsafe { info.assume_init().pti_threadnum as usize }
}

// Return (start identity, zombie). Root identities stay pinned until reaping;
// descendants may remain as zombies under the system reaper after group kill.
#[cfg(target_os = "linux")]
fn process_identity(pid: u32) -> Option<(u64, bool)> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("cannot inspect {pid}: {error}"),
    };
    // Reaping can race a procfs read, leaving an empty or truncated record.
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((fields.get(19)?.parse().ok()?, *fields.first()? == "Z"))
}

#[cfg(target_os = "macos")]
fn process_identity(pid: u32) -> Option<(u64, bool)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: the output fits a complete record; arg=1 includes zombies.
    let (count, errno) = unsafe {
        *libc::__error() = 0;
        let count = libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            1,
            info.as_mut_ptr().cast(),
            size,
        );
        (count, *libc::__error())
    };
    if count <= 0 && errno == libc::ESRCH {
        return None;
    }
    assert_eq!(count, size, "proc_pidinfo({pid}) failed: errno {errno}");
    // SAFETY: the full initialized record was returned above.
    let info = unsafe { info.assume_init() };
    Some((
        info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        info.pbi_status == libc::SZOMB,
    ))
}

fn identity_file(path: &std::path::Path) -> (u32, u64) {
    let text = std::fs::read_to_string(path).unwrap();
    let (pid, start) = text.trim().split_once(' ').unwrap();
    (pid.parse().unwrap(), start.parse().unwrap())
}

fn program(steps: &[&str]) -> ProgramSpec {
    ProgramSpec {
        program: fixture_path(),
        args: steps.iter().map(Into::into).collect(),
        cwd: std::env::current_dir().unwrap(),
        env: Vec::new(),
        stdin: StdinSpec::Null,
        stdout: OutputSpec::Null,
        stderr: OutputSpec::Null,
        limits: Vec::new(),
        measure: vec![MetricKind::CpuTime],
    }
}
fn spec(s: &[&str], i: &[&str]) -> SessionSpec {
    SessionSpec::new(program(s), program(i), Duration::from_secs(4))
}
fn root(record: &RunRecord) -> &RootOutcome {
    match record {
        RunRecord::Started(outcome) => outcome,
        _ => panic!("not started: {record:?}"),
    }
}
fn stop(outcome: &SessionOutcome, role: Role) -> Option<(u64, StopCause)> {
    outcome.record.events().iter().find_map(|e| match e.fact {
        Fact::StopRequested { role: r, cause } if r == role => Some((e.obs_seq, cause)),
        _ => None,
    })
}
fn exited(outcome: &SessionOutcome, role: Role) -> (u64, Provenance) {
    outcome
        .record
        .events()
        .iter()
        .find_map(|e| match e.fact {
            Fact::RootExited {
                role: r,
                provenance,
                ..
            } if r == role => Some((e.obs_seq, provenance)),
            _ => None,
        })
        .unwrap()
}
fn trace(outcome: &SessionOutcome) {
    let events = outcome.record.events();
    assert!(events.len() <= 17);
    assert!(outcome.record.to_json().len() <= 16 * 1024);
    for (seq, event) in events.iter().enumerate() {
        assert_eq!(event.obs_seq, seq as u64);
    }
    for (role, run) in [(Role::S, &outcome.solution), (Role::I, &outcome.interactor)] {
        assert_eq!(events.iter().filter(|e| matches!(e.fact, Fact::Started { role: r } | Fact::NotStarted { role: r, .. } if r == role)).count(), 1);
        assert!(
            events
                .iter()
                .filter(|e| matches!(e.fact, Fact::StopRequested { role: r, .. } if r == role))
                .count()
                <= 1
        );
        if let RunRecord::Started(run) = run {
            let (exit, provenance) = exited(outcome, role);
            let cleanup = events
                .iter()
                .find(|e| matches!(e.fact, Fact::CleanupResult { role: r, .. } if r == role))
                .unwrap();
            assert!(cleanup.obs_seq > exit);
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e.fact, Fact::RootExited { role: r, .. } if r == role))
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e.fact, Fact::CleanupResult { role: r, .. } if r == role))
                    .count(),
                1
            );
            match (stop(outcome, role), provenance) {
                (None, Provenance::Natural) => {}
                (
                    Some((seq, StopCause::OwnLimit(OwnLimit::DialogueOutput))),
                    Provenance::Natural,
                ) => {
                    assert!(seq > exit);
                }
                (Some((seq, _)), Provenance::Stopped(reference)) => {
                    assert_eq!(seq, reference);
                    assert!(seq < exit);
                }
                _ => panic!("inconsistent provenance: {events:?}"),
            }
            assert!(run.errors.is_empty(), "{run:?}");
            assert!(!matches!(
                run.termination.cleanup,
                Cleanup::Incomplete { .. }
            ));
        }
    }
    for stats in [
        &outcome.solution_to_interactor,
        &outcome.interactor_to_solution,
    ]
    .into_iter()
    .filter_map(DirectionStatistics::as_relay)
    {
        assert_eq!(
            stats.delivery_incomplete.bytes,
            stats.bytes_read - stats.bytes_forwarded - stats.discarded_over_cap
        );
        assert_eq!(stats.bytes_queued_at_end, stats.delivery_incomplete.bytes);
        assert!(stats.peak_queued_bytes <= outcome.queue_bytes as u64);
        assert!(stats.peak_queued_records <= outcome.queue_records);
        assert_eq!(stats.final_state, RelayState::Closed);
        assert!(stats.transitions.len() <= 3);
        assert_eq!(stats.transitions.last(), Some(&RelayState::Closed));
    }
}

#[test]
fn both_modes_use_native_observers_or_bounded_polling_and_scoped_accounting() {
    test(async {
        for mode in [SessionMode::Relay, SessionMode::Direct] {
            for polling in [false, true] {
                let mut spec = spec(&["pingpong:1000"], &["pong:1000"]);
                spec.mode = mode;
                for options in &mut spec.spawn_options {
                    options.force_polling_observer = polling;
                }
                let outcome = run_session(spec).await;
                trace(&outcome);
                for run in [&outcome.solution, &outcome.interactor] {
                    let run = root(run);
                    assert_eq!(run.status, Termination::Exited { code: 0 });
                    let expected = if polling {
                        ObserverKind::Polling
                    } else if cfg!(target_os = "macos") {
                        ObserverKind::Kqueue
                    } else {
                        ObserverKind::Pidfd
                    };
                    assert_eq!(run.timing.observer, expected);
                    // Linux requires pidfd fdinfo to establish procfs numbering.
                    // Darwin's libproc scan can verify even with polling selected.
                    let (cleanup, completeness) = if polling && cfg!(target_os = "linux") {
                        (
                            Cleanup::Unverified {
                                reason: UnverifiedReason::NamespaceUnknown,
                            },
                            Completeness::Partial,
                        )
                    } else {
                        (Cleanup::Confirmed, Completeness::Final)
                    };
                    assert_eq!(run.termination.cleanup, cleanup);
                    assert!(run.measurements.iter().all(|measurement| {
                        measurement.value.is_some() && measurement.completeness == completeness
                    }));
                }
            }
        }
    });
}

#[test]
fn both_modes_reap_roots_and_remove_live_descendant_identities_on_finish_cancel_and_drop() {
    test(async {
        for mode in [SessionMode::Relay, SessionMode::Direct] {
            for action in ["finish", "cancel", "drop"] {
                let directory = TranscriptDirectory::new();
                std::fs::create_dir_all(&directory.0).unwrap();
                let release = directory.0.join("release");
                let paths =
                    ["s-root", "s-child", "i-root", "i-child"].map(|name| directory.0.join(name));
                let steps = |index: usize| {
                    vec![
                        format!("spawn:1:identity-file:{},hold", paths[index + 1].display()),
                        format!("identity-file:{}", paths[index].display()),
                        format!("wait-file:{}", release.display()),
                        "exit:0".to_owned(),
                    ]
                };
                let s_steps = steps(0);
                let i_steps = steps(2);
                let mut spec = spec(
                    &s_steps.iter().map(String::as_str).collect::<Vec<_>>(),
                    &i_steps.iter().map(String::as_str).collect::<Vec<_>>(),
                );
                spec.mode = mode;
                let cancel = spec.cancel.clone();
                let mut session = Box::pin(run_session(spec));
                tokio::select! {
                    outcome = &mut session => panic!("session ended before release: {outcome:?}"),
                    () = until(|| paths.iter().all(|path| path.exists())) => {},
                }
                let identities = paths.each_ref().map(|path| identity_file(path));
                for &(pid, start) in &identities {
                    assert_eq!(process_identity(pid), Some((start, false)));
                }
                if action == "drop" {
                    // Drop the owning future, not merely its pinned reference.
                    drop(session);
                } else {
                    if action == "cancel" {
                        cancel.cancel();
                    } else {
                        std::fs::write(&release, b"").unwrap();
                    }
                    let outcome = session.await;
                    trace(&outcome);
                    for role in [Role::S, Role::I] {
                        if action == "cancel" {
                            assert_eq!(
                                stop(&outcome, role).unwrap().1,
                                StopCause::Teardown(Teardown::Cancel)
                            );
                        } else {
                            assert!(stop(&outcome, role).is_none());
                        }
                    }
                }
                quiescent().await;
                for (index, &(pid, start)) in identities.iter().enumerate() {
                    until(|| {
                        process_identity(pid).is_none_or(|identity| {
                            identity.0 != start || (index % 2 == 1 && identity.1)
                        })
                    })
                    .await;
                }
                assert_eq!(debug_counters().credits_in_use, 1);
            }
        }
    });
}

#[test]
fn fixture_01_wall_limit_and_provenance() {
    test(async {
        let mut spec = spec(&["hold"], &["drain-stdin"]);
        spec.wall_deadline = Duration::from_millis(80);
        let outcome = run_session(spec).await;
        trace(&outcome);
        let (seq, cause) = stop(&outcome, Role::S).unwrap();
        assert_eq!(cause, StopCause::OwnLimit(OwnLimit::Wall));
        assert!(seq < exited(&outcome, Role::I).0);
    });
}
#[test]
fn fixture_02_sink_gone_reaches_writer_as_sigpipe() {
    test(async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let release = directory.0.join("release");
        let mut spec = spec(
            &[
                "default-sigpipe",
                &format!("wait-file:{}", release.display()),
                "write:stdout:4194304",
            ],
            &["exit:0"],
        );
        let observed = CancelToken::new();
        spec.exit_observed[1] = Some(observed.clone());
        let (outcome, ()) = tokio::join!(run_session(spec), async {
            observed.cancelled().await;
            std::fs::write(&release, b"").unwrap();
        });
        trace(&outcome);
        assert_eq!(
            root(&outcome.solution).status,
            Termination::Signalled {
                signal: libc::SIGPIPE,
                core_dumped: false
            }
        );
        assert!(exited(&outcome, Role::I).0 < exited(&outcome, Role::S).0);
        assert_eq!(exited(&outcome, Role::S).1, Provenance::Natural);
        assert!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .transitions
                .contains(&RelayState::SinkGone)
        );
    });
}
#[test]
fn fixture_03_half_close_while_root_alive() {
    test(async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let release = directory.0.join("release");
        let mut spec = spec(
            &["close:stdout", &format!("wait-file:{}", release.display())],
            &["drain-stdin"],
        );
        let observed = CancelToken::new();
        spec.exit_observed[1] = Some(observed.clone());
        let (outcome, ()) = tokio::join!(run_session(spec), async {
            observed.cancelled().await;
            std::fs::write(&release, b"").unwrap();
        });
        trace(&outcome);
        assert!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .transitions
                .contains(&RelayState::HalfClosed)
        );
        assert!(exited(&outcome, Role::I).0 < exited(&outcome, Role::S).0);
        assert!(stop(&outcome, Role::S).is_none());
    });
}
#[test]
fn fixture_04_draining_destination_receives_every_byte() {
    test(async {
        let outcome = run_session(spec(
            &["write:stdout:524288", "close:stdout", "read-exactly:7"],
            &["drain-stdin"],
        ))
        .await;
        trace(&outcome);
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_read,
            524288
        );
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            524288
        );
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .delivery_incomplete
                .bytes,
            0
        );
        // drain-stdin writes the decimal count plus newline back to S.
        assert_eq!(
            outcome
                .interactor_to_solution
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            7
        );
        assert_eq!(
            root(&outcome.interactor).status,
            Termination::Exited { code: 0 }
        );
    });
}
#[test]
fn fixture_05_slow_destination_and_exact_loss_accounting() {
    test(async {
        let mut spec = spec(&["write:stdout:4194304", "hold"], &["hold"]);
        spec.session_deadline = Some(Duration::from_millis(100));
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(stop(&outcome, Role::S).unwrap().1, StopCause::SessionLimit);
        let stats = outcome.solution_to_interactor.as_relay().unwrap();
        assert!(stats.delivery_incomplete.bytes > 0);
        assert_eq!(stats.discarded_over_cap, 0);
        assert_eq!(
            stats.delivery_incomplete.bytes,
            stats.bytes_read - stats.bytes_forwarded
        );
        assert_eq!(
            stats.delivery_incomplete.unread_pipe_contents,
            UnreadPipeContents::Unknown
        );
        assert!(stats.bytes_read < 4194304);
    });
}
#[test]
fn fixture_06_inherited_stdout_survives_until_transport_drain() {
    test(async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let release = directory.0.join("release");
        let mut spec = spec(
            &[
                &format!(
                    "spawn:1:wait-file:{},write:stdout:20000,close:stdout,hold",
                    release.display()
                ),
                "exit:0",
            ],
            &["read-exactly:20000", "exit:0"],
        );
        let observed = CancelToken::new();
        spec.exit_observed[0] = Some(observed.clone());
        let (outcome, ()) = tokio::join!(run_session(spec), async {
            observed.cancelled().await;
            std::fs::write(&release, b"").unwrap();
        });
        trace(&outcome);
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            20000
        );
        assert!(exited(&outcome, Role::S).0 < exited(&outcome, Role::I).0);
        assert!(outcome.elapsed < Duration::from_secs(3));
    });
}
#[test]
fn fixture_07_large_bidirectional_writes_end_at_earliest_limit() {
    test(async {
        let mut spec = spec(
            &["write:stdout:4194304", "drain-stdin"],
            &["write:stdout:4194304", "drain-stdin"],
        );
        spec.wall_deadline = Duration::from_millis(100);
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(
            stop(&outcome, Role::S).unwrap().1,
            StopCause::OwnLimit(OwnLimit::Wall)
        );
        for stats in [
            outcome.solution_to_interactor.as_relay().unwrap(),
            outcome.interactor_to_solution.as_relay().unwrap(),
        ] {
            assert_eq!(stats.peak_queued_bytes, 256 * 1024);
            assert!(stats.delivery_incomplete.bytes > 0);
        }
    });
}
#[test]
fn fixture_08_stderr_flood_does_not_stall_forwarding() {
    test(async {
        let outcome = run_session(spec(
            &["flood:2097152", "close:stdout", "read-exactly:8"],
            &["write:stderr:2097152", "drain-stdin"],
        ))
        .await;
        trace(&outcome);
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            2097152
        );
        for run in [&outcome.solution, &outcome.interactor] {
            let stderr = root(run).stderr.as_ref().unwrap();
            assert_eq!(stderr.observed, 2097152);
            assert_eq!(stderr.retained.len(), 1024 * 1024);
            assert!(
                stop(
                    &outcome,
                    if std::ptr::eq(run, &outcome.solution) {
                        Role::S
                    } else {
                        Role::I
                    }
                )
                .is_none()
            );
        }
    });
}
#[test]
fn fixture_13_both_exits_then_bounded_final_drain() {
    test(async {
        let outcome = run_session(spec(
            &["spawn:1:hold", "write:stdout:262144", "exit:0"],
            &["spawn:1:hold", "write:stdout:262144", "exit:0"],
        ))
        .await;
        trace(&outcome);
        assert_eq!(exited(&outcome, Role::S).1, Provenance::Natural);
        assert_eq!(exited(&outcome, Role::I).1, Provenance::Natural);
        for stats in [
            outcome.solution_to_interactor.as_relay().unwrap(),
            outcome.interactor_to_solution.as_relay().unwrap(),
        ] {
            assert_eq!(stats.bytes_read, 262144);
            assert!(stats.delivery_incomplete.bytes > 0);
            assert_eq!(stats.final_state, RelayState::Closed);
        }
        assert!(outcome.elapsed >= Duration::from_millis(500));
        assert!(outcome.elapsed < Duration::from_secs(3));
    });
}
#[test]
fn fixture_14_peer_allowance_in_both_directions() {
    test(async {
        for late in [Role::S, Role::I] {
            let mut spec = if late == Role::S {
                spec(&["hold"], &["exit:0"])
            } else {
                spec(&["exit:0"], &["hold"])
            };
            spec.peer_allowance = Duration::from_millis(80);
            let outcome = run_session(spec).await;
            trace(&outcome);
            assert_eq!(
                stop(&outcome, late).unwrap().1,
                StopCause::PeerAllowanceExpired
            );
            assert_eq!(
                exited(&outcome, late).1,
                Provenance::Stopped(stop(&outcome, late).unwrap().0)
            );
            assert!(outcome.elapsed < Duration::from_secs(2));
        }
    });
}
#[test]
fn fixture_15_cancel_during_blocked_forward() {
    test(async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let ready = directory.0.join("writer-ready");
        let spec = spec(
            &[
                "write:stdout:262144",
                &format!("identity-file:{}", ready.display()),
                "write:stdout:3932160",
                "hold",
            ],
            &["hold"],
        );
        let cancel = spec.cancel.clone();
        let (outcome, ()) = tokio::join!(run_session(spec), async {
            // The non-reading peer has accepted only a pipe's worth; the
            // remaining prefix has reached the relay before cancellation.
            until(|| ready.exists()).await;
            cancel.cancel();
            cancel.cancel();
        });
        trace(&outcome);
        assert_eq!(
            outcome
                .record
                .events()
                .iter()
                .filter(|e| matches!(e.fact, Fact::CancelRequested))
                .count(),
            1
        );
        let cancel_seq = outcome
            .record
            .events()
            .iter()
            .find(|e| matches!(e.fact, Fact::CancelRequested))
            .unwrap()
            .obs_seq;
        for role in [Role::S, Role::I] {
            let (seq, cause) = stop(&outcome, role).unwrap();
            assert!(seq > cancel_seq);
            assert_eq!(cause, StopCause::Teardown(Teardown::Cancel));
        }
        assert!(outcome.elapsed < Duration::from_secs(2));
    });
}
#[test]
fn fixture_18_fifty_warmup_then_five_hundred_sessions_release_all_resources() {
    test_with_timeout(Duration::from_secs(120), async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let paths = [directory.0.join("s"), directory.0.join("i")];
        let mut identities = Vec::with_capacity(1100);
        async fn once(paths: &[std::path::PathBuf; 2], identities: &mut Vec<(u32, u64)>) {
            // No dialogue, descendants, or barrier round trips: publish each
            // root's identity solely so an unreaped zombie cannot be missed.
            let outcome = run_session(spec(
                &[&format!("identity-file:{}", paths[0].display()), "exit:0"],
                &[&format!("identity-file:{}", paths[1].display()), "exit:0"],
            ))
            .await;
            trace(&outcome);
            for run in [&outcome.solution, &outcome.interactor] {
                assert_eq!(root(run).status, Termination::Exited { code: 0 });
            }
            identities.extend(paths.iter().map(|path| identity_file(path)));
        }
        fn counts(identities: &[(u32, u64)]) -> (usize, usize, Counters, usize) {
            let mut zombies = 0;
            for &(pid, start) in identities {
                if let Some((observed_start, zombie)) = process_identity(pid) {
                    if observed_start == start {
                        assert!(zombie, "session root {pid} is still live at quiescence");
                        zombies += 1;
                    }
                }
            }
            (fd_count(), thread_count(), debug_counters(), zombies)
        }
        for _ in 0..50 {
            once(&paths, &mut identities).await;
        }
        quiescent().await;
        let baseline = counts(&identities);
        assert_eq!(baseline.2.slots_in_use, 0);
        assert_eq!(baseline.2.service_pending, 0);
        assert_eq!(baseline.2.credits_in_use, 1); // the process cleanup service
        assert_eq!(baseline.3, 0);
        for batch in 1..=5 {
            for _ in 0..100 {
                once(&paths, &mut identities).await;
            }
            quiescent().await;
            // OS thread disappearance can trail the last credit release.
            until(|| counts(&identities) == baseline).await;
            assert_eq!(counts(&identities), baseline, "session batch {batch}");
        }
    });
}
#[test]
fn dialogue_cap_is_attributed_to_its_writer() {
    test(async {
        for writer in [Role::S, Role::I] {
            let mut spec = if writer == Role::S {
                spec(&["write:stdout:4194304", "hold"], &["drain-stdin"])
            } else {
                spec(&["drain-stdin"], &["write:stdout:4194304", "hold"])
            };
            spec.dialogue_cap = [4096; 2];
            let outcome = run_session(spec).await;
            trace(&outcome);
            assert_eq!(
                stop(&outcome, writer).unwrap().1,
                StopCause::OwnLimit(OwnLimit::DialogueOutput)
            );
            let stats = if writer == Role::S {
                outcome.solution_to_interactor.as_relay().unwrap()
            } else {
                outcome.interactor_to_solution.as_relay().unwrap()
            };
            assert!(stats.bytes_read >= 4097);
            assert_eq!(stats.bytes_forwarded, 4096);
            assert_eq!(stats.discarded_over_cap, stats.bytes_read - 4096);
        }
    });
}
#[test]
fn failed_creation_records_cause_before_peer_teardown() {
    test(async {
        for failed in [Role::S, Role::I] {
            let mut spec = spec(&["hold"], &["hold"]);
            if failed == Role::S {
                spec.solution.program = spec.solution.cwd.join("no-such-cpf-session-fixture");
            } else {
                spec.interactor.program = spec.interactor.cwd.join("no-such-cpf-session-fixture");
            }
            let outcome = run_session(spec).await;
            trace(&outcome);
            let infrastructure = outcome
                .record
                .events()
                .iter()
                .find(|e| matches!(e.fact, Fact::Infrastructure { .. }))
                .unwrap()
                .obs_seq;
            let peer = if failed == Role::S { Role::I } else { Role::S };
            let (seq, cause) = stop(&outcome, peer).unwrap();
            assert!(infrastructure < seq);
            assert_eq!(cause, StopCause::Teardown(Teardown::Infrastructure));
        }
    });
}
#[test]
fn pair_admission_failure_releases_every_reservation() {
    test(async {
        let held = [admit().unwrap(), admit().unwrap(), admit().unwrap()];
        let before = debug_counters();
        let outcome = run_session(spec(&["hold"], &["hold"])).await;
        trace(&outcome);
        assert_eq!(debug_counters(), before);
        for run in [&outcome.solution, &outcome.interactor] {
            assert!(
                matches!(run, RunRecord::NotStarted(reason) if reason.kind == StartFailureKind::CleanupCapacity)
            );
        }
        drop(held);
    });
}
#[test]
fn connected_plain_runs_are_unsupported() {
    test(async {
        let mut plain = program(&["exit:0"]);
        plain.stdin = StdinSpec::Connected;
        assert!(matches!(
            spawn_root(plain, admit().unwrap(), &SpawnOptions::default()),
            Err(NotStarted::Unsupported)
        ));
        let mut plain = program(&["exit:0"]);
        plain.stdout = OutputSpec::Connected;
        assert!(matches!(
            spawn_root(plain, admit().unwrap(), &SpawnOptions::default()),
            Err(NotStarted::Unsupported)
        ));
    });
}
#[test]
fn cancelled_before_start_has_no_infrastructure_or_children() {
    test(async {
        let spec = spec(&["hold"], &["hold"]);
        spec.cancel.cancel();
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert!(matches!(
            outcome.record.events()[0].fact,
            Fact::CancelRequested
        ));
        assert!(
            !outcome
                .record
                .events()
                .iter()
                .any(|e| matches!(e.fact, Fact::Infrastructure { .. } | Fact::Started { .. }))
        );
    });
}

#[test]
fn one_blocked_direction_does_not_stall_the_reverse_direction() {
    test(async {
        let mut spec = spec(
            &[
                "spawn:1:read-exactly:4,write:stderr:1,hold",
                "write:stdout:4194304",
                "hold",
            ],
            &["write:stdout:4", "hold"],
        );
        spec.queue_bytes = 4096;
        spec.queue_records = 1;
        spec.session_deadline = Some(Duration::from_millis(200));
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .peak_queued_bytes,
            4096
        );
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .peak_queued_records,
            1
        );
        assert_eq!(
            outcome
                .interactor_to_solution
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            4
        );
        assert_eq!(
            root(&outcome.solution).stderr.as_ref().unwrap().retained,
            b"x"
        );
    });
}

#[test]
fn interactor_wall_includes_its_allowance() {
    test(async {
        let mut spec = spec(&["exit:0"], &["hold"]);
        spec.wall_deadline = Duration::from_millis(80);
        spec.interactor_allowance = Duration::from_millis(80);
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(
            stop(&outcome, Role::I).unwrap().1,
            StopCause::OwnLimit(OwnLimit::Wall)
        );
        assert!(stop(&outcome, Role::S).is_none());
        assert!(root(&outcome.interactor).timing.wall.unwrap() >= Duration::from_millis(160));
    });
}

#[test]
fn pair_worker_credit_failure_creates_no_workers_or_roots() {
    test(async {
        let held = global_bounds()
            .unwrap()
            .thread_credits
            .try_reserve(31)
            .unwrap();
        let baseline = debug_counters();
        let mut spec = spec(&["hold"], &["hold"]);
        let path = spec
            .solution
            .cwd
            .join("target")
            .join(format!("session-unopened-stderr-{}", std::process::id()));
        assert!(!path.exists());
        for run in [&mut spec.solution, &mut spec.interactor] {
            run.stderr = OutputSpec::Capture(StreamPolicy {
                sink: Sink::File(path.clone()),
                budget: 1,
                on_exceed: Exceed::Kill,
                events: false,
            });
        }
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert!(!path.exists());
        assert_eq!(debug_counters(), baseline);
        for run in [&outcome.solution, &outcome.interactor] {
            assert!(
                matches!(run, RunRecord::NotStarted(reason) if reason.kind == StartFailureKind::Worker)
            );
        }
        drop(held);
    });
}

#[test]
fn second_worker_failure_rolls_back_prepared_peer_without_starting() {
    test(async {
        let mut spec = spec(&["hold"], &["hold"]);
        let path = spec
            .solution
            .cwd
            .join("target")
            .join(format!("session-prepare-stderr-{}", std::process::id()));
        assert!(!path.exists());
        for run in [&mut spec.solution, &mut spec.interactor] {
            run.stderr = OutputSpec::Capture(StreamPolicy {
                sink: Sink::File(path.clone()),
                budget: 1,
                on_exceed: Exceed::Kill,
                events: false,
            });
        }
        spec.spawn_options[1].stderr_sink.fail_start = true;
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert!(
            !outcome
                .record
                .events()
                .iter()
                .any(|e| matches!(e.fact, Fact::Started { .. }))
        );
        assert!(matches!(
            outcome.interactor,
            RunRecord::NotStarted(StartFailure {
                kind: StartFailureKind::Worker,
                ..
            })
        ));
        quiescent().await;
        std::fs::remove_file(path).unwrap();
    });
}

#[test]
fn unobservable_root_recovery_records_infrastructure_before_stopping_peer() {
    test(async {
        let mut spec = spec(&["hold"], &["hold"]);
        spec.wall_deadline = Duration::from_millis(80);
        spec.interactor_allowance = Duration::from_secs(10);
        spec.spawn_options[0].observer_never_ready = true;
        let outcome = run_session(spec).await;
        assert_eq!(
            stop(&outcome, Role::S).unwrap().1,
            StopCause::OwnLimit(OwnLimit::Wall)
        );
        assert_eq!(
            root(&outcome.solution).stop_trigger,
            Some(StopTrigger::WallDeadline)
        );
        assert!(
            root(&outcome.solution)
                .errors
                .contains(&SupervisorError::ReapTimeout)
        );
        let infrastructure = outcome
            .record
            .events()
            .iter()
            .find(|e| matches!(e.fact, Fact::Infrastructure { .. }))
            .unwrap()
            .obs_seq;
        let (stop_seq, cause) = stop(&outcome, Role::I).unwrap();
        assert!(infrastructure < stop_seq);
        assert_eq!(cause, StopCause::Teardown(Teardown::Infrastructure));
        assert_eq!(
            exited(&outcome, Role::S).1,
            Provenance::Stopped(stop(&outcome, Role::S).unwrap().0)
        );
        assert_eq!(exited(&outcome, Role::I).1, Provenance::Stopped(stop_seq));
        assert!(
            outcome.elapsed < Duration::from_millis(2850),
            "{:?}",
            outcome.elapsed
        );
        assert_eq!(
            outcome
                .record
                .events()
                .iter()
                .filter(|e| matches!(e.fact, Fact::Infrastructure { .. }))
                .count(),
            1
        );
    });
}

#[cfg(debug_assertions)]
#[test]
fn immediate_cap_breach_is_recorded_after_natural_exit_including_final_drain() {
    test(async {
        for final_drain in [false, true] {
            for writer in [Role::S, Role::I] {
                let reader = if final_drain {
                    vec!["spawn:1:drain-stdin", "exit:0"]
                } else {
                    vec!["drain-stdin"]
                };
                let mut spec = if writer == Role::S {
                    spec(&["write:stdout:4097", "exit:0"], &reader)
                } else {
                    spec(&reader, &["write:stdout:4097", "exit:0"])
                };
                let observed = CancelToken::new();
                let peer_observed = CancelToken::new();
                let index = if writer == Role::S { 0 } else { 1 };
                spec.exit_observed[index] = Some(observed.clone());
                if final_drain {
                    spec.exit_observed[1 - index] = Some(peer_observed.clone());
                }
                spec.dialogue_cap = [4096; 2];
                // Hold transport until the small writer has exited. In the
                // final-drain case both roots exit while descendants own stdin.
                let gate = CancelToken::new();
                spec.relay_start_gate = Some(gate.clone());
                let (outcome, ()) = tokio::join!(run_session(spec), async {
                    observed.cancelled().await;
                    if final_drain {
                        peer_observed.cancelled().await;
                    }
                    gate.cancel();
                });
                trace(&outcome);
                let (exit, provenance) = exited(&outcome, writer);
                assert_eq!(provenance, Provenance::Natural);
                let (limit, cause) = stop(&outcome, writer).unwrap();
                assert_eq!(cause, StopCause::OwnLimit(OwnLimit::DialogueOutput));
                assert!(limit > exit);
                let (stats, run) = if writer == Role::S {
                    (
                        outcome.solution_to_interactor.as_relay().unwrap(),
                        &outcome.solution,
                    )
                } else {
                    (
                        outcome.interactor_to_solution.as_relay().unwrap(),
                        &outcome.interactor,
                    )
                };
                assert_eq!(root(run).stop_trigger, None);
                assert!(root(run).violations.is_empty());
                assert_eq!(stats.bytes_read, 4097);
                assert_eq!(stats.bytes_forwarded, 4096);
                assert_eq!(stats.discarded_over_cap, 1);
                assert_eq!(stats.delivery_incomplete.bytes, 0);
                assert!(stats.source_eof);
            }
        }
    });
}

#[test]
fn overflow_drains_to_eof_and_interactor_exits_naturally() {
    test(async {
        let mut spec = spec(&["write:stdout:16384", "hold"], &["drain-stdin"]);
        spec.dialogue_cap[0] = 4096;
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(
            stop(&outcome, Role::S).unwrap().1,
            StopCause::OwnLimit(OwnLimit::DialogueOutput)
        );
        assert!(stop(&outcome, Role::I).is_none());
        assert_eq!(exited(&outcome, Role::I).1, Provenance::Natural);
        assert_eq!(
            root(&outcome.interactor).status,
            Termination::Exited { code: 0 }
        );
        let stats = outcome.solution_to_interactor.as_relay().unwrap();
        assert_eq!(stats.bytes_forwarded, 4096);
        assert!(stats.discarded_over_cap > 1);
        assert!(stats.source_eof);
        assert_eq!(stats.delivery_incomplete.bytes, 0);
        assert_eq!(
            stats.delivery_incomplete.unread_pipe_contents,
            UnreadPipeContents::KnownZero
        );
    });
}

#[test]
fn late_overflow_after_wall_stop_is_diagnostic_only() {
    test(async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let release = directory.0.join("release");
        let mut spec = spec(
            &[
                &format!("spawn:1:wait-file:{},write:stdout:4097", release.display()),
                "hold",
            ],
            &["drain-stdin"],
        );
        spec.wall_deadline = Duration::from_millis(80);
        spec.dialogue_cap[0] = 4096;
        let (diagnostics, mut received) = tokio::sync::mpsc::channel(32);
        spec.diagnostics = Some(diagnostics);
        let observed = CancelToken::new();
        spec.exit_observed[0] = Some(observed.clone());
        let (outcome, ()) = tokio::join!(run_session(spec), async {
            // The root's wall stop must precede the descendant's overflow.
            observed.cancelled().await;
            std::fs::write(&release, b"").unwrap();
        });
        trace(&outcome);
        assert_eq!(
            stop(&outcome, Role::S).unwrap().1,
            StopCause::OwnLimit(OwnLimit::Wall)
        );
        let mut overflow_diagnostics = 0;
        while let Ok(diagnostic) = received.try_recv() {
            if diagnostic.role == Role::S
                && diagnostic.cause == StopCause::OwnLimit(OwnLimit::DialogueOutput)
            {
                overflow_diagnostics += 1;
            }
        }
        assert_eq!(overflow_diagnostics, 1);
        assert!(stop(&outcome, Role::I).is_none());
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .discarded_over_cap,
            1
        );
    });
}

#[cfg(debug_assertions)]
#[test]
fn sleeping_roots_do_not_poll_the_coordinator() {
    test(async {
        for polling in [false, true] {
            let mut spec = spec(&["sleep:500"], &["sleep:500"]);
            for options in &mut spec.spawn_options {
                options.force_polling_observer = polling;
            }
            let outcome = run_session(spec).await;
            trace(&outcome);
            assert!(
                outcome.coordinator_wakeups < 100,
                "polling={polling}: {} wakeups",
                outcome.coordinator_wakeups
            );
            assert!(outcome.elapsed >= Duration::from_millis(500));
        }
    });
}

#[test]
fn recovery_preserves_session_trigger_or_none_without_inventing_cancellation() {
    test(async {
        for cancelled in [false, true] {
            let directory = TranscriptDirectory::new();
            std::fs::create_dir_all(&directory.0).unwrap();
            let ready = directory.0.join("root-ready");
            let mut spec = spec(
                &[&format!("identity-file:{}", ready.display()), "hold"],
                &["drain-stdin"],
            );
            spec.spawn_options[0].observer_never_ready = true;
            spec.session_deadline = Some(Duration::from_millis(80));
            let cancel = spec.cancel.clone();
            let (outcome, ()) = tokio::join!(run_session(spec), async {
                if cancelled {
                    until(|| ready.exists()).await;
                    cancel.cancel();
                }
            });
            let run = root(&outcome.solution);
            assert_eq!(
                run.stop_trigger,
                cancelled.then_some(StopTrigger::Cancelled)
            );
            assert!(run.errors.contains(&SupervisorError::ReapTimeout));
            assert_eq!(
                stop(&outcome, Role::S).unwrap().1,
                if cancelled {
                    StopCause::Teardown(Teardown::Cancel)
                } else {
                    StopCause::SessionLimit
                }
            );
            assert_eq!(
                outcome
                    .record
                    .events()
                    .iter()
                    .any(|e| matches!(e.fact, Fact::CancelRequested)),
                cancelled
            );
            assert!(
                !run.notes
                    .contains(&Note::LateTrigger(StopTrigger::Cancelled))
            );
            assert!(
                outcome.elapsed < Duration::from_millis(2850),
                "{:?}",
                outcome.elapsed
            );
        }
    });
}

struct TranscriptDirectory(std::path::PathBuf);
impl TranscriptDirectory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "session-transcript-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        assert!(!path.exists());
        Self(path)
    }
    fn index(&self, direction: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.0.join(format!("{direction}.jsonl")))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
impl Drop for TranscriptDirectory {
    fn drop(&mut self) {
        if self.0.exists() {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

#[test]
fn transcript_raw_bytes_offsets_and_root_events_are_complete() {
    test(async {
        let directory = TranscriptDirectory::new();
        let mut spec = spec(&["write-hex:stdout:4096:ff"], &["read-exactly:4096"]);
        spec.transcript = Some(directory.0.clone());
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert!(matches!(
            outcome.transcript.status,
            TranscriptStatus::Complete
        ));
        assert!(!outcome.transcript.excerpt);
        assert_eq!(
            std::fs::read(directory.0.join("s2i.bin")).unwrap(),
            vec![0xff; 4096]
        );
        assert!(
            std::fs::read(directory.0.join("i2s.bin"))
                .unwrap()
                .is_empty()
        );
        let index = directory.index("s2i");
        let mut read = 0;
        let mut forwarded = 0;
        let mut time = 0;
        for entry in &index {
            let observed = entry["time_ns"].as_u64().unwrap();
            assert!(observed >= time);
            time = observed;
            match entry["kind"].as_str().unwrap() {
                "chunk" => {
                    assert_eq!(entry["offset"], read);
                    assert_eq!(entry["file_offset"], read);
                    read += entry["length"].as_u64().unwrap();
                }
                "forwarded" => {
                    assert_eq!(entry["offset"], forwarded);
                    forwarded += entry["length"].as_u64().unwrap();
                }
                _ => {}
            }
        }
        assert_eq!(read, 4096);
        assert_eq!(
            forwarded,
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded
        );
        assert!(index.iter().any(|e| e["kind"] == "eof"));
        assert!(index.iter().any(|e| e["kind"] == "half_close"));
        let final_record = index.last().unwrap();
        assert_eq!(final_record["kind"], "final");
        assert_eq!(final_record["read_offset"], read);
        assert_eq!(final_record["forwarded_offset"], forwarded);
        let events = directory.index("events");
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|e| e["time_ns"].is_u64()));
        for role in ["S", "I"] {
            assert!(
                events
                    .iter()
                    .any(|e| e["event"]["fact"]["Started"]["role"] == role)
            );
            assert!(
                events
                    .iter()
                    .any(|e| e["event"]["fact"]["RootExited"]["role"] == role)
            );
        }
    });
}

#[cfg(debug_assertions)]
#[test]
fn fixture_09_blocked_transcript_queue_does_not_block_forwarding() {
    test(async {
        let directory = TranscriptDirectory::new();
        let mut spec = spec(&["write:stdout:262144"], &["read-exactly:262144"]);
        spec.transcript = Some(directory.0.clone());
        spec.transcript_test.writer_delay = Duration::from_millis(400);
        spec.transcript_test.queue_records = Some(1);
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            262144
        );
        for run in [&outcome.solution, &outcome.interactor] {
            assert!(root(run).timing.wall.unwrap() < Duration::from_millis(350));
        }
        assert!(matches!(
            outcome.transcript.status,
            TranscriptStatus::Gapped
        ));
        assert!(outcome.transcript.excerpt);
        let stats = &outcome.transcript.directions[0];
        assert_eq!(stats.gaps.len(), 1);
        assert!(stats.gaps[0].records > 1);
        assert!(stats.further_gaps.is_none());
        assert_eq!(
            directory
                .index("s2i")
                .iter()
                .filter(|e| e["kind"] == "gap")
                .count(),
            1
        );
    });
}

#[cfg(debug_assertions)]
#[test]
fn fixture_10_permanent_transcript_failure_is_reported_in_outcome() {
    test(async {
        let directory = TranscriptDirectory::new();
        let mut spec = spec(&["write:stdout:262144"], &["read-exactly:262144"]);
        spec.transcript = Some(directory.0.clone());
        spec.transcript_test.fail_after_bytes = Some(0);
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            262144
        );
        assert!(matches!(
            outcome.transcript.status,
            TranscriptStatus::Failed { at_offset: 0, .. }
        ));
        assert!(outcome.transcript.excerpt);
        assert!(stop(&outcome, Role::S).is_none());
        assert!(stop(&outcome, Role::I).is_none());
    });
}

#[cfg(debug_assertions)]
#[test]
fn fixture_11_cancellation_records_the_exact_partial_forward() {
    test(async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let ready = directory.0.join("writer-ready");
        let mut spec = spec(
            &[
                "write:stdout:8192",
                &format!("identity-file:{}", ready.display()),
                "write:stdout:253952",
                "hold",
            ],
            &["hold"],
        );
        spec.transcript = Some(directory.0.clone());
        // Darwin cannot resize pipes. Pause at the same exact 4096-byte prefix
        // on both kernels, then cancel while the remainder is still queued.
        let partial = CancelToken::new();
        spec.relay_pause_after = Some((4096, partial.clone()));
        let gate = CancelToken::new();
        spec.relay_start_gate = Some(gate.clone());
        let cancel = spec.cancel.clone();
        let (outcome, ()) = tokio::join!(run_session(spec), async {
            // Ensure the first read exceeds 4096 before releasing the relay.
            until(|| ready.exists()).await;
            gate.cancel();
            partial.cancelled().await;
            cancel.cancel();
        });
        trace(&outcome);
        assert!(matches!(
            outcome.transcript.status,
            TranscriptStatus::Complete
        ));
        let stats = outcome.solution_to_interactor.as_relay().unwrap();
        let index = directory.index("s2i");
        let chunks: Vec<_> = index.iter().filter(|e| e["kind"] == "chunk").collect();
        let writes: Vec<_> = index.iter().filter(|e| e["kind"] == "forwarded").collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0]["length"], 4096);
        assert!(chunks[0]["length"].as_u64().unwrap() > 4096);
        let forwarded: u64 = writes.iter().map(|e| e["length"].as_u64().unwrap()).sum();
        assert_eq!(forwarded, stats.bytes_forwarded);
        assert!(stats.bytes_read > forwarded);
        assert_eq!(index.last().unwrap()["forwarded_offset"], forwarded);
        assert_eq!(index.last().unwrap()["read_offset"], stats.bytes_read);
        assert_eq!(
            std::fs::read(directory.0.join("s2i.bin")).unwrap().len() as u64,
            stats.bytes_read
        );
        let events = directory.index("events");
        assert_eq!(
            events
                .iter()
                .filter(|e| e["event"]["fact"].get("StopRequested").is_some())
                .count(),
            2
        );
    });
}

#[test]
fn fixture_12_metadata_saturation_is_bounded() {
    test_with_timeout(Duration::from_secs(90), async {
        let directory = TranscriptDirectory::new();
        let mut spec = spec(&["pingpong:100000"], &["pong:100000"]);
        spec.wall_deadline = Duration::from_secs(60);
        spec.transcript = Some(directory.0.clone());
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert!(matches!(
            outcome.transcript.status,
            TranscriptStatus::Gapped
        ));
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            100000
        );
        assert_eq!(
            outcome
                .interactor_to_solution
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            100000
        );
        for (name, stats) in ["s2i", "i2s"]
            .into_iter()
            .zip(&outcome.transcript.directions)
        {
            assert_eq!(stats.retained_records, 65_536);
            assert!(stats.retained_bytes <= 1024 * 1024);
            assert!(stats.gaps.len() <= 64);
            let index = directory.index(name);
            assert_eq!(
                index
                    .iter()
                    .filter(|e| e["kind"] != "gap" && e["kind"] != "further_gaps")
                    .count() as u64,
                stats.retained_records
            );
            assert!(index.iter().filter(|e| e["kind"] == "gap").count() <= 64);
            assert!(index.iter().filter(|e| e["kind"] == "further_gaps").count() <= 1);
            assert!(
                stats
                    .gaps
                    .iter()
                    .chain(stats.further_gaps.iter())
                    .map(|g| g.records)
                    .sum::<u64>()
                    >= 100000
            );
            assert_eq!(index.last().unwrap()["forwarded_offset"], 100000);
        }
    });
}

#[test]
fn transcript_byte_retention_cap_is_an_excerpt() {
    test(async {
        let directory = TranscriptDirectory::new();
        let mut spec = spec(&["write:stdout:2097152"], &["read-exactly:2097152"]);
        spec.transcript = Some(directory.0.clone());
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            2097152
        );
        assert!(matches!(
            outcome.transcript.status,
            TranscriptStatus::Gapped
        ));
        assert!(outcome.transcript.excerpt);
        let stats = &outcome.transcript.directions[0];
        assert!(stats.retained_bytes <= 1024 * 1024);
        assert_eq!(
            std::fs::metadata(directory.0.join("s2i.bin"))
                .unwrap()
                .len(),
            stats.retained_bytes
        );
        assert!(stats.gaps.len() <= 64);
    });
}

#[test]
fn idle_stops_both_sleeping_roots_in_role_order() {
    test(async {
        let mut spec = spec(&["hold"], &["hold"]);
        spec.idle = Some(Duration::from_millis(80));
        let outcome = run_session(spec).await;
        trace(&outcome);
        let (s, s_cause) = stop(&outcome, Role::S).unwrap();
        let (i, i_cause) = stop(&outcome, Role::I).unwrap();
        assert_eq!(s_cause, StopCause::Idle);
        assert_eq!(i_cause, StopCause::Idle);
        assert_eq!(i, s + 1);
        assert_eq!(
            outcome.relay_limits,
            RelayLimits::Available {
                dialogue_cap: [64 * 1024 * 1024; 2],
                idle: Some(Duration::from_millis(80)),
            }
        );
        let idle = outcome.idle.unwrap();
        assert_eq!(idle.directions, [DialogueSnapshot::default(); 2]);
        assert!(idle.elapsed >= Duration::from_millis(80));
        assert!(outcome.elapsed < Duration::from_secs(1));
    });
}

#[test]
fn ten_thousand_exchanges_do_not_wake_the_coordinator_per_chunk_or_fire_idle() {
    test(async {
        // Nine pauses force at least 900 ms of dialogue even on fast hosts.
        // Only the initiator sleeps; the responder stays alive waiting for
        // the next batch. Each 100 ms gap is well below the idle window.
        let mut solution = Vec::new();
        for batch in 0..10 {
            if batch != 0 {
                solution.push("sleep:100");
            }
            solution.push("pingpong:1000");
        }
        let mut spec = spec(&solution, &["pong:10000"]);
        spec.idle = Some(Duration::from_millis(500));
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert_eq!(
            outcome
                .solution_to_interactor
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            10000
        );
        assert_eq!(
            outcome
                .interactor_to_solution
                .as_relay()
                .unwrap()
                .bytes_forwarded,
            10000
        );
        assert!(stop(&outcome, Role::S).is_none());
        assert!(stop(&outcome, Role::I).is_none());
        assert!(outcome.idle.is_none());
        assert!(
            outcome.coordinator_wakeups < 1000,
            "{} wakeups",
            outcome.coordinator_wakeups
        );
        assert!(outcome.elapsed > Duration::from_millis(500));
    });
}

#[test]
fn idle_reports_queued_bytes_and_disables_after_one_root_exits() {
    test(async {
        let mut blocked = spec(&["write:stdout:262144", "hold"], &["hold"]);
        blocked.idle = Some(Duration::from_millis(80));
        let outcome = run_session(blocked).await;
        trace(&outcome);
        assert_eq!(stop(&outcome, Role::S).unwrap().1, StopCause::Idle);
        let snapshot = outcome.idle.unwrap().directions[0];
        assert!(snapshot.bytes_forwarded > 0);
        assert!(snapshot.bytes_queued > 0);
        assert_eq!(
            snapshot.bytes_read,
            snapshot.bytes_forwarded + snapshot.bytes_queued
        );
        let mut one_alive = spec(&["exit:0"], &["hold"]);
        one_alive.idle = Some(Duration::from_millis(80));
        one_alive.peer_allowance = Duration::from_millis(160);
        let outcome = run_session(one_alive).await;
        trace(&outcome);
        assert_eq!(
            stop(&outcome, Role::I).unwrap().1,
            StopCause::PeerAllowanceExpired
        );
        assert!(outcome.idle.is_none());
    });
}

#[test]
fn direct_mode_echo_works_and_reports_unavailable_statistics() {
    test(async {
        let mut spec = spec(
            &["pingpong:10000", "write:stderr:4"],
            &["pong:10000", "write:stderr:4"],
        );
        spec.mode = SessionMode::Direct;
        spec.dialogue_cap = [0; 2];
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert!(matches!(
            outcome.solution_to_interactor,
            DirectionStatistics::Unavailable
        ));
        assert!(matches!(
            outcome.interactor_to_solution,
            DirectionStatistics::Unavailable
        ));
        assert!(matches!(
            outcome.transcript.status,
            TranscriptStatus::Unavailable
        ));
        assert_eq!(outcome.relay_limits, RelayLimits::Unavailable);
        for role in [Role::S, Role::I] {
            assert!(stop(&outcome, role).is_none());
        }
        for run in [&outcome.solution, &outcome.interactor] {
            assert_eq!(root(run).status, Termination::Exited { code: 0 });
            assert_eq!(root(run).stderr.as_ref().unwrap().retained, b"xxxx");
        }
    });
}

#[test]
fn direct_mode_has_child_to_child_eof_and_no_final_transport_drain() {
    test(async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let release = directory.0.join("release");
        let mut spec = spec(
            &[
                "write:stdout:4096",
                "close:stdout",
                &format!("wait-file:{}", release.display()),
            ],
            &["drain-stdin"],
        );
        spec.mode = SessionMode::Direct;
        let observed = CancelToken::new();
        spec.exit_observed[1] = Some(observed.clone());
        let (outcome, ()) = tokio::join!(run_session(spec), async {
            observed.cancelled().await;
            std::fs::write(&release, b"").unwrap();
        });
        trace(&outcome);
        assert!(exited(&outcome, Role::I).0 < exited(&outcome, Role::S).0);
        assert!(stop(&outcome, Role::S).is_none());
        let mut inherited = spec_for_direct_inherited_writer();
        inherited.peer_allowance = Duration::from_millis(100);
        let outcome = run_session(inherited).await;
        trace(&outcome);
        assert!(outcome.elapsed < Duration::from_millis(450));
    });
}
fn spec_for_direct_inherited_writer() -> SessionSpec {
    let mut spec = spec(&["spawn:1:hold", "exit:0"], &["exit:0"]);
    spec.mode = SessionMode::Direct;
    spec
}

#[test]
fn direct_mode_transcript_and_idle_requests_are_rejected_before_start() {
    test(async {
        for transcript in [false, true] {
            let directory = TranscriptDirectory::new();
            let mut spec = spec(&["hold"], &["hold"]);
            spec.mode = SessionMode::Direct;
            if transcript {
                spec.transcript = Some(directory.0.clone());
            } else {
                spec.idle = Some(Duration::from_millis(1));
            }
            let outcome = run_session(spec).await;
            trace(&outcome);
            for run in [&outcome.solution, &outcome.interactor] {
                assert!(matches!(
                    run,
                    RunRecord::NotStarted(StartFailure {
                        kind: StartFailureKind::Unsupported,
                        ..
                    })
                ));
            }
            assert!(!directory.0.exists());
        }
    });
}

async fn tiny_exchange_latency(exchanges: u32, ceiling: Option<Duration>) {
    for mode in [SessionMode::Relay, SessionMode::Direct] {
        let mut spec = spec(
            &[&format!("pingpong:{exchanges}")],
            &[&format!("pong:{exchanges}")],
        );
        spec.mode = mode;
        spec.wall_deadline = Duration::from_secs(60);
        let outcome = run_session(spec).await;
        trace(&outcome);
        for run in [&outcome.solution, &outcome.interactor] {
            assert_eq!(root(run).status, Termination::Exited { code: 0 });
        }
        // Write through the OS stream so successful tests retain the numbers
        // in CI logs even when libtest's output capture is enabled.
        writeln!(
            std::io::stdout().lock(),
            "{mode:?}: {exchanges} round trips, total {:.6} s, {:.3} us/round trip, coordinator wakeups {}",
            outcome.elapsed.as_secs_f64(),
            outcome.elapsed.as_secs_f64() * 1e6 / f64::from(exchanges),
            outcome.coordinator_wakeups
        )
        .unwrap();
        if let Some(ceiling) = ceiling {
            assert!(
                outcome.elapsed < ceiling,
                "{mode:?}: {} us/round trip exceeds generous CI ceiling",
                outcome.elapsed.as_secs_f64() * 1e6 / f64::from(exchanges)
            );
        }
    }
}

#[test]
fn fixture_17_ten_thousand_exchange_latency_ci() {
    test_with_timeout(Duration::from_secs(90), async {
        // 3 ms per round trip allows heavily loaded shared CI hosts.
        tiny_exchange_latency(10_000, Some(Duration::from_secs(30))).await;
    });
}

#[test]
#[ignore = "fixture 17: full latency benchmark without a performance threshold"]
fn fixture_17_tiny_exchange_latency_benchmark() {
    test_with_timeout(
        Duration::from_secs(130),
        tiny_exchange_latency(100_000, None),
    );
}

#[test]
fn direct_mode_preserves_wall_peer_session_and_cancel_stops() {
    test(async {
        for cause in [
            StopCause::OwnLimit(OwnLimit::Wall),
            StopCause::PeerAllowanceExpired,
            StopCause::SessionLimit,
            StopCause::Teardown(Teardown::Cancel),
        ] {
            let directory = TranscriptDirectory::new();
            std::fs::create_dir_all(&directory.0).unwrap();
            let s_ready = directory.0.join("s-ready");
            let i_ready = directory.0.join("i-ready");
            let mut spec = if cause == StopCause::PeerAllowanceExpired {
                spec(&["exit:0"], &["hold"])
            } else {
                spec(
                    &[
                        "write:stdout:4096",
                        &format!("identity-file:{}", s_ready.display()),
                        "write:stdout:258048",
                        "hold",
                    ],
                    &[
                        "write:stdout:4096",
                        &format!("identity-file:{}", i_ready.display()),
                        "write:stdout:258048",
                        "hold",
                    ],
                )
            };
            spec.mode = SessionMode::Direct;
            match cause {
                StopCause::OwnLimit(OwnLimit::Wall) => {
                    spec.wall_deadline = Duration::from_millis(80)
                }
                StopCause::PeerAllowanceExpired => spec.peer_allowance = Duration::from_millis(80),
                StopCause::SessionLimit => spec.session_deadline = Some(Duration::from_millis(80)),
                _ => {}
            }
            let cancel = spec.cancel.clone();
            let (outcome, ()) = tokio::join!(run_session(spec), async {
                if cause == StopCause::Teardown(Teardown::Cancel) {
                    until(|| s_ready.exists() && i_ready.exists()).await;
                    cancel.cancel();
                }
            });
            trace(&outcome);
            let role = if cause == StopCause::PeerAllowanceExpired {
                Role::I
            } else {
                Role::S
            };
            assert_eq!(stop(&outcome, role).unwrap().1, cause);
            assert!(outcome.elapsed < Duration::from_secs(2));
        }
    });
}

#[test]
fn transcript_directory_creation_failure_is_setup_before_admission() {
    test(async {
        let directory = TranscriptDirectory::new();
        std::fs::create_dir_all(&directory.0).unwrap();
        let blocked = directory.0.join("regular-file");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let mut spec = spec(&["pingpong:1000"], &["pong:1000"]);
        spec.transcript = Some(blocked);
        let outcome = run_session(spec).await;
        assert_setup_not_started(&outcome);
    });
}

fn assert_setup_not_started(outcome: &SessionOutcome) {
    trace(outcome);
    for run in [&outcome.solution, &outcome.interactor] {
        assert!(matches!(
            run,
            RunRecord::NotStarted(StartFailure {
                kind: StartFailureKind::Setup,
                ..
            })
        ));
    }
    assert_eq!(debug_counters().slots_in_use, 0);
    assert_eq!(debug_counters().credits_in_use, 1);
}

#[test]
fn every_transcript_path_rejects_existing_files_fifos_and_symlinks_before_admission() {
    test(async {
        for name in [
            "s2i.bin",
            "i2s.bin",
            "s2i.jsonl",
            "i2s.jsonl",
            "events.jsonl",
        ] {
            for kind in ["file", "fifo", "symlink"] {
                let directory = TranscriptDirectory::new();
                std::fs::create_dir_all(&directory.0).unwrap();
                let path = directory.0.join(name);
                let sentinel = directory.0.join("sentinel");
                std::fs::write(&sentinel, b"untouched").unwrap();
                match kind {
                    "file" => std::fs::write(&path, b"untouched").unwrap(),
                    "fifo" => assert!(
                        std::process::Command::new("mkfifo")
                            .arg(&path)
                            .status()
                            .unwrap()
                            .success()
                    ),
                    "symlink" => std::os::unix::fs::symlink(&sentinel, &path).unwrap(),
                    _ => unreachable!(),
                }
                let mut spec = spec(&["hold"], &["hold"]);
                spec.transcript = Some(directory.0.clone());
                let outcome = run_session(spec).await;
                assert_setup_not_started(&outcome);
                assert_eq!(std::fs::read(&sentinel).unwrap(), b"untouched");
                if kind == "file" {
                    assert_eq!(std::fs::read(&path).unwrap(), b"untouched");
                }
            }
        }
    });
}

#[cfg(debug_assertions)]
#[test]
fn twenty_transcript_writers_delayed_past_deadline_return_credits_and_threads() {
    test_with_timeout(Duration::from_secs(120), async {
        quiescent().await;
        let credits = debug_counters().credits_in_use;
        let threads = thread_count();
        for _ in 0..20 {
            let directory = TranscriptDirectory::new();
            let mut spec = spec(&["exit:0"], &["exit:0"]);
            spec.transcript = Some(directory.0.clone());
            spec.transcript_test.writer_delay = Duration::from_secs(30);
            let outcome = run_session(spec).await;
            assert!(matches!(
                outcome.transcript.status,
                TranscriptStatus::Failed { .. }
            ));
            // Drain (500 ms) plus the transcript finalization window (1 s), with slack.
            assert!(outcome.elapsed < Duration::from_secs(3));
            quiescent().await;
            assert_eq!(debug_counters().credits_in_use, credits);
            until(|| thread_count() == threads).await;
            assert_eq!(thread_count(), threads);
        }
    });
}

#[cfg(debug_assertions)]
#[test]
fn dropping_session_joins_delayed_transcript_workers_and_aborts_relays() {
    test(async {
        let directory = TranscriptDirectory::new();
        let mut spec = spec(&["hold"], &["hold"]);
        spec.transcript = Some(directory.0.clone());
        spec.transcript_test.writer_delay = Duration::from_secs(30);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), run_session(spec))
                .await
                .is_err()
        );
        quiescent().await;
        assert_eq!(debug_counters().credits_in_use, 1);
    });
}

#[test]
fn transcript_process_events_are_written_while_roots_are_alive() {
    test(async {
        let directory = TranscriptDirectory::new();
        let mut spec = spec(&["hold"], &["hold"]);
        spec.transcript = Some(directory.0.clone());
        let cancel = spec.cancel.clone();
        let (outcome, ()) = tokio::join!(run_session(spec), async {
            loop {
                let bytes =
                    std::fs::read_to_string(directory.0.join("events.jsonl")).unwrap_or_default();
                if bytes.lines().count() == 2 {
                    let entries = directory.index("events");
                    assert!(
                        entries
                            .iter()
                            .all(|entry| entry["event"]["fact"].get("Started").is_some())
                    );
                    cancel.cancel();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });
        assert!(matches!(
            outcome.transcript.status,
            TranscriptStatus::Complete
        ));
        assert_eq!(directory.index("events").len(), 6);
    });
}

#[test]
fn inherited_dialogue_handles_through_final_drain_leave_transcript_complete() {
    test(async {
        let directory = TranscriptDirectory::new();
        let mut spec = spec(
            &["spawn:1:hold", "write:stdout:1024", "exit:0"],
            &["spawn:1:hold", "exit:0"],
        );
        spec.transcript = Some(directory.0.clone());
        let outcome = run_session(spec).await;
        trace(&outcome);
        assert!(outcome.elapsed >= Duration::from_millis(475));
        assert!(outcome.elapsed < Duration::from_secs(2));
        assert!(
            matches!(outcome.transcript.status, TranscriptStatus::Complete),
            "{:?}",
            outcome.transcript
        );
        assert_eq!(directory.index("events").len(), 4);
        for (name, stats) in ["s2i", "i2s"].into_iter().zip([
            outcome.solution_to_interactor.as_relay().unwrap(),
            outcome.interactor_to_solution.as_relay().unwrap(),
        ]) {
            let entries = directory.index(name);
            let final_record = entries.last().unwrap();
            assert_eq!(final_record["kind"], "final");
            assert_eq!(final_record["read_offset"], stats.bytes_read);
            assert_eq!(final_record["forwarded_offset"], stats.bytes_forwarded);
            assert!(!stats.source_eof);
        }
        assert_eq!(debug_counters().credits_in_use, 1);
    });
}
