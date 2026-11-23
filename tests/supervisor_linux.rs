#![cfg(any(target_os = "linux", target_os = "macos"))]

use cpf_fixture::harness::fixture_path;
use cpforge::supervisor::{
    Aggregation, CancelToken, Cleanup, Completeness, DEADLINE_GRACE, LocalLimit, MetricKind,
    NotStarted, Note, ObserverKind, OutputSpec, ParentReader, ProgramSpec, ReadResult, RootOutcome,
    RunControls, Scope, SpawnOptions, Spawned, StdinSpec, StopTrigger, SupervisorError,
    Termination, Unit, UnverifiedReason, Violation, admit, debug_counters, global_bounds,
    init_process, spawn_root, stdout_pipe, wait_root as wait_root_controlled,
};
#[cfg(target_os = "linux")]
use cpforge::supervisor::{LimitKind, LimitValidation, capabilities};
use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// Each test owns its runtime and its children until quiescence, so fd/zombie
// checks cannot race another test's creation or runtime destruction.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn run_test(test: impl Future<Output = ()>) {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    init_process().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(allowance(10_000), async {
                test.await;
                quiescent().await;
            })
            .await
            .expect("supervisor test exceeded its watchdog");
        });
}

fn spec(steps: &[&str]) -> ProgramSpec {
    ProgramSpec {
        program: fixture_path(),
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

fn spawn(steps: &[&str], polling: bool) -> Spawned {
    spawn_root(
        spec(steps),
        admit().unwrap(),
        &SpawnOptions {
            force_polling_observer: polling,
            ..SpawnOptions::default()
        },
    )
    .unwrap()
}

async fn wait_root(root: Spawned) -> RootOutcome {
    wait_root_controlled(root, controls(Duration::from_secs(60))).await
}

fn controls(wall_deadline: Duration) -> RunControls {
    RunControls {
        wall_deadline,
        cancel: CancelToken::new(),
        events: None,
    }
}

fn success(outcome: &RootOutcome, code: i64) {
    assert_eq!(outcome.status, Termination::Exited { code });
    assert!(outcome.errors.is_empty(), "{outcome:?}");
    assert!(outcome.measurements.iter().all(|m| m.value.is_some()));
    let timing = outcome.timing;
    assert!(timing.exit_observed.unwrap() >= timing.create_end);
    assert!(timing.create_end <= Instant::now());
}

#[derive(Debug)]
struct Process {
    pid: u32,
    state: char,
    ppid: u32,
    pgrp: u32,
    start: u64,
}

#[cfg(target_os = "linux")]
fn process(pid: u32) -> Option<Process> {
    // A process can exit while it is being read: a missing, empty or truncated
    // stat file means "gone", never a test failure.
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    Some(Process {
        pid,
        state: fields.first()?.chars().next()?,
        ppid: fields.get(1)?.parse().ok()?,
        pgrp: fields.get(2)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(target_os = "linux")]
fn processes() -> Vec<Process> {
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| {
            let pid = entry.ok()?.file_name().to_str()?.parse().ok()?;
            process(pid)
        })
        .collect()
}

// Test observations include zombies to distinguish them from live members.
#[cfg(target_os = "macos")]
fn process(pid: u32) -> Option<Process> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: output has room for the entire structure; arg=1 includes zombies.
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            1,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if count != size {
        return None;
    }
    // SAFETY: the exact structure size was returned above.
    let info = unsafe { info.assume_init() };
    Some(Process {
        pid,
        state: if info.pbi_status == libc::SZOMB {
            'Z'
        } else {
            'R'
        },
        ppid: info.pbi_ppid,
        pgrp: info.pbi_pgid,
        start: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
    })
}

#[cfg(target_os = "macos")]
fn processes() -> Vec<Process> {
    let mut pids = vec![0_i32; 65536];
    let capacity = std::mem::size_of_val(pids.as_slice());
    // SAFETY: errno is thread-local and the buffer has capacity bytes. The
    // literal 1 is Darwin's PROC_ALL_PIDS (not exported by pinned libc).
    let count = unsafe {
        *libc::__error() = 0;
        libc::proc_listpids(1, 0, pids.as_mut_ptr().cast(), capacity as i32)
    };
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert_eq!(errno, 0);
    assert!((1..capacity).contains(&(count as usize)));
    assert_eq!(count as usize % std::mem::size_of::<i32>(), 0);
    pids.truncate(count as usize / std::mem::size_of::<i32>());
    pids.into_iter()
        .filter(|pid| *pid > 0)
        .filter_map(|pid| process(pid as u32))
        .collect()
}

fn fd_count() -> usize {
    #[cfg(target_os = "linux")]
    let path = "/proc/self/fd";
    #[cfg(target_os = "macos")]
    let path = "/dev/fd";
    fs::read_dir(path).unwrap().count()
}

#[cfg(target_os = "linux")]
fn thread_count() -> usize {
    fs::read_dir("/proc/self/task").unwrap().count()
}

#[cfg(target_os = "macos")]
fn thread_count() -> usize {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    // SAFETY: proc_pidinfo writes at most size bytes into this structure and
    // retains no pointer. Querying our own task does not acquire Mach rights.
    let count = unsafe {
        libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    assert_eq!(count, size, "short PROC_PIDTASKINFO reply");
    // SAFETY: the kernel returned the entire initialized structure.
    let threads = unsafe { info.assume_init() }.pti_threadnum;
    assert!(threads > 0);
    threads as usize
}

/// Hosted-runner allowance scale (execution contract §10.1): ×4 on macOS,
/// ×2 on hosted Linux CI, ×1 in the Linux container. Policy deadlines are
/// never scaled.
#[allow(dead_code)]
fn allowance_scale() -> u64 {
    if cfg!(target_os = "macos") {
        4
    } else if std::env::var_os("GITHUB_ACTIONS").is_some() {
        2
    } else {
        1
    }
}

// Scale runner scheduling allowances only; policy deadlines stay fixed.
fn allowance(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds * allowance_scale())
}

fn expected_cleanup() -> Cleanup {
    Cleanup::Confirmed
}

fn children() -> BTreeSet<u32> {
    processes()
        .into_iter()
        .filter(|p| p.ppid == std::process::id())
        .map(|p| p.pid)
        .collect()
}

async fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + allowance(1000);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "condition not met within 1 second"
        );
        // This polls an observable condition, rather than sleeping to coordinate.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

// Call at a quiescence point. Workers return their credits before their
// threads finish exiting, and macOS reclaims a joined thread asynchronously,
// so equality is awaited within the bound; a leak never converges.
async fn settled(fds: usize, threads: usize) {
    let deadline = Instant::now() + allowance(1000);
    loop {
        let observed = (fd_count(), thread_count());
        if observed == (fds, threads) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "fds and threads {observed:?}, expected {:?}",
            (fds, threads)
        );
        // This polls an observable condition, rather than sleeping to coordinate.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn drain(mut reader: ParentReader) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buffer = [0; 16384];
    loop {
        match reader.read_chunk(&mut buffer).await {
            ReadResult::Data(n) => result.extend_from_slice(&buffer[..n]),
            ReadResult::Eof => return result,
            ReadResult::Failed(error) => panic!("read failed: {error}"),
        }
    }
}

#[test]
fn inherited_sigchld_ignore_is_reset_before_spawn() {
    const MARKER: &str = "CPF_TEST_INHERITED_SIGCHLD";
    if std::env::var_os(MARKER).is_some() {
        assert!(matches!(
            admit(),
            Err(NotStarted::Setup(error)) if error.to_string() == "supervisor not initialised"
        ));
        run_test(async {
            // SAFETY: zero is valid initial storage, and a null act queries
            // SIGCHLD without changing the process's disposition.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: action is writable storage for the queried disposition.
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) },
                0
            );
            assert_eq!(action.sa_sigaction, libc::SIG_DFL);
            assert_eq!(action.sa_flags & libc::SA_NOCLDWAIT, 0);
            success(&wait_root(spawn(&["exit:3"], false)).await, 3);
        });
        return;
    }

    let _lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    init_process().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "inherited_sigchld_ignore_is_reset_before_spawn",
            "--nocapture",
        ])
        .env(MARKER, "1");
    // SAFETY: this closure runs only in the forked child, calls sigaction with
    // initialized storage, and performs no allocation on success.
    unsafe {
        command.pre_exec(|| {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_IGN;
            if libc::sigemptyset(&mut action.sa_mask) == -1
                || libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    assert!(command.status().unwrap().success());
}

#[test]
fn exits_and_signals_with_both_observers() {
    run_test(async {
        #[cfg(target_os = "linux")]
        let unforced_observer = {
            // SAFETY: pidfd_open takes integer arguments and returns an owned fd;
            // probing our own pid neither observes nor consumes any child status.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) };
            if fd == -1 {
                let error = std::io::Error::last_os_error();
                assert!(
                    matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)),
                    "unexpected pidfd probe failure: {error}"
                );
                eprintln!("pidfd unavailable ({error}); accepting polling observer");
                ObserverKind::Polling
            } else {
                // SAFETY: the successful probe returned a uniquely owned fd.
                drop(unsafe { OwnedFd::from_raw_fd(fd as i32) });
                ObserverKind::Pidfd
            }
        };
        #[cfg(target_os = "macos")]
        let unforced_observer = ObserverKind::Kqueue;
        for polling in [false, true] {
            let expected_observer = if polling {
                ObserverKind::Polling
            } else {
                unforced_observer
            };
            for code in [0, 3] {
                let outcome = wait_root(spawn(&[&format!("exit:{code}")], polling)).await;
                success(&outcome, code);
                assert_eq!(outcome.timing.observer, expected_observer);
            }
            for (name, signal) in [("KILL", 9), ("SEGV", 11)] {
                let outcome = wait_root(spawn(&[&format!("signal:{name}")], polling)).await;
                assert!(
                    matches!(outcome.status,
                    Termination::Signalled { signal: actual, .. } if actual == signal),
                    "{outcome:?}"
                );
                assert!(outcome.errors.is_empty(), "{outcome:?}");
                assert_eq!(outcome.timing.observer, expected_observer);
            }
        }
    });
}

#[test]
fn immediate_exit_remains_observable_and_unreaped_until_wait() {
    run_test(async {
        for polling in [false, true] {
            let root = spawn(&["exit:0"], polling);
            let pid = root.pid();
            // Polling path deliberately has no observer registration before exit.
            // The pidfd path also retains status when wait_root starts after exit.
            until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
            let outcome = wait_root(root).await;
            success(&outcome, 0);
            assert!(process(pid).is_none());
        }
    });
}

#[test]
fn failed_creation_and_stdin_setup_leave_no_children_or_pipe_fds() {
    run_test(async {
        // Warm the reactor before measuring resource stability.
        success(&wait_root(spawn(&["exit:0"], false)).await, 0);
        let before = children();
        let before_fds = fd_count();
        let pipe = stdout_pipe().unwrap();
        let mut missing = spec(&["exit:0"]);
        missing.program = std::env::current_dir()
            .unwrap()
            .join("nonexistent-b2b-program");
        missing.stdout = OutputSpec::Pipe(pipe.child);
        let admission = admit().unwrap();
        let failed_ticket = admission.ticket();
        assert!(matches!(
            spawn_root(missing, admission, &SpawnOptions::default()),
            Err(NotStarted::CreateFailed {
                api: "spawn",
                raw_error: libc::ENOENT
            })
        ));
        assert!(failed_ticket.outcome().await.is_none());
        assert_eq!(debug_counters().slots_in_use, 0);
        assert!(drain(pipe.parent).await.is_empty());
        let mut bad_stdin = spec(&["exit:0"]);
        bad_stdin.stdin = StdinSpec::File(
            std::env::current_dir()
                .unwrap()
                .join("nonexistent-b2b-stdin"),
        );
        assert!(matches!(
            spawn_root(bad_stdin, admit().unwrap(), &SpawnOptions::default()),
            Err(NotStarted::Setup(_))
        ));
        assert_eq!(children(), before);
        assert_eq!(fd_count(), before_fds);
    });
}

#[test]
fn post_exit_kill_removes_live_group_members() {
    run_test(async {
        let root = spawn(&["spawn:1:hold", "exit:0"], false);
        let pgid = root.pid();
        // Confirm the descendant exists before allowing the consuming wait.
        until(|| processes().iter().any(|p| p.pgrp == pgid && p.pid != pgid)).await;
        success(&wait_root(root).await, 0);
        until(|| {
            !processes()
                .iter()
                .any(|p| p.pgrp == pgid && !matches!(p.state, 'Z' | 'X'))
        })
        .await;
    });
}

#[test]
fn pipes_drain_concurrently_and_descendant_writers_close() {
    run_test(async {
        for stream in ["stdout", "stderr"] {
            let pipe = stdout_pipe().unwrap();
            let mut program = spec(&["spawn:1:hold", &format!("write:{stream}:100000"), "exit:0"]);
            if stream == "stdout" {
                program.stdout = OutputSpec::Pipe(pipe.child);
            } else {
                program.stderr = OutputSpec::Pipe(pipe.child);
            }
            let root = spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap();
            let (bytes, outcome) = tokio::join!(drain(pipe.parent), wait_root(root));
            assert_eq!(bytes, vec![b'x'; 100000]);
            success(&outcome, 0);
        }
    });
}

#[test]
fn dropped_owner_is_killed_and_reaped() {
    run_test(async {
        for polling in [false, true] {
            let root = spawn(&["hold"], polling);
            let pid = root.pid();
            let start = process(pid).unwrap().start;
            drop(root);
            until(|| process(pid).is_none_or(|p| p.start != start)).await;
        }
    });
}

#[test]
fn cancelled_wait_future_transfers_ownership() {
    run_test(async {
        let root = spawn(&["hold"], false);
        let pid = root.pid();
        let start = process(pid).unwrap().start;
        let mut wait = Box::pin(wait_root(root));
        // Poll once, ensuring cancellation happens inside the observer await.
        std::future::poll_fn(|cx| {
            assert!(wait.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(wait);
        until(|| process(pid).is_none_or(|p| p.start != start)).await;
    });
}

#[test]
fn creation_applies_cwd_complete_env_and_file_stdin() {
    run_test(async {
        #[cfg(target_os = "linux")]
        {
            let mut program = spec(&["hold"]);
            program.cwd = std::env::current_dir().unwrap().join("crates");
            program
                .env
                .push(("CPF_B2B_TEST".into(), "complete environment".into()));
            let cwd = program.cwd.clone();
            let root = spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap();
            let pid = root.pid();
            assert_eq!(fs::read_link(format!("/proc/{pid}/cwd")).unwrap(), cwd);
            until(|| {
                fs::read(format!("/proc/{pid}/environ")).unwrap()
                    == b"CPF_B2B_TEST=complete environment\0"
            })
            .await;
            drop(root);
            until(|| process(pid).is_none()).await;
        }
        #[cfg(target_os = "macos")]
        for executable in ["/bin/pwd", "/usr/bin/env"] {
            let pipe = stdout_pipe().unwrap();
            let mut program = spec(&[]);
            program.program = executable.into();
            program.cwd = std::env::current_dir().unwrap().join("crates");
            program
                .env
                .push(("CPF_TEST_ENV".into(), "complete environment".into()));
            let expected = if executable == "/bin/pwd" {
                format!("{}\n", fs::canonicalize(&program.cwd).unwrap().display())
            } else {
                "CPF_TEST_ENV=complete environment\n".into()
            };
            program.stdout = OutputSpec::Pipe(pipe.child);
            let root = spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap();
            let (bytes, outcome) = tokio::join!(drain(pipe.parent), wait_root(root));
            assert_eq!(bytes, expected.as_bytes());
            success(&outcome, 0);
        }

        let pipe = stdout_pipe().unwrap();
        let mut program = spec(&["drain-stdin"]);
        let input = std::env::current_dir().unwrap().join("README.md");
        let length = fs::metadata(&input).unwrap().len();
        program.stdin = StdinSpec::File(input);
        program.stdout = OutputSpec::Pipe(pipe.child);
        let root = spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap();
        let (bytes, outcome) = tokio::join!(drain(pipe.parent), wait_root(root));
        assert_eq!(bytes, format!("{length}\n").as_bytes());
        success(&outcome, 0);
    });
}

fn metric(outcome: &RootOutcome, kind: MetricKind) -> &cpforge::supervisor::Measurement {
    outcome
        .measurements
        .iter()
        .find(|m| m.kind == kind)
        .unwrap()
}

#[test]
fn wait4_reports_resident_memory_in_bytes() {
    run_test(async {
        let mut baseline = 0;
        for mib in [0, 64, 256] {
            let outcome = wait_root(spawn(&[&format!("touch-mem:{mib}")], false)).await;
            success(&outcome, 0);
            let measurement = metric(&outcome, MetricKind::PeakRss);
            assert_eq!(measurement.unit, Unit::Bytes);
            assert_eq!(measurement.scope, Scope::RootAndWaitedDescendants);
            assert_eq!(measurement.aggregation, Aggregation::MaxOfProcesses);
            assert_eq!(measurement.method, "wait4 ru_maxrss");
            let value = measurement.value.unwrap();
            if mib == 0 {
                baseline = value;
            }
            assert!(
                (mib * 1024 * 1024..=(mib + 16) * 1024 * 1024 + baseline).contains(&value),
                "{measurement:?}"
            );
            assert_eq!(
                measurement.completeness,
                if outcome.termination.cleanup == Cleanup::Confirmed {
                    Completeness::Final
                } else {
                    Completeness::Partial
                }
            );
        }
    });
}

#[test]
fn busy_fixture_reports_cpu_microseconds() {
    run_test(async {
        let outcome = wait_root(spawn(&["spin:150"], false)).await;
        success(&outcome, 0);
        let measurement = metric(&outcome, MetricKind::CpuTime);
        assert_eq!(measurement.unit, Unit::Microseconds);
        assert_eq!(measurement.scope, Scope::RootAndWaitedDescendants);
        assert_eq!(measurement.aggregation, Aggregation::Sum);
        let value = measurement.value.unwrap();
        assert!(value > 0);
        assert!(
            u128::from(value)
                <= (outcome.timing.wall.unwrap() + Duration::from_millis(100)).as_micros(),
            "{outcome:?}"
        );
    });
}

#[test]
fn four_hundred_runs_leave_no_zombies_or_descriptors() {
    run_test(async {
        // Warm up both observer paths before the quiescent baseline.
        for polling in [false, true] {
            success(&wait_root(spawn(&["exit:0"], polling)).await, 0);
        }
        let fds = fd_count();
        for _ in 0..200 {
            success(&wait_root(spawn(&["exit:0"], false)).await, 0);
        }
        async fn batch(polling: bool) {
            for _ in 0..50 {
                success(&wait_root(spawn(&["exit:0"], polling)).await, 0);
            }
        }
        tokio::join!(batch(false), batch(true), batch(false), batch(true));
        assert!(
            !processes()
                .iter()
                .any(|p| p.ppid == std::process::id() && p.state == 'Z')
        );
        assert_eq!(fd_count(), fds);
    });
}

async fn quiescent() {
    until(|| {
        let counters = debug_counters();
        counters.slots_in_use == 0 && counters.service_pending == 0 && counters.credits_in_use == 1
    })
    .await;
}

#[test]
fn four_slots_bound_admission_and_release_after_reap() {
    run_test(async {
        let mut roots = Vec::new();
        for _ in 0..4 {
            roots.push(spawn(&["hold"], false));
        }
        assert_eq!(debug_counters().slots_in_use, 4);
        assert!(matches!(admit(), Err(NotStarted::CleanupCapacity)));
        let root = roots.pop().unwrap();
        let ticket = root.ticket();
        let retained = ticket.clone();
        let pid = root.pid();
        drop(root);
        assert!(ticket.outcome().await.is_some());
        assert!(process(pid).is_none());
        // The outcome is published when the root finishes; the slot is
        // released once cleanup completes, which may be a moment later.
        until(|| debug_counters().slots_in_use == 3).await;
        let returned = admit().expect("reaping releases capacity even with tickets alive");
        assert!(matches!(admit(), Err(NotStarted::CleanupCapacity)));
        drop(returned);
        assert!(retained.outcome().await.is_some());
        drop(roots);
        quiescent().await;
    });
}

#[test]
fn dropped_exit_transfers_full_outcome_and_cloneable_ticket() {
    run_test(async {
        for polling in [false, true] {
            for already_exited in [false, true] {
                let root = spawn(&["exit:3"], polling);
                let pid = root.pid();
                let ticket = root.ticket();
                let another = ticket.clone();
                if already_exited {
                    until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
                }
                drop(root);
                let (outcome, clone_outcome) = tokio::join!(ticket.outcome(), another.outcome());
                let outcome = outcome.unwrap();
                // An immediate drop is permitted to kill the root before exec
                // reaches exit:3. The already-exited case must retain exit:3.
                if already_exited {
                    success(&outcome, 3);
                } else {
                    assert!(
                        matches!(
                            outcome.status,
                            Termination::Exited { code: 3 }
                                | Termination::Signalled { signal: 9, .. }
                        ),
                        "{outcome:?}"
                    );
                    assert!(outcome.measurements.iter().all(|m| m.value.is_some()));
                    assert!(outcome.errors.is_empty());
                }
                assert_eq!(
                    outcome
                        .notes
                        .iter()
                        .filter(|&&n| n == Note::Transferred)
                        .count(),
                    1
                );
                assert_eq!(clone_outcome.unwrap().status, outcome.status);
                assert!(outcome.timing.exit_observed.unwrap() >= outcome.timing.create_end);
                assert!(process(pid).is_none());
                quiescent().await;
            }
        }
    });
}

#[test]
fn cancelled_wait_ticket_records_sigkill_even_before_first_poll() {
    run_test(async {
        for polling in [false, true] {
            for polled in [false, true] {
                let root = spawn(&["spawn:1:hold", "hold"], polling);
                let pid = root.pid();
                until(|| processes().iter().any(|p| p.pgrp == pid && p.pid != pid)).await;
                let ticket = root.ticket();
                let wait = wait_root(root);
                if polled {
                    assert!(tokio::time::timeout(Duration::ZERO, wait).await.is_err());
                } else {
                    drop(wait);
                }
                let outcome = ticket.outcome().await.unwrap();
                assert!(
                    matches!(outcome.status, Termination::Signalled { signal: 9, .. }),
                    "{outcome:?}"
                );
                assert!(outcome.notes.contains(&Note::Transferred));
                assert!(outcome.errors.is_empty(), "{outcome:?}");
                assert!(outcome.measurements.iter().all(|m| m.value.is_some()));
                assert!(process(pid).is_none());
                until(|| {
                    !processes()
                        .iter()
                        .any(|p| p.pgrp == pid && !matches!(p.state, 'Z' | 'X'))
                })
                .await;
                quiescent().await;
            }
        }
    });
}

#[test]
fn dropped_ticket_does_not_abandon_cleanup_and_normal_completion_is_none() {
    run_test(async {
        let root = spawn(&["hold"], false);
        let pid = root.pid();
        let ticket = root.ticket();
        drop(ticket);
        drop(root);
        quiescent().await;
        assert!(process(pid).is_none());

        let root = spawn(&["exit:3"], false);
        let ticket = root.ticket();
        let (outcome, transferred) = tokio::join!(wait_root(root), ticket.outcome());
        success(&outcome, 3);
        assert!(transferred.is_none());
        assert!(!outcome.notes.contains(&Note::Transferred));
        quiescent().await;
    });
}

#[test]
fn init_is_idempotent_and_thread_credits_are_bounded_and_returned() {
    run_test(async {
        #[cfg(target_os = "linux")]
        let services = || {
            fs::read_dir("/proc/self/task")
                .unwrap()
                .filter(|task| {
                    fs::read_to_string(task.as_ref().unwrap().path().join("comm"))
                        .is_ok_and(|name| name.trim() == "cpf-cleanup")
                })
                .count()
        };
        #[cfg(target_os = "linux")]
        assert_eq!(services(), 1);
        init_process().unwrap();
        init_process().unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(services(), 1);
        assert_eq!(debug_counters().credits_in_use, 1);
        let credits = &global_bounds().unwrap().thread_credits;
        let remaining = credits.try_reserve(31).unwrap();
        assert_eq!(debug_counters().credits_in_use, 32);
        assert!(credits.try_reserve(1).is_none());
        assert!(credits.try_reserve(usize::MAX).is_none());
        drop(remaining);
        assert_eq!(debug_counters().credits_in_use, 1);
        assert!(credits.try_reserve(32).is_none());
        drop(credits.try_reserve(0).unwrap());
    });
}

#[test]
fn mixed_runs_leave_stable_fds_threads_slots_and_no_zombies() {
    run_test(async {
        async fn mixed(index: usize) {
            let root = spawn(
                if index % 5 == 0 {
                    &["exit:0"]
                } else {
                    &["hold"]
                },
                index % 2 == 0,
            );
            let pid = root.pid();
            let ticket = root.ticket();
            match index % 5 {
                0 => {
                    success(&wait_root(root).await, 0);
                    assert!(ticket.outcome().await.is_none());
                }
                1 => {
                    drop(root);
                    // Alternate retained and discarded outcomes.
                    if index % 2 == 0 {
                        assert!(ticket.outcome().await.is_some());
                    } else {
                        drop(ticket);
                    }
                }
                2 => {
                    let mut wait = Box::pin(wait_root(root));
                    std::future::poll_fn(|cx| {
                        assert!(wait.as_mut().poll(cx).is_pending());
                        std::task::Poll::Ready(())
                    })
                    .await;
                    drop(wait);
                    assert!(ticket.outcome().await.is_some());
                }
                mode => {
                    let controls = controls(if mode == 3 {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(5)
                    });
                    let expected = if mode == 3 {
                        StopTrigger::WallDeadline
                    } else {
                        StopTrigger::Cancelled
                    };
                    if mode == 4 {
                        controls.cancel.cancel();
                    }
                    let outcome = wait_root_controlled(root, controls).await;
                    assert_eq!(outcome.stop_trigger, Some(expected));
                    assert!(matches!(
                        outcome.status,
                        Termination::Signalled { signal: 9, .. }
                    ));
                    assert!(outcome.errors.is_empty(), "{outcome:?}");
                    assert!(ticket.outcome().await.is_none());
                }
            }
            quiescent().await;
            assert!(process(pid).is_none());
        }
        for index in 0..50 {
            mixed(index).await;
        }
        let fds = fd_count();
        let threads = thread_count();
        let counters = debug_counters();
        assert_eq!(counters.slots_in_use, 0);
        assert_eq!(counters.service_pending, 0);
        assert_eq!(counters.credits_in_use, 1); // Only the process-wide service.
        for _ in 0..5 {
            for index in 0..100 {
                mixed(index).await;
            }
            quiescent().await;
            settled(fds, threads).await;
            assert_eq!(debug_counters(), counters);
            assert!(
                !processes()
                    .iter()
                    .any(|p| p.ppid == std::process::id() && p.state == 'Z')
            );
        }
    });
}

fn killed(outcome: &RootOutcome, trigger: StopTrigger) {
    assert_eq!(outcome.stop_trigger, Some(trigger), "{outcome:?}");
    assert!(
        matches!(outcome.status, Termination::Signalled { signal: 9, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.termination.cleanup,
        expected_cleanup(),
        "{outcome:?}"
    );
    assert_eq!(outcome.termination.method, Some("killpg(SIGKILL)"));
    assert!(outcome.errors.is_empty(), "{outcome:?}");
}

#[test]
fn deadline_kills_hold_within_the_policy_bounds() {
    run_test(async {
        let outcome = wait_root_controlled(
            spawn(&["hold"], false),
            controls(Duration::from_millis(200)),
        )
        .await;
        killed(&outcome, StopTrigger::WallDeadline);
        assert!(matches!(
            outcome.violations.as_slice(),
            [Violation::WallDeadline { .. }]
        ));
        let requested = outcome.termination.requested_at.unwrap();
        let timer = outcome.timing.create_end + Duration::from_millis(200);
        assert!(requested >= timer);
        assert!(
            requested.duration_since(timer) <= allowance(50),
            "{outcome:?}"
        );
        assert!(
            outcome
                .timing
                .exit_observed
                .unwrap()
                .duration_since(requested)
                <= allowance(250)
        );
        assert_eq!(outcome.timing.grace, DEADLINE_GRACE);
        assert_eq!(
            outcome.timing.wall,
            Some(
                outcome
                    .timing
                    .exit_observed
                    .unwrap()
                    .duration_since(outcome.timing.create_end)
            )
        );
    });
}

#[test]
fn short_sleep_exits_without_enforcement_and_records_observed_wall() {
    run_test(async {
        let outcome = wait_root_controlled(
            spawn(&["sleep:50", "exit:0"], false),
            controls(Duration::from_secs(1)),
        )
        .await;
        success(&outcome, 0);
        assert_eq!(outcome.stop_trigger, None);
        assert!(outcome.violations.is_empty());
        let wall = outcome.timing.wall.unwrap();
        assert!(
            wall >= Duration::from_millis(50) && wall <= Duration::from_millis(50) + allowance(250),
            "{outcome:?}"
        );
        assert_eq!(outcome.termination.cleanup, expected_cleanup());
    });
}

#[test]
fn cancellation_is_persistent_and_mid_run_cancellation_kills() {
    run_test(async {
        let pipe = stdout_pipe().unwrap();
        let mut program = spec(&["write:stdout:1", "hold"]);
        program.stdout = OutputSpec::Pipe(pipe.child);
        let root = spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap();
        let controls = controls(Duration::from_secs(5));
        let token = controls.cancel.clone();
        let mut reader = pipe.parent;
        let (outcome, ()) = tokio::join!(wait_root_controlled(root, controls), async {
            let mut byte = [0];
            assert!(matches!(
                reader.read_chunk(&mut byte).await,
                ReadResult::Data(1)
            ));
            // The fixture has executed a step; no elapsed-time coordination.
            let mut waiter = Box::pin(token.cancelled());
            std::future::poll_fn(|cx| {
                assert!(waiter.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            drop(waiter); // abandoning one waiter cannot consume cancellation
            let clone = token.clone();
            token.cancel();
            token.cancel();
            assert!(clone.is_cancelled());
            tokio::join!(clone.cancelled(), token.cancelled());
        });
        killed(&outcome, StopTrigger::Cancelled);
        assert!(outcome.violations.is_empty());
    });
}

#[test]
fn cancellation_after_observable_exit_is_only_a_note() {
    run_test(async {
        for polling in [false, true] {
            let root = spawn(&["exit:0"], polling);
            until(|| process(root.pid()).is_some_and(|p| p.state == 'Z')).await;
            let controls = controls(Duration::from_secs(1));
            controls.cancel.cancel();
            let outcome = wait_root_controlled(root, controls).await;
            success(&outcome, 0);
            assert_eq!(outcome.stop_trigger, None);
            assert!(
                outcome
                    .notes
                    .contains(&Note::LateTrigger(StopTrigger::Cancelled))
            );
            assert!(outcome.violations.is_empty());
        }
    });
}

#[test]
fn deadline_removes_three_descendants_and_confirms_the_group_empty() {
    run_test(async {
        let root = spawn(&["spawn:3:hold", "hold"], false);
        let pgid = root.pid();
        until(|| {
            processes()
                .iter()
                .filter(|p| p.pgrp == pgid && p.pid != pgid)
                .count()
                == 3
        })
        .await;
        let members: Vec<_> = processes().into_iter().filter(|p| p.pgrp == pgid).collect();
        let outcome = wait_root_controlled(root, controls(Duration::from_millis(200))).await;
        killed(&outcome, StopTrigger::WallDeadline);
        for member in members {
            assert!(
                process(member.pid)
                    .is_none_or(|p| p.start != member.start || matches!(p.state, 'Z' | 'X'))
            );
        }
    });
}

#[test]
fn escaped_descendant_is_outside_the_cleanup_claim_and_is_killed_by_the_test() {
    run_test(async {
        struct Escaped(Process);
        impl Drop for Escaped {
            fn drop(&mut self) {
                if process(self.0.pid)
                    .is_some_and(|p| p.start == self.0.start && !matches!(p.state, 'Z' | 'X'))
                {
                    // SAFETY: the held live fixture identity has not been reused.
                    assert_eq!(unsafe { libc::kill(self.0.pid as i32, libc::SIGKILL) }, 0);
                }
            }
        }
        let root = spawn(&["spawn:1:escape,hold", "hold"], false);
        let pgid = root.pid();
        until(|| {
            processes()
                .iter()
                .any(|p| p.ppid == pgid && p.pgrp == p.pid && p.pid != pgid)
        })
        .await;
        let escaped = Escaped(
            processes()
                .into_iter()
                .find(|p| p.ppid == pgid && p.pgrp == p.pid)
                .unwrap(),
        );
        let pid = escaped.0.pid;
        let outcome = wait_root_controlled(root, controls(Duration::from_millis(200))).await;
        killed(&outcome, StopTrigger::WallDeadline);
        // Confirmed covers the process group only: setsid escapes containment.
        assert!(process(pid).is_some_and(|p| !matches!(p.state, 'Z' | 'X')));
        drop(escaped);
        until(|| process(pid).is_none_or(|p| matches!(p.state, 'Z' | 'X'))).await;
    });
}

#[test]
fn timer_and_exit_ready_together_are_reconciled_in_both_selection_orders() {
    run_test(async {
        for timer_first in [false, true] {
            for polling in [false, true] {
                // A zero deadline plus a gate gives two ready branches without
                // sleeping to arrange a race. Grace and over-grace judgments are
                // covered independently by the clock-driven unit test.
                let gate = CancelToken::new();
                let root = spawn_root(
                    spec(&["exit:0"]),
                    admit().unwrap(),
                    &SpawnOptions {
                        supervisor_gate: Some(gate.clone()),
                        timer_first,
                        force_polling_observer: polling,
                        ..SpawnOptions::default()
                    },
                )
                .unwrap();
                let pid = root.pid();
                let mut wait = Box::pin(wait_root_controlled(root, controls(Duration::ZERO)));
                std::future::poll_fn(|cx| {
                    assert!(wait.as_mut().poll(cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
                gate.cancel();
                let outcome = wait.await;
                success(&outcome, 0);
                assert_eq!(outcome.stop_trigger, None, "{outcome:?}");
                assert_eq!(outcome.enforced_at, None);
                if outcome.timing.wall.unwrap() <= DEADLINE_GRACE {
                    assert!(outcome.violations.is_empty());
                    assert!(outcome.notes.contains(&Note::DeadlineAmbiguous));
                } else {
                    assert_eq!(outcome.violations.len(), 1);
                }
            }
        }
    });
}

#[test]
fn reap_timeouts_transfer_identity_and_ticket_gets_the_physical_outcome() {
    run_test(async {
        for observer_never_ready in [true, false] {
            let gate = CancelToken::new();
            let root = spawn_root(
                spec(&["hold"]),
                admit().unwrap(),
                &SpawnOptions {
                    observer_never_ready,
                    reap_never_ready: !observer_never_ready,
                    exit_observation_gate: Some(gate.clone()),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
            let pid = root.pid();
            let ticket = root.ticket();
            let controls = controls(Duration::from_millis(10));
            let cancel = controls.cancel.clone();
            let (logical, ()) = tokio::join!(wait_root_controlled(root, controls), async {
                // Observe the already-issued deadline kill before a later trigger.
                until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
                cancel.cancel();
                gate.cancel();
            });
            assert!(
                logical
                    .notes
                    .contains(&Note::LateTrigger(StopTrigger::Cancelled))
            );
            let requested = logical.termination.requested_at.unwrap();
            assert!(Instant::now().duration_since(requested) >= Duration::from_secs(2));
            assert!(
                Instant::now().duration_since(requested) <= Duration::from_secs(2) + allowance(100)
            );
            assert_eq!(logical.status, Termination::Unknown);
            assert!(logical.errors.contains(&SupervisorError::ReapTimeout));
            assert_eq!(logical.stop_trigger, Some(StopTrigger::WallDeadline));
            assert!(logical.notes.contains(&Note::Transferred));
            if observer_never_ready {
                assert_eq!(
                    logical.termination.cleanup,
                    Cleanup::Unverified {
                        reason: UnverifiedReason::RootNotObserved
                    }
                );
                assert_eq!(logical.timing.wall, None);
            }
            let physical = ticket.outcome().await.unwrap();
            assert!(
                matches!(physical.status, Termination::Signalled { signal: 9, .. }),
                "{physical:?}"
            );
            assert_eq!(physical.stop_trigger, logical.stop_trigger);
            assert_eq!(physical.violations, logical.violations);
            assert!(physical.errors.contains(&SupervisorError::ReapTimeout));
            assert_eq!(
                physical.termination.cleanup,
                expected_cleanup(),
                "{physical:?}"
            );
            assert!(process(pid).is_none());
            // The earlier logical snapshot remains finalized.
            assert_eq!(logical.status, Termination::Unknown);
            quiescent().await;
        }
    });
}

#[test]
fn dropping_a_terminating_future_preserves_the_trigger_and_notes_cancellation() {
    run_test(async {
        let root = spawn_root(
            spec(&["hold"]),
            admit().unwrap(),
            &SpawnOptions {
                observer_never_ready: true,
                ..SpawnOptions::default()
            },
        )
        .unwrap();
        let pid = root.pid();
        let ticket = root.ticket();
        let mut wait = Box::pin(wait_root_controlled(root, controls(Duration::ZERO)));
        tokio::select! {
            outcome = &mut wait => panic!("injected observer unexpectedly completed: {outcome:?}"),
            _ = until(|| process(pid).is_some_and(|p| p.state == 'Z')) => {}
        }
        drop(wait);
        let outcome = ticket.outcome().await.unwrap();
        killed(&outcome, StopTrigger::WallDeadline);
        assert!(
            outcome
                .notes
                .contains(&Note::LateTrigger(StopTrigger::Cancelled))
        );
        assert!(outcome.notes.contains(&Note::Transferred));
        assert_eq!(outcome.violations.len(), 1);
        assert!(process(pid).is_none());
    });
}

#[test]
fn dropping_a_paused_wait_reconciles_an_already_exited_root() {
    run_test(async {
        for polling in [false, true] {
            let root = spawn_root(
                spec(&["exit:3"]),
                admit().unwrap(),
                &SpawnOptions {
                    supervisor_gate: Some(CancelToken::new()),
                    force_polling_observer: polling,
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
            let pid = root.pid();
            let ticket = root.ticket();
            let mut wait = Box::pin(wait_root(root));
            std::future::poll_fn(|cx| {
                assert!(wait.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
            drop(wait);
            let outcome = ticket.outcome().await.unwrap();
            success(&outcome, 3);
            assert_eq!(outcome.stop_trigger, None, "{outcome:?}");
            assert!(
                outcome
                    .notes
                    .contains(&Note::LateTrigger(StopTrigger::Cancelled))
            );
            assert!(outcome.notes.contains(&Note::Transferred));
            assert!(process(pid).is_none());
        }
    });
}

#[cfg(target_os = "linux")]
#[test]
fn slow_cleanup_transfers_at_the_original_kill_reap_deadline() {
    run_test(async {
        let gate = CancelToken::new();
        let root = spawn_root(
            spec(&["hold"]),
            admit().unwrap(),
            &SpawnOptions {
                exit_observation_gate: Some(gate.clone()),
                cleanup_entry_delay: Duration::from_millis(300),
                ..SpawnOptions::default()
            },
        )
        .unwrap();
        let pid = root.pid();
        let ticket = root.ticket();
        let (outcome, ()) = tokio::join!(
            wait_root_controlled(root, controls(Duration::ZERO)),
            async {
                until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
                // Hold observation until most of the original reap budget is spent.
                tokio::time::sleep(Duration::from_millis(1800)).await;
                gate.cancel();
            }
        );
        let elapsed = outcome.termination.requested_at.unwrap().elapsed();
        assert!(elapsed >= Duration::from_secs(2), "{elapsed:?}");
        assert!(
            elapsed <= Duration::from_secs(2) + allowance(100),
            "{elapsed:?}"
        );
        assert_eq!(outcome.stop_trigger, Some(StopTrigger::WallDeadline));
        assert_eq!(outcome.status, Termination::Unknown);
        assert_eq!(
            outcome.termination.cleanup,
            Cleanup::Unverified {
                reason: UnverifiedReason::ReapDeadline
            }
        );
        assert!(outcome.errors.contains(&SupervisorError::ReapTimeout));
        assert!(outcome.notes.contains(&Note::Transferred));
        let physical = ticket.outcome().await.unwrap();
        assert!(matches!(
            physical.status,
            Termination::Signalled { signal: 9, .. }
        ));
        assert!(process(pid).is_none());
    });
}

#[test]
fn post_exit_deadline_and_cancellation_do_not_add_late_trigger_notes() {
    run_test(async {
        let root = spawn_root(
            spec(&["exit:0"]),
            admit().unwrap(),
            &SpawnOptions {
                reap_never_ready: true,
                ..SpawnOptions::default()
            },
        )
        .unwrap();
        let pid = root.pid();
        let ticket = root.ticket();
        until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
        let controls = controls(Duration::from_millis(500));
        let cancel = controls.cancel.clone();
        let mut wait = Box::pin(wait_root_controlled(root, controls));
        // This poll observes the ready root and reaches the injected reap retry.
        std::future::poll_fn(|cx| {
            assert!(wait.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        cancel.cancel();
        let outcome = wait.await;
        assert!(
            outcome.timing.wall.unwrap() < Duration::from_millis(500),
            "{outcome:?}"
        );
        assert!(outcome.timing.create_end.elapsed() > Duration::from_millis(500));
        assert_eq!(outcome.stop_trigger, None);
        for trigger in [StopTrigger::WallDeadline, StopTrigger::Cancelled] {
            assert!(
                !outcome.notes.contains(&Note::LateTrigger(trigger)),
                "{outcome:?}"
            );
        }
        assert!(outcome.errors.contains(&SupervisorError::ReapTimeout));
        assert_eq!(
            ticket.outcome().await.unwrap().status,
            Termination::Exited { code: 0 }
        );
    });
}

#[test]
fn deadline_before_observation_after_cancellation_is_a_late_trigger() {
    run_test(async {
        let gate = CancelToken::new();
        let root = spawn_root(
            spec(&["hold"]),
            admit().unwrap(),
            &SpawnOptions {
                exit_observation_gate: Some(gate.clone()),
                ..SpawnOptions::default()
            },
        )
        .unwrap();
        let pid = root.pid();
        let controls = controls(Duration::from_millis(100));
        controls.cancel.cancel();
        let (outcome, ()) = tokio::join!(wait_root_controlled(root, controls), async {
            until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
            gate.cancel();
        });
        killed(&outcome, StopTrigger::Cancelled);
        assert!(
            outcome
                .notes
                .contains(&Note::LateTrigger(StopTrigger::WallDeadline)),
            "{outcome:?}"
        );
    });
}

#[test]
fn cancellation_after_a_deadline_kill_and_exit_observation_is_not_a_late_trigger() {
    run_test(async {
        let gate = CancelToken::new();
        let root = spawn_root(
            spec(&["hold"]),
            admit().unwrap(),
            &SpawnOptions {
                exit_observation_gate: Some(gate.clone()),
                reap_never_ready: true,
                ..SpawnOptions::default()
            },
        )
        .unwrap();
        let pid = root.pid();
        let ticket = root.ticket();
        let controls = controls(Duration::ZERO);
        let cancel = controls.cancel.clone();
        let mut wait = Box::pin(wait_root_controlled(root, controls));
        tokio::select! {
            outcome = &mut wait => panic!("injected reap unexpectedly completed: {outcome:?}"),
            _ = until(|| process(pid).is_some_and(|p| p.state == 'Z')) => {}
        }
        gate.cancel();
        std::future::poll_fn(|cx| {
            assert!(wait.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        cancel.cancel();
        let outcome = wait.await;
        assert_eq!(outcome.stop_trigger, Some(StopTrigger::WallDeadline));
        assert!(outcome.timing.exit_observed.is_some());
        assert!(
            !outcome
                .notes
                .contains(&Note::LateTrigger(StopTrigger::Cancelled)),
            "{outcome:?}"
        );
        assert!(outcome.errors.contains(&SupervisorError::ReapTimeout));
        assert!(matches!(
            ticket.outcome().await.unwrap().status,
            Termination::Signalled { signal: 9, .. }
        ));
    });
}

// Capture tests share the lifecycle tests' process-wide serialization and
// hard watchdog. All test artifacts live in this worktree.
use cpforge::supervisor::{
    CaptureLoss, EventSender, Exceed, Sink, SinkGate, SinkTestOptions, Stream, StreamPolicy,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CapturePath(std::path::PathBuf);
impl CapturePath {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(
            std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!(
                    "b3-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )),
        )
    }
    fn step(&self) -> String {
        format!("wait-file:{}", self.0.display())
    }
    fn open(&self) {
        fs::write(&self.0, []).unwrap();
    }
}
impl Drop for CapturePath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn capture(sink: Sink, budget: u64, on_exceed: Exceed, events: bool) -> OutputSpec {
    OutputSpec::Capture(StreamPolicy {
        sink,
        budget,
        on_exceed,
        events,
    })
}

fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

#[test]
fn capture_budget_boundaries_clip_both_streams_for_every_policy() {
    run_test(async {
        for stream in [Stream::Stdout, Stream::Stderr] {
            for file in [false, true] {
                for exceed in [Exceed::Kill, Exceed::Discard] {
                    for size in [4095, 4096, 4097] {
                        let path = CapturePath::new();
                        let name = if stream == Stream::Stdout {
                            "stdout"
                        } else {
                            "stderr"
                        };
                        // Hold only on a killing breach, making enforcement
                        // deterministic while all other cases reach EOF.
                        let steps = [
                            format!("write:{name}:{size}"),
                            if exceed == Exceed::Kill && size == 4097 {
                                "hold".into()
                            } else {
                                "exit:0".into()
                            },
                        ];
                        let mut program = spec(&[&steps[0], &steps[1]]);
                        let output = capture(
                            if file {
                                Sink::File(path.0.clone())
                            } else {
                                Sink::Discard
                            },
                            4096,
                            exceed,
                            false,
                        );
                        match stream {
                            Stream::Stdout => program.stdout = output,
                            Stream::Stderr => program.stderr = output,
                        }
                        let root = spawn_root(program, admit().unwrap(), &SpawnOptions::default())
                            .unwrap();
                        let outcome = wait_root(root).await;
                        let record = match stream {
                            Stream::Stdout => outcome.stdout.as_ref(),
                            Stream::Stderr => outcome.stderr.as_ref(),
                        }
                        .unwrap();
                        assert_eq!(record.observed, size);
                        assert_eq!(record.stored, if file { size.min(4096) } else { 0 });
                        assert_eq!(record.exceeded_at, (size == 4097).then_some(4097));
                        assert_eq!(record.complete(), size <= 4096);
                        assert!(!record.has_infrastructure_loss());
                        assert_eq!(
                            record.losses.contains(&CaptureLoss::BudgetDiscard),
                            size == 4097 && exceed == Exceed::Discard
                        );
                        if file {
                            assert_eq!(fs::metadata(&path.0).unwrap().len(), record.stored);
                        }
                        if size == 4097 {
                            assert!(outcome.violations.contains(&Violation::OutputBudget {
                                stream,
                                offset: 4097
                            }));
                            if exceed == Exceed::Kill {
                                killed(&outcome, StopTrigger::OutputBudget(stream));
                            }
                        } else {
                            success(&outcome, 0);
                        }
                    }
                }
            }
        }
    });
}

#[test]
fn capture_flood_is_exact_and_unread_events_stay_bounded() {
    run_test(async {
        let out = CapturePath::new();
        let err = CapturePath::new();
        let count = 64 * 1024 * 1024;
        let mut program = spec(&["flood:67108864"]);
        program.stdout = capture(Sink::File(out.0.clone()), count, Exceed::Kill, true);
        program.stderr = capture(Sink::File(err.0.clone()), count, Exceed::Kill, true);
        let (sender, receiver) = EventSender::with_budget(32768);
        let mut controls = controls(Duration::from_secs(8));
        controls.events = Some(sender.clone());
        let outcome = wait_root_controlled(
            spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap(),
            controls,
        )
        .await;
        success(&outcome, 0);
        let expected = fnv(std::iter::repeat_n(b'x', count as usize));
        for (record, path) in [
            (outcome.stdout.unwrap(), out),
            (outcome.stderr.unwrap(), err),
        ] {
            assert!(record.complete(), "{record:?}");
            assert_eq!(record.observed, count);
            assert_eq!(record.stored, count);
            let bytes = fs::read(&path.0).unwrap();
            assert_eq!(bytes.len() as u64, count);
            assert_eq!(fnv(bytes), expected);
        }
        assert!(sender.dropped() > 0);
        assert!(sender.queued_bytes() <= 32768);
        drop(receiver);
        assert_eq!(sender.queued_bytes(), 0);
        let drops = sender.dropped();
        sender.offer(Stream::Stdout, b"disconnected");
        assert_eq!(sender.dropped(), drops);
    });
}

#[test]
fn capture_preserves_invalid_utf8_and_events_in_stream_order() {
    run_test(async {
        let path = CapturePath::new();
        let mut program = spec(&[
            "write:stdout:70000:a",
            "write-hex:stdout:70000:ff",
            "write:stdout:70000:z",
        ]);
        program.stdout = capture(Sink::File(path.0.clone()), 210000, Exceed::Kill, true);
        let (sender, mut receiver) = EventSender::channel();
        let mut controls = controls(Duration::from_secs(5));
        controls.events = Some(sender.clone());
        let root = spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap();
        // Do not start waiting immediately: attaching controls must precede
        // initial event admission even if the child has already written.
        until(|| process(root.pid()).is_some()).await;
        let (outcome, events) = tokio::join!(wait_root_controlled(root, controls), async {
            let mut bytes = Vec::new();
            while bytes.len() < 210000 {
                let chunk = receiver.recv().await.unwrap();
                assert_eq!(chunk.stream, Stream::Stdout);
                assert!(!chunk.bytes.is_empty() && chunk.bytes.len() <= 4096);
                bytes.extend(chunk.bytes);
            }
            bytes
        });
        success(&outcome, 0);
        assert!(outcome.stdout.unwrap().complete());
        let expected: Vec<_> = std::iter::repeat_n(b'a', 70000)
            .chain(std::iter::repeat_n(255, 70000))
            .chain(std::iter::repeat_n(b'z', 70000))
            .collect();
        assert_eq!(fs::read(&path.0).unwrap(), expected);
        assert_eq!(events, expected);
        assert_eq!(sender.dropped(), 0);
    });
}

#[cfg(feature = "storage-test-hooks")]
struct CreationRelease(Arc<cpforge::supervisor::CreationGate>);
#[cfg(feature = "storage-test-hooks")]
impl CreationRelease {
    fn new(after_commit: bool) -> Self {
        use cpforge::supervisor::CreationGate;
        Self(Arc::new(if after_commit {
            CreationGate::after_commit()
        } else {
            CreationGate::before_commit()
        }))
    }
}
#[cfg(feature = "storage-test-hooks")]
impl Drop for CreationRelease {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(feature = "storage-test-hooks")]
async fn at_creation_gate<F: Future>(future: &mut std::pin::Pin<Box<F>>, gate: &CreationRelease) {
    tokio::time::timeout(allowance(1000), async {
        tokio::select! { biased;
            _ = gate.0.entered() => {},
            _ = future.as_mut() => panic!("creation finished before reaching its gate"),
        }
    })
    .await
    .expect("creation did not reach its gate");
}

#[cfg(feature = "storage-test-hooks")]
async fn assert_creation_pending<F: Future>(future: &mut std::pin::Pin<Box<F>>) {
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(future.as_mut().poll(cx).is_pending())
        })
        .await,
        "committed creation returned while its gate was closed"
    );
}

#[cfg(feature = "storage-test-hooks")]
async fn creation_baseline() -> (cpforge::supervisor::Counters, BTreeSet<u32>, usize, usize) {
    success(&wait_root(spawn(&["exit:0"], false)).await, 0);
    quiescent().await;
    (debug_counters(), children(), fd_count(), thread_count())
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancel_before_commit_returns_while_creation_is_blocked() {
    run_test(async {
        use cpforge::supervisor::{Creator, create_root};
        let (baseline, before, fds, threads) = creation_baseline().await;
        let creator = Creator::start().unwrap();
        let gate = CreationRelease::new(false);
        let options = SpawnOptions {
            creation_gate: Some(gate.0.clone()),
            ..SpawnOptions::default()
        };
        let cancel = CancelToken::new();
        let admission = admit().unwrap();
        let ticket = admission.ticket();
        let mut future = Box::pin(create_root(
            &creator,
            spec(&["hold"]),
            admission,
            &options,
            &cancel,
        ));
        at_creation_gate(&mut future, &gate).await;
        assert_eq!(children(), before);
        cancel.cancel();
        let result = tokio::time::timeout(allowance(1000), future.as_mut())
            .await
            .expect("pre-commit cancellation waited for creation");
        assert!(matches!(result, Err(NotStarted::CancelledBeforeStart)));
        drop(future);
        assert_eq!(children(), before);
        gate.0.release();
        assert!(
            tokio::time::timeout(allowance(1000), ticket.outcome())
                .await
                .unwrap()
                .is_none()
        );
        drop(creator);
        quiescent().await;
        assert_eq!(children(), before);
        settled(fds, threads).await;
        assert_eq!(debug_counters(), baseline);
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancel_after_commit_finishes_creation_then_kills() {
    run_test(async {
        use cpforge::supervisor::{Creator, create_root};
        let (baseline, before, fds, threads) = creation_baseline().await;
        let creator = Creator::start().unwrap();
        let gate = CreationRelease::new(true);
        let options = SpawnOptions {
            creation_gate: Some(gate.0.clone()),
            ..SpawnOptions::default()
        };
        let cancel = CancelToken::new();
        let mut future = Box::pin(create_root(
            &creator,
            spec(&["hold"]),
            admit().unwrap(),
            &options,
            &cancel,
        ));
        at_creation_gate(&mut future, &gate).await;
        cancel.cancel();
        assert_creation_pending(&mut future).await;
        assert_eq!(children(), before);
        gate.0.release();
        let root = tokio::time::timeout(allowance(1000), future.as_mut())
            .await
            .unwrap()
            .unwrap();
        let pid = root.pid();
        let ticket = root.ticket();
        drop(future);
        let outcome = tokio::time::timeout(
            allowance(1000),
            wait_root_controlled(
                root,
                RunControls {
                    cancel,
                    ..controls(Duration::from_secs(5))
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(outcome.stop_trigger, Some(StopTrigger::Cancelled));
        assert!(matches!(
            outcome.status,
            Termination::Signalled { signal: 9, .. }
        ));
        assert!(process(pid).is_none());
        assert!(ticket.outcome().await.is_none());
        drop(creator);
        quiescent().await;
        assert_eq!(children(), before);
        settled(fds, threads).await;
        assert_eq!(debug_counters(), baseline);
    });
}

#[cfg(feature = "storage-test-hooks")]
async fn drop_creation(after_commit: bool) {
    use cpforge::supervisor::{Creator, create_root};
    let (baseline, before, fds, threads) = creation_baseline().await;
    let creator = Creator::start().unwrap();
    let gate = CreationRelease::new(after_commit);
    let options = SpawnOptions {
        creation_gate: Some(gate.0.clone()),
        ..SpawnOptions::default()
    };
    let cancel = CancelToken::new();
    let admission = admit().unwrap();
    let ticket = admission.ticket();
    let mut future = Box::pin(create_root(
        &creator,
        spec(&["hold"]),
        admission,
        &options,
        &cancel,
    ));
    at_creation_gate(&mut future, &gate).await;
    assert_eq!(children(), before);
    drop(future);
    gate.0.release();
    let outcome = tokio::time::timeout(allowance(1000), ticket.outcome())
        .await
        .unwrap();
    if after_commit {
        let outcome = outcome.expect("committed child must transfer its record");
        assert_eq!(outcome.stop_trigger, Some(StopTrigger::Cancelled));
        assert!(matches!(
            outcome.status,
            Termination::Signalled { signal: 9, .. }
        ));
    } else {
        assert!(outcome.is_none(), "uncommitted creation started a child");
    }
    drop(creator);
    quiescent().await;
    assert_eq!(
        children(),
        before,
        "a child or zombie survived cancellation"
    );
    settled(fds, threads).await;
    assert_eq!(debug_counters(), baseline);
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn drop_before_commit_never_creates_a_child() {
    run_test(drop_creation(false));
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn drop_after_commit_kills_and_transfers_the_created_child() {
    run_test(drop_creation(true));
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn force_exit_registry_primitive_does_not_wait_for_blocked_creation() {
    run_test(async {
        use cpforge::supervisor::{Creator, create_root, force_kill_active_for_test};
        // Warm Tokio's test control worker before counting threads.
        tokio::task::spawn_blocking(|| ()).await.unwrap();
        for after_commit in [false, true] {
            let (baseline, before, fds, threads) = creation_baseline().await;
            let creator = Creator::start().unwrap();
            let cancel = CancelToken::new();
            let options = SpawnOptions::default();
            let active = create_root(
                &creator,
                spec(&["hold"]),
                admit().unwrap(),
                &options,
                &cancel,
            )
            .await
            .unwrap();
            let active_pid = active.pid();
            let gate = CreationRelease::new(after_commit);
            let options = SpawnOptions {
                creation_gate: Some(gate.0.clone()),
                ..SpawnOptions::default()
            };
            let mut future = Box::pin(create_root(
                &creator,
                spec(&["exit:0"]),
                admit().unwrap(),
                &options,
                &cancel,
            ));
            at_creation_gate(&mut future, &gate).await;
            let expected: BTreeSet<_> = before.iter().copied().chain([active_pid]).collect();
            assert_eq!(children(), expected, "blocked creation started a child");
            tokio::time::timeout(
                allowance(1000),
                tokio::task::spawn_blocking(force_kill_active_for_test),
            )
            .await
            .expect("force exit waited for creation")
            .unwrap();
            let outcome = tokio::time::timeout(allowance(1000), wait_root(active))
                .await
                .unwrap();
            assert!(matches!(
                outcome.status,
                Termination::Signalled { signal: 9, .. }
            ));
            assert!(process(active_pid).is_none());
            assert_eq!(children(), before);
            // This primitive cannot set process-wide EXITING; late creation
            // completes normally, exercising the narrowed Unix guarantee.
            gate.0.release();
            let root = tokio::time::timeout(allowance(1000), future.as_mut())
                .await
                .unwrap()
                .unwrap();
            drop(future);
            success(&wait_root(root).await, 0);
            drop(creator);
            quiescent().await;
            assert_eq!(children(), before);
            settled(fds, threads).await;
            assert_eq!(debug_counters(), baseline);
        }
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn session_cancel_and_drop_between_root_commits_release_both_roots() {
    run_test(async {
        use cpforge::supervisor::{
            Creator,
            session::{self, Fact, RunRecord, SessionSpec, StartFailureKind},
        };
        for after_commit in [false, true] {
            for drop_future in [false, true] {
                let (baseline, before, fds, threads) = creation_baseline().await;
                let creator = Arc::new(Creator::start().unwrap());
                let gate = CreationRelease::new(after_commit);
                let mut session =
                    SessionSpec::new(spec(&["hold"]), spec(&["hold"]), Duration::from_secs(5));
                session.creator = Some(creator.clone());
                session.spawn_options[1].creation_gate = Some(gate.0.clone());
                let cancel = session.cancel.clone();
                let mut future = Box::pin(session::run_session(session));
                at_creation_gate(&mut future, &gate).await;
                let peers: Vec<_> = children().difference(&before).copied().collect();
                assert_eq!(peers.len(), 1, "only S exists while I is gated");
                if drop_future {
                    drop(future);
                    gate.0.release();
                } else {
                    cancel.cancel();
                    // S has committed, so its record must be retained while
                    // the creation worker finishes the pair's request.
                    assert_creation_pending(&mut future).await;
                    gate.0.release();
                    let outcome = tokio::time::timeout(allowance(1000), future.as_mut())
                        .await
                        .unwrap();
                    drop(future);
                    assert!(
                        outcome
                            .record
                            .events()
                            .iter()
                            .any(|e| matches!(e.fact, Fact::CancelRequested))
                    );
                    assert!(
                        !outcome
                            .record
                            .events()
                            .iter()
                            .any(|e| matches!(e.fact, Fact::Infrastructure { .. }))
                    );
                    for (index, run) in [outcome.solution, outcome.interactor]
                        .into_iter()
                        .enumerate()
                    {
                        match run {
                            RunRecord::NotStarted(reason) => {
                                assert_eq!(index, 1);
                                assert!(!after_commit);
                                assert_eq!(reason.kind, StartFailureKind::CancelledBeforeStart);
                            }
                            RunRecord::Started(outcome) => {
                                assert!(index == 0 || after_commit);
                                assert_eq!(outcome.stop_trigger, Some(StopTrigger::Cancelled));
                                assert!(matches!(
                                    outcome.status,
                                    Termination::Signalled { signal: 9, .. }
                                ));
                            }
                        }
                    }
                }
                drop(creator);
                quiescent().await;
                for pid in peers {
                    assert!(process(pid).is_none());
                }
                assert_eq!(children(), before, "session leaked a child or zombie");
                settled(fds, threads).await;
                assert_eq!(debug_counters(), baseline);
            }
        }
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn creation_credit_failure_refuses_the_job_before_admission() {
    run_test(async {
        use cpforge::jobs::engine::SupervisorExecutor;
        let (baseline, before, fds, threads) = creation_baseline().await;
        let held = global_bounds()
            .unwrap()
            .thread_credits
            .try_reserve(31)
            .unwrap();
        let occupied = debug_counters();
        assert!(matches!(
            SupervisorExecutor::start(),
            Err(NotStarted::Worker)
        ));
        assert_eq!(debug_counters(), occupied);
        assert_eq!(children(), before);
        drop(held);
        quiescent().await;
        settled(fds, threads).await;
        assert_eq!(debug_counters(), baseline);
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn pipe_creation_failures_release_partial_setup_without_starting_children() {
    run_test(async {
        use cpforge::supervisor::{Creator, create_root};

        // Warm the runtime and service before checking the quiescent baseline.
        success(&wait_root(spawn(&["exit:0"], false)).await, 0);
        quiescent().await;
        let baseline = debug_counters();
        let before = children();
        let fds = fd_count();
        let threads = thread_count();
        for forward_stdin in [false, true] {
            // Forward stdin is prepared first, followed by stdout and stderr.
            // Without stdin, failing stderr exercises cleanup of a started sink.
            for k in 1..=if forward_stdin { 3 } else { 2 } {
                let out = CapturePath::new();
                let err = CapturePath::new();
                let mut program = spec(&["hold"]);
                if forward_stdin {
                    program.stdin = StdinSpec::Forward;
                }
                program.stdout = capture(Sink::File(out.0.clone()), 4096, Exceed::Kill, false);
                program.stderr = capture(Sink::File(err.0.clone()), 4096, Exceed::Kill, false);
                let admission = admit().unwrap();
                let ticket = admission.ticket();
                let creator = Creator::start().unwrap();
                let options = SpawnOptions {
                    fail_pipe_at: Some(k),
                    ..SpawnOptions::default()
                };
                let error = tokio::time::timeout(
                    allowance(1000),
                    create_root(&creator, program, admission, &options, &CancelToken::new()),
                )
                .await
                .unwrap()
                .unwrap_err();
                drop(creator);
                assert!(
                    matches!(error, NotStarted::Setup(ref e) if e.raw_os_error() == Some(libc::EMFILE)),
                    "stdin={forward_stdin}, pipe={k}: {error:?}"
                );
                assert!(ticket.outcome().await.is_none());
                quiescent().await; // One-second Linux / four-second macOS bound.
                assert_eq!(children(), before);
                settled(fds, threads).await;
                assert_eq!(debug_counters(), baseline);
            }
        }
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn session_pipe_creation_failures_release_cross_wiring_and_both_admissions() {
    run_test(async {
        use cpforge::supervisor::Creator;
        use cpforge::supervisor::session::{
            RunRecord, SessionMode, SessionSpec, StartFailureKind, run_session,
        };

        success(&wait_root(spawn(&["exit:0"], false)).await, 0);
        quiescent().await;
        let baseline = debug_counters();
        let before = children();
        let fds = fd_count();
        let threads = thread_count();
        for (mode, pipes) in [(SessionMode::Direct, 4), (SessionMode::Relay, 6)] {
            // Direct: two cross-wiring pipes, then the two stderr captures.
            // Relay: two output + two input pipes, then the stderr captures.
            for k in 1..=pipes {
                let out = CapturePath::new();
                let err = CapturePath::new();
                let creator = Arc::new(Creator::start().unwrap());
                let mut session =
                    SessionSpec::new(spec(&["hold"]), spec(&["hold"]), Duration::from_secs(5));
                session.creator = Some(creator.clone());
                session.fail_pipe_at = Some(k);
                session.mode = mode;
                session.solution.stderr =
                    capture(Sink::File(out.0.clone()), 4096, Exceed::Kill, false);
                session.interactor.stderr =
                    capture(Sink::File(err.0.clone()), 4096, Exceed::Kill, false);
                let outcome = tokio::time::timeout(allowance(1000), run_session(session))
                    .await
                    .expect("partial session setup exceeded its bound");
                drop(creator);
                let mut emfile = false;
                for run in [outcome.solution, outcome.interactor] {
                    let RunRecord::NotStarted(reason) = run else {
                        panic!("pipe failure started a session child");
                    };
                    assert_eq!(reason.kind, StartFailureKind::Setup);
                    emfile |= reason.code == libc::EMFILE;
                }
                assert!(
                    emfile,
                    "{mode:?} pipe {k} did not reach the injected failure"
                );
                quiescent().await;
                assert_eq!(children(), before);
                settled(fds, threads).await;
                assert_eq!(debug_counters(), baseline);
            }
        }
    });
}

// A real filesystem failure complements the deterministic failing sink stub
// below (which also runs on macOS). No mount privilege is required when
// CI supplies CPF_TEST_TINY_FS or a small /dev/shm.
#[cfg(target_os = "linux")]
#[path = "support/tiny_filesystem.rs"]
mod tiny_filesystem;

#[cfg(target_os = "linux")]
#[test]
fn real_enospc_reports_write_loss_without_blocking_or_conflating_budget_loss() {
    run_test(async {
        let Some(filesystem) = tiny_filesystem::TinyFs::acquire() else {
            return;
        };
        let baseline = debug_counters();
        let fds = fd_count();
        let threads = thread_count();
        for budget in [1024 * 1024, 512 * 1024] {
            filesystem.fill();
            let path = filesystem.directory.join("stdout");
            let mut program = spec(&["write:stdout:1048576", "exit:0"]);
            program.stdout = capture(Sink::File(path.clone()), budget, Exceed::Discard, false);
            let outcome = tokio::time::timeout(
                allowance(3000),
                wait_root(spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap()),
            )
            .await
            .expect("ENOSPC blocked the run past three seconds");
            success(&outcome, 0);
            assert_eq!(outcome.stop_trigger, None);
            // A breach of the smaller budget is reported alongside the write error.
            let breach = Violation::OutputBudget {
                stream: Stream::Stdout,
                offset: budget + 1,
            };
            let expected = if budget < 1024 * 1024 {
                vec![breach]
            } else {
                Vec::new()
            };
            assert_eq!(outcome.violations, expected, "{outcome:?}");
            let record = outcome.stdout.unwrap();
            assert!(record.eof);
            assert_eq!(record.observed, 1024 * 1024);
            assert_eq!(record.stored, fs::metadata(&path).unwrap().len());
            assert!(record.stored < budget);
            assert!(!record.complete());
            assert!(record.has_infrastructure_loss());
            assert!(
                record
                    .losses
                    .contains(&CaptureLoss::WriteError(libc::ENOSPC)),
                "{record:?}"
            );
            let exceeded = budget < record.observed;
            assert_eq!(record.exceeded_at, exceeded.then_some(budget + 1));
            assert_eq!(
                record.losses.contains(&CaptureLoss::BudgetDiscard),
                exceeded
            );
            quiescent().await;
            assert_eq!(debug_counters(), baseline);
            settled(fds, threads).await;
            fs::remove_file(path).unwrap();
        }
    });
}

#[test]
fn sink_partial_writes_and_eintr_are_retried_and_failures_never_block_the_child() {
    run_test(async {
        for injection in [
            SinkTestOptions {
                max_write: Some(37),
                interrupt_once: true,
                ..SinkTestOptions::default()
            },
            SinkTestOptions {
                fail_after: Some(1234),
                ..SinkTestOptions::default()
            },
            SinkTestOptions {
                zero_once: true,
                ..SinkTestOptions::default()
            },
            SinkTestOptions {
                fail_flush: true,
                ..SinkTestOptions::default()
            },
            SinkTestOptions {
                fail_close: true,
                ..SinkTestOptions::default()
            },
        ] {
            let failing = injection.fail_after.is_some()
                || injection.zero_once
                || injection.fail_flush
                || injection.fail_close;
            let path = CapturePath::new();
            let count = if failing { 1048576 } else { 65536 };
            let mut program = spec(&[&format!("write:stdout:{count}"), "exit:0"]);
            program.stdout = capture(Sink::File(path.0.clone()), 1048576, Exceed::Kill, false);
            let opts = SpawnOptions {
                stdout_sink: injection,
                ..SpawnOptions::default()
            };
            let outcome = wait_root(spawn_root(program, admit().unwrap(), &opts).unwrap()).await;
            success(&outcome, 0);
            let record = outcome.stdout.unwrap();
            assert_eq!(record.observed, count);
            assert!(record.eof);
            assert_eq!(record.complete(), !failing, "{record:?}");
            assert_eq!(record.has_infrastructure_loss(), failing);
            assert_eq!(
                record
                    .losses
                    .iter()
                    .any(|loss| matches!(loss, CaptureLoss::WriteError(_))),
                failing
            );
            assert_eq!(record.stored, fs::metadata(&path.0).unwrap().len());
        }
    });
}

#[test]
fn stalled_sink_freezes_at_five_seconds_and_keeps_its_slot_and_credit() {
    run_test(async {
        let baseline = debug_counters();
        let gate = ReleaseGate(Arc::new(SinkGate::default()));
        let path = CapturePath::new();
        let mut program = spec(&["write:stdout:4096", "exit:0"]);
        program.stdout = capture(Sink::File(path.0.clone()), 4096, Exceed::Kill, false);
        let opts = SpawnOptions {
            stdout_sink: SinkTestOptions {
                gate: Some(gate.clone()),
                ..SinkTestOptions::default()
            },
            ..SpawnOptions::default()
        };
        let root = spawn_root(program, admit().unwrap(), &opts).unwrap();
        let ticket = root.ticket();
        let outcome = wait_root(root).await;
        assert!(gate.entered());
        success(&outcome, 0);
        let record = outcome.stdout.as_ref().unwrap();
        assert!(record.losses.contains(&CaptureLoss::WriteStalled));
        assert_eq!(record.stored, 0);
        let since_exit = outcome.timing.exit_observed.unwrap().elapsed();
        assert!(
            (Duration::from_secs(5)..Duration::from_secs(5) + allowance(500)).contains(&since_exit),
            "{since_exit:?}"
        );
        assert_eq!(debug_counters().slots_in_use, baseline.slots_in_use + 1);
        assert_eq!(debug_counters().credits_in_use, baseline.credits_in_use + 1);
        // The ticket has a logical record even while the worker is still owned.
        let frozen = ticket.outcome().await.unwrap();
        assert_eq!(frozen.stdout, outcome.stdout);
        gate.release();
        quiescent().await;
        assert_eq!(debug_counters(), baseline);
        assert_eq!(record.stored, 0); // immutable after the late write completes
        assert_eq!(fs::metadata(&path.0).unwrap().len(), 4096);
    });
}

#[test]
fn full_capture_queue_does_not_delay_deadline() {
    run_test(async {
        let gate = ReleaseGate(Arc::new(SinkGate::default()));
        let path = CapturePath::new();
        let mut program = spec(&["write:stdout:67108864", "exit:0"]);
        program.stdout = capture(Sink::File(path.0.clone()), 67108864, Exceed::Kill, false);
        let opts = SpawnOptions {
            stdout_sink: SinkTestOptions {
                gate: Some(gate.clone()),
                ..SinkTestOptions::default()
            },
            ..SpawnOptions::default()
        };
        let root = spawn_root(program, admit().unwrap(), &opts).unwrap();
        let ticket = root.ticket();
        let mut waiting = Box::pin(wait_root_controlled(
            root,
            controls(Duration::from_millis(200)),
        ));
        let (outcome, ()) = tokio::join!(&mut waiting, async {
            until(|| gate.entered()).await;
            // The worker is now blocked while the runtime still processes
            // the independent wall timer.
        });
        killed(&outcome, StopTrigger::WallDeadline);
        assert!(outcome.enforced_at.unwrap() < Duration::from_millis(500));
        let record = outcome.stdout.as_ref().unwrap();
        assert!(record.observed <= 1024 * 1024 + 65536);
        assert!(
            record
                .losses
                .contains(&CaptureLoss::NoEofBeforeDrainDeadline)
        );
        assert!(record.losses.contains(&CaptureLoss::WriteStalled));
        assert!(!record.complete());
        gate.release();
        ticket.outcome().await;
    });
}

#[test]
fn capture_worker_admission_and_setup_failures_return_all_reservations() {
    run_test(async {
        let baseline = debug_counters();
        let out = CapturePath::new();
        let err = CapturePath::new();
        let make = || {
            let mut program = spec(&["hold"]);
            program.stdout = capture(Sink::File(out.0.clone()), 4096, Exceed::Kill, false);
            program.stderr = capture(Sink::File(err.0.clone()), 4096, Exceed::Kill, false);
            program
        };
        let held = global_bounds()
            .unwrap()
            .thread_credits
            .try_reserve(30)
            .unwrap();
        assert!(matches!(
            spawn_root(make(), admit().unwrap(), &SpawnOptions::default()),
            Err(NotStarted::Worker)
        ));
        drop(held);
        quiescent().await;
        assert_eq!(debug_counters(), baseline);
        let opts = SpawnOptions {
            stderr_sink: SinkTestOptions {
                fail_start: true,
                ..SinkTestOptions::default()
            },
            ..SpawnOptions::default()
        };
        assert!(matches!(
            spawn_root(make(), admit().unwrap(), &opts),
            Err(NotStarted::Worker)
        ));
        quiescent().await;
        assert_eq!(debug_counters(), baseline);
        let mut program = make();
        program.program = out.0.join("missing-executable");
        assert!(matches!(
            spawn_root(program, admit().unwrap(), &SpawnOptions::default()),
            Err(NotStarted::CreateFailed { .. })
        ));
        quiescent().await;
        assert_eq!(debug_counters(), baseline);
    });
}

#[test]
fn failed_capture_setup_transfers_a_sink_stalled_on_close() {
    run_test(async {
        let baseline = debug_counters();
        let out = CapturePath::new();
        let err = CapturePath::new();
        for fail_setup in [true, false] {
            let mut program = spec(&["hold"]);
            program.stdout = capture(Sink::File(out.0.clone()), 4096, Exceed::Kill, false);
            program.stderr = capture(Sink::File(err.0.clone()), 4096, Exceed::Kill, false);
            if !fail_setup {
                program.program = out.0.join("missing-executable");
            }
            let gate = ReleaseGate(Arc::new(SinkGate::default()));
            let opts = SpawnOptions {
                stdout_sink: SinkTestOptions {
                    close_gate: Some(gate.0.clone()),
                    ..SinkTestOptions::default()
                },
                stderr_sink: SinkTestOptions {
                    fail_start: fail_setup,
                    ..SinkTestOptions::default()
                },
                ..SpawnOptions::default()
            };
            let admission = admit().unwrap();
            let ticket = admission.ticket();
            // A separate watchdog releases the gate even if spawn blocks the
            // current-thread runtime, making the original bug fail promptly.
            let watchdog_gate = gate.0.clone();
            let watchdog = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(1);
                while !watchdog_gate.entered() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                std::thread::sleep(Duration::from_millis(300));
                watchdog_gate.release();
            });
            let start = Instant::now();
            let result = spawn_root(program, admission, &opts);
            let elapsed = start.elapsed();
            assert!(elapsed < Duration::from_millis(150), "{elapsed:?}");
            if fail_setup {
                assert!(matches!(result, Err(NotStarted::Worker)));
            } else {
                assert!(matches!(result, Err(NotStarted::CreateFailed { .. })));
            }
            until(|| {
                gate.entered() && debug_counters().credits_in_use == baseline.credits_in_use + 1
            })
            .await;
            assert_eq!(debug_counters().slots_in_use, baseline.slots_in_use + 1);
            assert_eq!(debug_counters().credits_in_use, baseline.credits_in_use + 1);
            gate.release();
            assert!(ticket.outcome().await.is_none());
            quiescent().await;
            watchdog.join().unwrap();
            assert_eq!(debug_counters(), baseline);
        }
    });
}

struct ReleaseGate(Arc<SinkGate>);
impl std::ops::Deref for ReleaseGate {
    type Target = Arc<SinkGate>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.release();
    }
}

struct CancelOnDrop(CancelToken);
impl std::ops::Deref for CancelOnDrop {
    type Target = CancelToken;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct CaptureEscape(Process);
impl Drop for CaptureEscape {
    fn drop(&mut self) {
        if process(self.0.pid)
            .is_some_and(|p| p.start == self.0.start && !matches!(p.state, 'Z' | 'X'))
        {
            // SAFETY: this test owns the still-live escaped fixture identity.
            assert_eq!(unsafe { libc::kill(self.0.pid as i32, libc::SIGKILL) }, 0);
        }
    }
}

async fn escaped_child(root: u32) -> CaptureEscape {
    until(|| {
        processes()
            .iter()
            .any(|p| p.ppid == root && p.pgrp == p.pid)
    })
    .await;
    CaptureEscape(
        processes()
            .into_iter()
            .find(|p| p.ppid == root && p.pgrp == p.pid)
            .unwrap(),
    )
}

#[test]
fn capture_late_budget_breach_after_root_exit_is_still_a_violation() {
    run_test(async {
        for exceed in [Exceed::Kill, Exceed::Discard] {
            let root_gate = CapturePath::new();
            let child_gate = CapturePath::new();
            let child_step = format!("spawn:1:escape,{},write:stdout:4097", child_gate.step());
            let mut program = spec(&[&child_step, &root_gate.step(), "exit:0"]);
            program.stdout = capture(Sink::Discard, 4096, exceed, false);
            let opts = SpawnOptions {
                // Force Kill's budget branch to perform the nonblocking exit
                // check even though the ordinary observer is still gated.
                exit_observation_gate: (exceed == Exceed::Kill).then(CancelToken::new),
                ..SpawnOptions::default()
            };
            let root = spawn_root(program, admit().unwrap(), &opts).unwrap();
            let pid = root.pid();
            let (outcome, escaped) = tokio::join!(wait_root(root), async {
                let escaped = escaped_child(pid).await;
                root_gate.open();
                // A zombie pins the exited root identity; the descendant cannot
                // cross its budget until the test explicitly opens its gate.
                until(|| process(pid).is_none_or(|p| p.state == 'Z')).await;
                child_gate.open();
                escaped
            });
            success(&outcome, 0);
            assert_eq!(outcome.stop_trigger, None);
            assert!(outcome.violations.contains(&Violation::OutputBudget {
                stream: Stream::Stdout,
                offset: 4097
            }));
            let record = outcome.stdout.unwrap();
            assert_eq!(record.observed, 4097);
            assert_eq!(record.exceeded_at, Some(4097));
            assert!(!record.complete());
            assert!(record.eof);
            if exceed == Exceed::Kill {
                assert!(
                    outcome
                        .notes
                        .contains(&Note::LateTrigger(StopTrigger::OutputBudget(
                            Stream::Stdout
                        )))
                );
            }
            drop(escaped);
        }
    });
}

#[test]
fn capture_drain_deadline_stops_an_escaped_pipe_holder() {
    run_test(async {
        let gate = CapturePath::new();
        let mut program = spec(&["spawn:1:escape,hold", &gate.step(), "exit:0"]);
        program.stdout = capture(Sink::Discard, 4096, Exceed::Kill, false);
        let root = spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap();
        let pid = root.pid();
        let (outcome, escaped) = tokio::join!(wait_root(root), async {
            let escaped = escaped_child(pid).await;
            gate.open();
            escaped
        });
        success(&outcome, 0);
        let elapsed = outcome.timing.exit_observed.unwrap().elapsed();
        assert!(
            (Duration::from_millis(500)..Duration::from_millis(500) + allowance(400))
                .contains(&elapsed),
            "{elapsed:?}"
        );
        let record = outcome.stdout.unwrap();
        assert_eq!(record.losses, [CaptureLoss::NoEofBeforeDrainDeadline]);
        assert!(!record.eof && !record.complete());
        assert!(process(escaped.0.pid).is_some_and(|p| !matches!(p.state, 'Z' | 'X')));
        drop(escaped);
    });
}

#[test]
fn dropping_capture_wait_freezes_at_transfer_cutoff_without_releasing_a_stalled_worker() {
    run_test(async {
        let baseline = debug_counters();
        let gate = ReleaseGate(Arc::new(SinkGate::default()));
        let path = CapturePath::new();
        let mut program = spec(&["write:stdout:67108864", "hold"]);
        program.stdout = capture(Sink::File(path.0.clone()), 67108864, Exceed::Kill, false);
        let opts = SpawnOptions {
            stdout_sink: SinkTestOptions {
                gate: Some(gate.clone()),
                ..SinkTestOptions::default()
            },
            ..SpawnOptions::default()
        };
        let root = spawn_root(program, admit().unwrap(), &opts).unwrap();
        let ticket = root.ticket();
        let mut waiting = Box::pin(wait_root(root));
        tokio::select! {
            outcome = &mut waiting => panic!("fixture unexpectedly finished: {outcome:?}"),
            _ = until(|| gate.entered()) => {}
        }
        let start = Instant::now();
        drop(waiting);
        let outcome = ticket.outcome().await.unwrap();
        assert!(start.elapsed() < Duration::from_millis(500) + allowance(400));
        killed(&outcome, StopTrigger::Cancelled);
        let record = outcome.stdout.unwrap();
        assert!(record.losses.contains(&CaptureLoss::WriteStalled));
        assert_eq!(record.stored, 0);
        assert_eq!(debug_counters().slots_in_use, baseline.slots_in_use + 1);
        assert_eq!(debug_counters().credits_in_use, baseline.credits_in_use + 1);
        gate.release();
        quiescent().await;
        assert_eq!(debug_counters(), baseline);
    });
}

#[test]
fn transferred_finished_capture_waits_for_root_facts_or_cutoff() {
    run_test(async {
        for finish_root in [true, false] {
            let root_gate = CancelOnDrop(CancelToken::new());
            let mut program = spec(&["write:stdout:4096", "exit:0"]);
            program.stdout = capture(Sink::Discard, 4096, Exceed::Kill, false);
            let root = spawn_root(
                program,
                admit().unwrap(),
                &SpawnOptions {
                    transfer_root_gate: Some(root_gate.clone()),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
            let pid = root.pid();
            until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
            let ticket = root.ticket();
            let again = ticket.clone();
            let start = Instant::now();
            drop(root);
            let mut outcome = Box::pin(ticket.outcome());
            assert!(
                tokio::time::timeout(Duration::from_millis(150), &mut outcome)
                    .await
                    .is_err()
            );
            if finish_root {
                root_gate.cancel();
            }
            let outcome = outcome.await.unwrap();
            let elapsed = start.elapsed();
            assert!(outcome.stdout.as_ref().unwrap().complete(), "{outcome:?}");
            if finish_root {
                success(&outcome, 0);
                assert!(elapsed < Duration::from_millis(450), "{elapsed:?}");
            } else {
                assert!(
                    (Duration::from_millis(500)..Duration::from_millis(500) + allowance(400))
                        .contains(&elapsed),
                    "{elapsed:?}"
                );
                assert_eq!(outcome.status, Termination::Unknown);
                root_gate.cancel();
            }
            quiescent().await;
            assert!(process(pid).is_none());
            assert_eq!(again.outcome().await.unwrap().status, outcome.status);
        }
    });
}

#[test]
fn transferred_run_without_captures_publishes_at_cutoff() {
    run_test(async {
        let baseline = debug_counters();
        let root_gate = CancelOnDrop(CancelToken::new());
        let root = spawn_root(
            spec(&["hold"]),
            admit().unwrap(),
            &SpawnOptions {
                transfer_root_gate: Some(root_gate.clone()),
                ..SpawnOptions::default()
            },
        )
        .unwrap();
        let pid = root.pid();
        let ticket = root.ticket();
        let again = ticket.clone();
        let start = Instant::now();
        drop(root);
        let outcome = tokio::time::timeout(
            Duration::from_millis(500) + allowance(400),
            ticket.outcome(),
        )
        .await
        .expect("logical record exceeded the transfer cutoff")
        .unwrap();
        assert!(start.elapsed() >= Duration::from_millis(500));
        assert_eq!(outcome.status, Termination::Unknown);
        assert_eq!(
            outcome.termination.cleanup,
            Cleanup::Unverified {
                reason: UnverifiedReason::RootNotObserved
            }
        );
        assert_eq!(debug_counters().slots_in_use, baseline.slots_in_use + 1);
        root_gate.cancel();
        quiescent().await;
        assert!(process(pid).is_none());
        assert_eq!(debug_counters(), baseline);
        assert_eq!(again.outcome().await.unwrap().status, Termination::Unknown);
    });
}

#[test]
fn transfer_cutoff_starts_at_transfer_after_an_earlier_drain() {
    run_test(async {
        let baseline = debug_counters();
        let gate = ReleaseGate(Arc::new(SinkGate::default()));
        let path = CapturePath::new();
        let mut program = spec(&["write:stdout:4096", "exit:0"]);
        program.stdout = capture(Sink::File(path.0.clone()), 4096, Exceed::Kill, false);
        let root = spawn_root(
            program,
            admit().unwrap(),
            &SpawnOptions {
                stdout_sink: SinkTestOptions {
                    close_gate: Some(gate.0.clone()),
                    ..SinkTestOptions::default()
                },
                ..SpawnOptions::default()
            },
        )
        .unwrap();
        let pid = root.pid();
        let ticket = root.ticket();
        let mut waiting = Box::pin(wait_root(root));
        tokio::select! {
            outcome = &mut waiting => panic!("gated sink unexpectedly finished: {outcome:?}"),
            _ = until(|| gate.entered() && process(pid).is_none()) => {}
        }
        // The root is reaped and finish_capture has started its original drain.
        tokio::select! {
            outcome = &mut waiting => panic!("gated sink unexpectedly finished: {outcome:?}"),
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        let start = Instant::now();
        drop(waiting);
        let (outcome, ()) = tokio::join!(ticket.outcome(), async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            gate.release();
        });
        let outcome = outcome.unwrap();
        success(&outcome, 0);
        let record = outcome.stdout.unwrap();
        assert!(record.complete(), "{record:?}");
        assert_eq!(record.stored, 4096);
        assert!(start.elapsed() < Duration::from_millis(500));
        quiescent().await;
        assert_eq!(debug_counters(), baseline);
    });
}

#[test]
fn captured_runs_release_descriptors_threads_and_credits() {
    run_test(async {
        let path = CapturePath::new();
        let run = || async {
            let mut program = spec(&["flood:65536"]);
            program.stdout = capture(Sink::File(path.0.clone()), 65536, Exceed::Kill, false);
            program.stderr = capture(
                Sink::File(path.0.with_extension("err")),
                65536,
                Exceed::Kill,
                false,
            );
            let outcome =
                wait_root(spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap())
                    .await;
            assert!(outcome.stdout.unwrap().complete());
            assert!(outcome.stderr.unwrap().complete());
        };
        run().await;
        quiescent().await;
        let fds = fd_count();
        let threads = thread_count();
        let counters = debug_counters();
        for _ in 0..64 {
            run().await;
        }
        quiescent().await;
        settled(fds, threads).await;
        assert_eq!(debug_counters(), counters);
        fs::remove_file(path.0.with_extension("err")).unwrap();
    });
}

#[test]
fn budget_discard_does_not_mask_write_loss_and_events_stop_exactly_at_cap() {
    run_test(async {
        let path = CapturePath::new();
        let mut program = spec(&["write:stdout:1048576", "exit:0"]);
        program.stdout = capture(Sink::File(path.0.clone()), 4096, Exceed::Discard, true);
        let opts = SpawnOptions {
            stdout_sink: SinkTestOptions {
                fail_after: Some(100),
                ..SinkTestOptions::default()
            },
            ..SpawnOptions::default()
        };
        let (sender, mut receiver) = EventSender::channel();
        let mut controls = controls(Duration::from_secs(5));
        controls.events = Some(sender.clone());
        let outcome = wait_root_controlled(
            spawn_root(program, admit().unwrap(), &opts).unwrap(),
            controls,
        )
        .await;
        success(&outcome, 0);
        let record = outcome.stdout.unwrap();
        assert_eq!(record.stored, 100);
        assert_eq!(record.exceeded_at, Some(4097));
        assert!(record.losses.contains(&CaptureLoss::BudgetDiscard));
        assert!(
            record
                .losses
                .iter()
                .any(|loss| matches!(loss, CaptureLoss::WriteError(_)))
        );
        assert!(record.has_infrastructure_loss());
        assert!(!record.complete());
        let mut events = Vec::new();
        while let Ok(chunk) = receiver.try_recv() {
            assert!(chunk.bytes.len() <= 4096);
            events.extend(chunk.bytes);
        }
        assert_eq!(events, vec![b'x'; 4096]);
        assert_eq!(sender.dropped(), 0);
    });
}

#[test]
fn capture_owner_and_runtime_can_drop_before_any_reader_poll() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    init_process().unwrap();
    let path = CapturePath::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = runtime.block_on(async {
        let mut program = spec(&["write:stdout:4096", "hold"]);
        program.stdout = capture(Sink::File(path.0.clone()), 4096, Exceed::Kill, false);
        spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap()
    });
    let ticket = root.ticket();
    drop(root);
    drop(runtime);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), async {
            let outcome = ticket.outcome().await.unwrap();
            let record = outcome.stdout.unwrap();
            assert!(!record.complete());
            assert!(!record.eof);
            assert!(
                record
                    .losses
                    .contains(&CaptureLoss::NoEofBeforeDrainDeadline)
            );
            quiescent().await;
        })
        .await
        .expect("runtime-drop capture cleanup exceeded two seconds");
    });
}

#[test]
fn measurements_select_deduplicate_and_report_unsupported_metrics() {
    run_test(async {
        let mut program = spec(&["exit:0"]);
        program.measure = vec![
            MetricKind::PeakWorkingSet,
            MetricKind::PeakJobCommit,
            MetricKind::ProcessLifetime,
            MetricKind::PeakWorkingSet,
        ];
        let outcome =
            wait_root(spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap())
                .await;
        assert_eq!(outcome.status, Termination::Exited { code: 0 });
        assert_eq!(outcome.measurements.len(), 3);
        for measurement in &outcome.measurements {
            assert_eq!(measurement.value, None);
            assert_eq!(
                measurement.reason.as_deref(),
                Some("unsupported on this platform")
            );
        }
        let mut program = spec(&["exit:0"]);
        program.measure = vec![MetricKind::CpuTime, MetricKind::CpuTime];
        let outcome =
            wait_root(spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap())
                .await;
        assert_eq!(outcome.measurements.len(), 1);
        assert_eq!(outcome.measurements[0].kind, MetricKind::CpuTime);
        assert!(outcome.measurements[0].value.is_some());

        let mut program = spec(&["exit:0"]);
        program.measure.clear();
        let outcome =
            wait_root(spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap())
                .await;
        assert!(outcome.measurements.is_empty());
    });
}

#[test]
fn unreaped_measurements_remain_absent_and_frozen_after_late_reap() {
    run_test(async {
        let gate = CancelOnDrop(CancelToken::new());
        let root = spawn_root(
            spec(&["hold"]),
            admit().unwrap(),
            &SpawnOptions {
                transfer_root_gate: Some(gate.clone()),
                ..SpawnOptions::default()
            },
        )
        .unwrap();
        let ticket = root.ticket();
        let again = ticket.clone();
        drop(root);
        let outcome = ticket.outcome().await.unwrap();
        for measurement in &outcome.measurements {
            assert_eq!(measurement.value, None);
            assert_eq!(measurement.completeness, Completeness::Partial);
            assert_eq!(measurement.reason.as_deref(), Some("root was not reaped"));
        }
        gate.cancel();
        quiescent().await;
        assert_eq!(
            again.outcome().await.unwrap().measurements,
            outcome.measurements
        );
    });
}

#[cfg(target_os = "linux")]
#[test]
fn unverified_cleanup_marks_reaped_measurements_partial() {
    run_test(async {
        let outcome = wait_root(spawn(&["spin:10"], true)).await;
        success(&outcome, 0);
        assert!(matches!(
            outcome.termination.cleanup,
            Cleanup::Unverified { .. }
        ));
        for measurement in &outcome.measurements {
            assert!(measurement.value.is_some());
            assert_eq!(measurement.completeness, Completeness::Partial);
            assert_eq!(measurement.reason.as_deref(), Some("cleanup not confirmed"));
        }
    });
}

#[cfg(target_os = "linux")]
#[test]
fn address_space_limit_records_ordinary_allocation_failure_and_rounding() {
    run_test(async {
        let mut program = spec(&["touch-mem:512"]);
        program.limits = vec![LocalLimit::AddressSpace {
            bytes: 128 * 1024 * 1024,
        }];
        let outcome =
            wait_root(spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap())
                .await;
        assert!(
            matches!(outcome.status, Termination::Exited { code } if code != 0)
                || matches!(outcome.status, Termination::Signalled { .. }),
            "{outcome:?}"
        );
        eprintln!("128 MiB RLIMIT_AS, touch-mem:512: {:?}", outcome.status);
        assert!(outcome.errors.is_empty(), "{outcome:?}");
        assert!(outcome.violations.is_empty());
        assert_eq!(outcome.stop_trigger, None);
        assert_eq!(outcome.limits.len(), 1);
        let limit = &outcome.limits[0];
        assert_eq!(
            limit.configured,
            LocalLimit::AddressSpace {
                bytes: 128 * 1024 * 1024
            }
        );
        assert_eq!(limit.normalised, 128 * 1024 * 1024);
        assert_eq!(limit.installed, Some(limit.normalised));
        assert!(limit.inherited.is_some());
        assert_eq!(limit.mechanism, "setrlimit RLIMIT_AS");
        assert_eq!(limit.outer, cpforge::supervisor::OuterRestriction::None);

        let mut program = spec(&["exit:0"]);
        let bytes = 128 * 1024 * 1024 + 1;
        program.limits = vec![LocalLimit::AddressSpace { bytes }];
        let outcome =
            wait_root(spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap())
                .await;
        success(&outcome, 0);
        // SAFETY: sysconf has no pointer arguments and does not change limits.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        assert_eq!(outcome.limits[0].normalised, bytes.div_ceil(page) * page);
        assert_eq!(
            outcome.limits[0].installed,
            Some(outcome.limits[0].normalised)
        );
    });
}

#[cfg(target_os = "linux")]
#[test]
fn invalid_limits_fail_before_setup_without_creating_children() {
    run_test(async {
        let baseline = debug_counters();
        let before = children();
        let fds = fd_count();
        for (limits, expected) in [
            (
                vec![LocalLimit::AddressSpace { bytes: 0 }],
                Some(LimitValidation::Zero),
            ),
            (
                vec![LocalLimit::AddressSpace { bytes: 4096 }; 2],
                Some(LimitValidation::Duplicate),
            ),
            (
                vec![LocalLimit::AddressSpace { bytes: u64::MAX }],
                Some(LimitValidation::OutOfRange),
            ),
            (
                vec![LocalLimit::AddressSpace {
                    bytes: u64::MAX - 1,
                }],
                Some(LimitValidation::OutOfRange),
            ),
            (vec![LocalLimit::JobCommit { bytes: 4096 }], None),
        ] {
            let mut program = spec(&["hold"]);
            program.limits = limits;
            // Invalid stdin proves validation precedes even file setup.
            program.stdin =
                StdinSpec::File(std::env::current_dir().unwrap().join("missing-limit-input"));
            let error =
                spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap_err();
            match expected {
                Some(expected) => assert!(
                    matches!(error, NotStarted::InvalidLimit(actual) if actual == expected),
                    "{error:?}"
                ),
                None => assert!(matches!(error, NotStarted::UnsupportedLimit), "{error:?}"),
            }
            assert_eq!(children(), before);
            assert_eq!(debug_counters(), baseline);
            assert_eq!(fd_count(), fds);
        }
    });
}

#[cfg(target_os = "linux")]
#[test]
fn phase_pipe_distinguishes_limit_installation_from_exec_failure() {
    run_test(async {
        let before = children();
        let counters = debug_counters();
        let fds = fd_count();
        for inject in [true, false] {
            let mut program = spec(&["hold"]);
            program.limits = vec![LocalLimit::AddressSpace {
                bytes: 128 * 1024 * 1024,
            }];
            if !inject {
                program.program = std::env::current_dir()
                    .unwrap()
                    .join("missing-limit-program");
            }
            let error = spawn_root(
                program,
                admit().unwrap(),
                &SpawnOptions {
                    fail_limit_install: inject,
                    ..SpawnOptions::default()
                },
            )
            .unwrap_err();
            if inject {
                assert!(
                    matches!(error, NotStarted::Limit(libc::EINVAL)),
                    "{error:?}"
                );
            } else {
                assert!(
                    matches!(
                        error,
                        NotStarted::CreateFailed {
                            api: "spawn",
                            raw_error: libc::ENOENT
                        }
                    ),
                    "{error:?}"
                );
            }
            quiescent().await;
            assert_eq!(children(), before);
            assert_eq!(debug_counters(), counters);
            assert_eq!(fd_count(), fds);
        }
    });
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_address_space_hard_limit_is_respected() {
    const MARKER: &str = "CPF_TEST_INHERITED_AS_LIMIT";
    if std::env::var_os(MARKER).is_some() {
        run_test(async {
            // Request above hard, then below hard but above inherited soft.
            for (requested, installed, lowered) in [
                (512 * 1024 * 1024, 256 * 1024 * 1024, true),
                (224 * 1024 * 1024, 224 * 1024 * 1024, false),
            ] {
                let mut program = spec(&["exit:0"]);
                program.limits = vec![LocalLimit::AddressSpace { bytes: requested }];
                let outcome = wait_root(
                    spawn_root(program, admit().unwrap(), &SpawnOptions::default()).unwrap(),
                )
                .await;
                success(&outcome, 0);
                assert_eq!(
                    outcome.limits[0].inherited,
                    Some((192 * 1024 * 1024, 256 * 1024 * 1024))
                );
                assert_eq!(outcome.limits[0].normalised, requested);
                assert_eq!(outcome.limits[0].installed, Some(installed));
                assert_eq!(outcome.notes.contains(&Note::LimitLowered), lowered);
            }
        });
        return;
    }
    run_test(async {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "inherited_address_space_hard_limit_is_respected",
                "--nocapture",
            ])
            .env(MARKER, "1");
        // SAFETY: only the re-executed test process is restricted; all storage
        // is on the stack, and setrlimit and errno access do not allocate.
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 192 * 1024 * 1024,
                    rlim_max: 256 * 1024 * 1024,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &limit) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("inherited-limit subprocess exceeded eight seconds");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
}

#[cfg(target_os = "linux")]
#[test]
fn linux_capabilities_report_support_and_runtime_probes() {
    run_test(async {
        let fds = fd_count();
        let report = capabilities();
        assert_eq!(report.metrics.len(), 5);
        for kind in [MetricKind::PeakRss, MetricKind::CpuTime] {
            let capability = report.metrics.iter().find(|c| c.kind == kind).unwrap();
            assert!(capability.compiled);
            assert_eq!(capability.probe, Ok(()));
        }
        let address_space = report
            .limits
            .iter()
            .find(|c| c.kind == LimitKind::AddressSpace)
            .unwrap();
        assert!(address_space.compiled);
        assert_eq!(address_space.probe, Ok(()));
        let commit = report
            .limits
            .iter()
            .find(|c| c.kind == LimitKind::JobCommit)
            .unwrap();
        assert!(!commit.compiled);
        assert_eq!(
            commit.probe.as_ref().unwrap_err(),
            "unsupported on this platform"
        );
        assert_eq!(report.observers.len(), 2);
        for capability in &report.observers {
            assert!(capability.compiled);
        }
        assert_eq!(report.deferred.len(), 4);
        for capability in &report.deferred {
            assert!(!capability.compiled);
            assert_eq!(
                capability.probe.as_ref().unwrap_err(),
                "unavailable feature"
            );
        }
        assert_eq!(fd_count(), fds);
    });
}

#[cfg(target_os = "macos")]
#[test]
fn macos_capabilities_advertise_kqueue_cleanup_accounting_and_no_local_limits() {
    run_test(async {
        let before = fd_count();
        let capabilities = cpforge::supervisor::capabilities();
        assert!(
            capabilities
                .limits
                .iter()
                .all(|limit| !limit.compiled && limit.probe.is_err())
        );
        for kind in [MetricKind::PeakRss, MetricKind::CpuTime] {
            let metric = capabilities
                .metrics
                .iter()
                .find(|metric| metric.kind == kind)
                .unwrap();
            assert!(metric.compiled && metric.probe.is_ok());
        }
        let observer = capabilities
            .observers
            .iter()
            .find(|observer| observer.kind == "kqueue")
            .unwrap();
        assert!(observer.compiled && observer.probe.is_ok());
        let cleanup = capabilities
            .observers
            .iter()
            .find(|observer| observer.kind == "libproc cleanup verification")
            .unwrap();
        assert!(cleanup.compiled && cleanup.probe.is_ok());
        assert_eq!(fd_count(), before);
    });
}

#[cfg(target_os = "macos")]
#[test]
fn macos_rejects_both_limit_kinds_before_workers_files_or_spawn() {
    run_test(async {
        let path = CapturePath::new();
        let baseline = debug_counters();
        let fds = fd_count();
        for limit in [
            LocalLimit::AddressSpace { bytes: 1 << 20 },
            LocalLimit::JobCommit { bytes: 1 << 20 },
        ] {
            let mut program = spec(&["hold"]);
            program.limits = vec![limit];
            program.program = path.0.with_extension("nonexistent");
            program.stdout = capture(Sink::File(path.0.clone()), 4096, Exceed::Kill, false);
            let admission = admit().unwrap();
            let ticket = admission.ticket();
            assert!(matches!(
                spawn_root(
                    program,
                    admission,
                    &SpawnOptions {
                        stdout_sink: SinkTestOptions {
                            fail_start: true,
                            ..SinkTestOptions::default()
                        },
                        ..SpawnOptions::default()
                    }
                ),
                Err(NotStarted::UnsupportedLimit)
            ));
            assert!(ticket.outcome().await.is_none());
            assert!(!path.0.exists());
            assert_eq!(debug_counters(), baseline);
            assert_eq!(fd_count(), fds);
        }
    });
}

#[cfg(all(feature = "storage-test-hooks", target_os = "linux"))]
#[test]
fn exit_during_pidfd_registration_is_observed_and_reaped_once() {
    run_test(async {
        use cpforge::supervisor::{Creator, create_root};
        let (baseline, before, fds, threads) = creation_baseline().await;
        let creator = Creator::start().unwrap();
        let gate = CreationRelease::new(false);
        let stamps = Arc::new(cpforge::supervisor::Milestones::default());
        let options = SpawnOptions {
            milestones: Some(stamps.clone()),
            registration_gate: Some(gate.0.clone()),
            ..SpawnOptions::default()
        };
        let cancel = CancelToken::new();
        let mut future = Box::pin(create_root(
            &creator,
            spec(&["exit:23"]),
            admit().unwrap(),
            &options,
            &cancel,
        ));
        at_creation_gate(&mut future, &gate).await;
        let pid = gate.0.pid().unwrap();
        until(|| process(pid).is_some_and(|p| p.state == 'Z')).await;
        assert_creation_pending(&mut future).await;
        gate.0.release();
        let root = tokio::time::timeout(allowance(1000), future.as_mut())
            .await
            .unwrap()
            .unwrap();
        let ticket = root.ticket();
        drop(future);
        let outcome = wait_root(root).await;
        success(&outcome, 23);
        assert_eq!(outcome.kill_attempts, 1);
        assert_eq!(stamps.exit_observations(), 1);
        assert_eq!(
            outcome.timing.method,
            cpforge::supervisor::WallMethod::Observed
        );
        assert!(process(pid).is_none());
        assert!(ticket.clone().outcome().await.is_none());
        assert!(ticket.outcome().await.is_none());
        drop(creator);
        quiescent().await;
        assert_eq!(children(), before);
        settled(fds, threads).await;
        assert_eq!(debug_counters(), baseline);
    });
}

#[cfg(all(feature = "storage-test-hooks", target_os = "linux"))]
#[test]
fn pidfd_open_enosys_and_emfile_fall_back_to_polling() {
    run_test(async {
        for errno in [libc::ENOSYS, libc::EMFILE] {
            let root = spawn_root(
                spec(&["exit:17"]),
                admit().unwrap(),
                &SpawnOptions {
                    pidfd_open_error: Some(errno),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
            let pid = root.pid();
            let outcome = wait_root(root).await;
            assert_eq!(outcome.status, Termination::Exited { code: 17 });
            assert_eq!(outcome.timing.observer, ObserverKind::Polling);
            assert_eq!(
                outcome.timing.method,
                cpforge::supervisor::WallMethod::Observed
            );
            assert_eq!(outcome.stop_trigger, None);
            assert!(outcome.violations.is_empty());
            assert_eq!(outcome.kill_attempts, 1);
            if errno == libc::ENOSYS {
                assert_eq!(outcome.notes, [Note::ObserverFallback]);
                assert!(outcome.errors.is_empty());
            } else {
                assert!(outcome.notes.is_empty());
                assert_eq!(outcome.errors, [SupervisorError::Register(libc::EMFILE)]);
            }
            assert!(process(pid).is_none());
        }
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancel_inside_os_creation_finishes_spawn_then_kills_and_reaps() {
    run_test(async {
        use cpforge::supervisor::{CreationGate, Creator, create_root};
        let (baseline, before, fds, threads) = creation_baseline().await;
        let creator = Creator::start().unwrap();
        let gate = CreationRelease(Arc::new(CreationGate::during_creation().unwrap()));
        let options = SpawnOptions {
            creation_gate: Some(gate.0.clone()),
            ..SpawnOptions::default()
        };
        let cancel = CancelToken::new();
        let mut future = Box::pin(create_root(
            &creator,
            spec(&["hold"]),
            admit().unwrap(),
            &options,
            &cancel,
        ));
        at_creation_gate(&mut future, &gate).await;
        // pre_exec has announced readiness: a child exists, but spawn cannot
        // return until exec closes std's creation-error pipe.
        let created: Vec<_> = children().difference(&before).copied().collect();
        assert_eq!(created.len(), 1);
        let pid = created[0];
        cancel.cancel();
        assert_creation_pending(&mut future).await;
        assert!(process(pid).is_some());
        gate.0.release();
        let root = tokio::time::timeout(allowance(1000), future.as_mut())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(root.pid(), pid);
        let ticket = root.ticket();
        drop(future);
        let outcome = tokio::time::timeout(
            Duration::from_secs(2) + allowance(100),
            wait_root_controlled(
                root,
                RunControls {
                    cancel,
                    ..controls(Duration::from_secs(5))
                },
            ),
        )
        .await
        .unwrap();
        killed(&outcome, StopTrigger::Cancelled);
        assert!(outcome.errors.is_empty(), "{outcome:?}");
        assert!(process(pid).is_none());
        assert!(ticket.outcome().await.is_none());
        drop(creator);
        drop(options);
        drop(gate);
        quiescent().await;
        assert_eq!(children(), before);
        settled(fds, threads).await;
        assert_eq!(debug_counters(), baseline);
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn root_exit_to_cleanup_result_meets_the_cleanup_deadline() {
    run_test(async {
        use cpforge::supervisor::{Milestone, Milestones};
        for delayed in [false, true] {
            let stamps = Arc::new(Milestones::default());
            let root = spawn_root(
                spec(&["exit:0"]),
                admit().unwrap(),
                &SpawnOptions {
                    milestones: Some(stamps.clone()),
                    cleanup_entry_delay: if delayed {
                        Duration::from_secs(2)
                    } else {
                        Duration::ZERO
                    },
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
            let outcome = wait_root(root).await;
            success(&outcome, 0);
            let elapsed = stamps
                .get(Milestone::CleanupResult)
                .unwrap()
                .duration_since(outcome.timing.exit_observed.unwrap());
            assert!(
                elapsed <= Duration::from_secs(1) + allowance(50),
                "{delayed}: {elapsed:?}"
            );
            // Linux always enumerates /proc entries; Darwin can report an
            // already-empty group without inspecting a delayed entry.
            #[cfg(target_os = "linux")]
            if delayed && outcome.timing.observer != ObserverKind::Polling {
                assert_eq!(
                    outcome.termination.cleanup,
                    Cleanup::Unverified {
                        reason: UnverifiedReason::ScanIncomplete
                    }
                );
            }
        }
    });
}
