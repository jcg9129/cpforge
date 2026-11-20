use cpf_fixture::harness::{Fixture, Options, assert_all_gone, fixture_path};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

// This reference makes Cargo build the ordinary executable before workspace
// tests, including tests in the root package, start running.
const BUILT_BINARY: &str = env!("CARGO_BIN_EXE_cpf-fixture");

fn options() -> Options {
    Options {
        deadline: Duration::from_secs(8),
        ..Options::default()
    }
}

fn spawn(steps: &[&str]) -> Fixture {
    assert!(std::path::Path::new(BUILT_BINARY).is_file());
    Fixture::spawn(steps, options())
}

fn read_all(mut stream: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    bytes
}

fn collect(fixture: &mut Fixture) -> (ExitStatus, Vec<u8>, Vec<u8>) {
    drop(fixture.stdin.take());
    let stdout = fixture.stdout.take().unwrap();
    let stderr = fixture.stderr.take().unwrap();
    std::thread::scope(|scope| {
        let stdout = scope.spawn(|| read_all(stdout));
        let stderr = scope.spawn(|| read_all(stderr));
        let status = fixture.wait().unwrap();
        (status, stdout.join().unwrap(), stderr.join().unwrap())
    })
}

#[test]
fn binary_is_discoverable() {
    let expected = std::env::var_os("CPF_FIXTURE_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(BUILT_BINARY));
    assert_eq!(fixture_path(), expected);
    let mut fixture = spawn(&["exit:0"]);
    assert!(fixture.wait().unwrap().success());
}

#[test]
fn whole_script_is_validated_before_any_step() {
    for invalid in [
        "unknown",
        "write:stdin:10",
        "write:stdout:no",
        "write:stdout:1:xx",
        "exit:-1",
        "sleep:18446744073709551616",
        "close:other",
        "flood:-1",
        "touch-mem:18446744073709551615",
        "signal:NOPE",
        "spawn:1:unknown",
        "spawn:1:spawn:1:spawn:1:hold",
        "ready:extra",
        "echo:extra",
    ] {
        let mut fixture = spawn(&["write:stdout:1", invalid]);
        let (status, stdout, stderr) = collect(&mut fixture);
        assert_eq!(status.code(), Some(64), "{invalid}");
        assert!(
            stdout.is_empty(),
            "script executed before validating {invalid}"
        );
        assert!(!stderr.is_empty(), "{invalid}");
    }
    let mut empty = spawn(&[]);
    assert_eq!(empty.wait().unwrap().code(), Some(64));
}

#[test]
fn writes_exact_bytes_and_custom_byte() {
    let mut fixture = spawn(&["write:stdout:4097", "write:stderr:17:z"]);
    let (status, stdout, stderr) = collect(&mut fixture);
    assert!(status.success());
    assert_eq!(stdout, vec![b'x'; 4097]);
    assert_eq!(stderr, vec![b'z'; 17]);
}

#[test]
fn echo_argv_preserves_data_and_empty_elements() {
    let mut fixture = spawn(&["echo-argv", "a b", "", "exit:9", "line1\nline2", "$x\\\"'"]);
    let (status, stdout, stderr) = collect(&mut fixture);
    assert!(status.success());
    assert_eq!(stdout, b"a b\0\0exit:9\0line1\nline2\0$x\\\"'\0");
    assert!(stderr.is_empty());
}

#[test]
fn flood_drains_both_streams_concurrently() {
    let mut fixture = spawn(&["flood:1048576"]);
    let (status, stdout, stderr) = collect(&mut fixture);
    assert!(status.success());
    assert_eq!(stdout, vec![b'x'; 1_048_576]);
    assert_eq!(stderr, vec![b'x'; 1_048_576]);
}

#[test]
fn exit_code_three() {
    let mut fixture = spawn(&["exit:3"]);
    assert_eq!(fixture.wait().unwrap().code(), Some(3));
}

#[cfg(unix)]
#[test]
fn kill_signal_status() {
    use std::os::unix::process::ExitStatusExt;
    let mut fixture = spawn(&["signal:KILL"]);
    assert_eq!(fixture.wait().unwrap().signal(), Some(libc::SIGKILL));
}

#[cfg(windows)]
#[test]
fn windows_full_width_exit_and_segv() {
    for step in ["exit:3221225477", "signal:SEGV"] {
        let mut fixture = spawn(&[step]);
        assert_eq!(fixture.wait().unwrap().code().unwrap() as u32, 0xC000_0005);
    }
    for step in ["signal:KILL", "signal:TERM", "escape"] {
        let mut fixture = spawn(&["write:stdout:1", step]);
        let (status, stdout, _) = collect(&mut fixture);
        assert_eq!(status.code(), Some(64));
        assert!(stdout.is_empty());
    }
}

#[test]
fn echo_flushes_each_line_before_the_next_is_sent() {
    let mut fixture = spawn(&["echo"]);
    let mut input = fixture.stdin.take().unwrap();
    let mut output = BufReader::new(fixture.stdout.take().unwrap());
    for line in ["first\n", "second\n", "third\n"] {
        input.write_all(line.as_bytes()).unwrap();
        input.flush().unwrap();
        let mut echoed = String::new();
        output.read_line(&mut echoed).unwrap();
        assert_eq!(echoed, line);
    }
    drop(input);
    assert!(fixture.wait().unwrap().success());
}

#[test]
fn drain_stdin_counts_bytes() {
    let mut fixture = spawn(&["drain-stdin"]);
    let bytes = vec![0; 100_003];
    fixture.stdin.take().unwrap().write_all(&bytes).unwrap();
    let (status, stdout, stderr) = collect(&mut fixture);
    assert!(status.success());
    assert_eq!(stdout, b"100003\n");
    assert!(stderr.is_empty());
}

#[test]
fn ready_pins_tree_identities_and_group_kill_removes_them() {
    let mut fixture = spawn(&["ready", "spawn:2:ready,hold", "hold"]);
    let identities = fixture.await_ready(3);
    assert_eq!(identities.len(), 3);
    assert!(identities.iter().all(|identity| identity.is_live()));
    assert_eq!(
        identities
            .iter()
            .filter(|id| id.pid == fixture.id())
            .count(),
        1
    );
    fixture.kill_tree().unwrap();
    assert_all_gone(&identities, Duration::from_secs(2));
    fixture.wait().unwrap();
}

#[test]
fn nested_spawn_and_repeated_ready_do_not_steal_acks() {
    let mut fixture = spawn(&[
        "ready",
        "spawn:2:ready,ready,spawn:1:ready;ready;hold,hold",
        "hold",
    ]);
    let identities = fixture.await_ready(9);
    let mut pids: Vec<_> = identities.iter().map(|identity| identity.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    assert_eq!(pids.len(), 5);
    assert!(identities.iter().all(|identity| identity.is_live()));
    fixture.kill_tree().unwrap();
    assert_all_gone(&identities, Duration::from_secs(2));
}

#[cfg(unix)]
#[test]
fn killed_root_is_gone_before_it_is_reaped() {
    use std::os::unix::process::ExitStatusExt;

    let mut fixture = spawn(&["ready", "hold"]);
    let identities = fixture.await_ready(1);
    assert!(identities[0].is_live());
    let mut changed_identity = identities[0].clone();
    changed_identity.start_id += 1;
    assert!(!changed_identity.is_live());
    fixture.kill_tree().unwrap();

    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::uninit();
    loop {
        // SAFETY: info is correctly sized writable storage and the PID names
        // our child. WNOWAIT observes its exit without reaping the zombie.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                fixture.id(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            break;
        }
        assert_eq!(
            io::Error::last_os_error().kind(),
            io::ErrorKind::Interrupted
        );
    }
    // SAFETY: signal zero only probes our unreaped child without sending a signal.
    assert_eq!(unsafe { libc::kill(fixture.id() as i32, 0) }, 0);
    assert_all_gone(&identities, Duration::from_secs(2));
    assert_eq!(fixture.wait().unwrap().signal(), Some(libc::SIGKILL));
    assert!(!identities[0].is_live());
}

#[cfg(unix)]
#[test]
fn escaped_child_survives_group_kill_and_is_cleaned_up_explicitly() {
    struct KillPid(u32);
    impl Drop for KillPid {
        fn drop(&mut self) {
            // SAFETY: the test pins this fixture identity while alive, and this
            // guard is dropped immediately after the survival assertion.
            unsafe {
                libc::kill(self.0 as i32, libc::SIGKILL);
            }
        }
    }
    let mut fixture = spawn(&["ready", "spawn:1:escape,ready,hold", "hold"]);
    let identities = fixture.await_ready(2);
    let escaped = identities
        .iter()
        .find(|identity| identity.pid != fixture.id())
        .unwrap();
    let cleanup = KillPid(escaped.pid);
    fixture.kill_tree().unwrap();
    fixture.wait().unwrap();
    assert!(
        escaped.is_live(),
        "setsid descendant should escape the group"
    );
    drop(cleanup);
    assert_all_gone(&identities, Duration::from_secs(2));
}

#[test]
fn touched_memory_stays_alive_until_killed() {
    let mut fixture = spawn(&["touch-mem:64", "ready", "hold"]);
    let identities = fixture.await_ready(1);
    assert!(identities[0].is_live());
    assert!(fixture.try_wait().unwrap().is_none());
    fixture.kill_tree().unwrap();
    assert_all_gone(&identities, Duration::from_secs(2));
}

#[test]
fn watchdog_kills_hold_and_reports_expiry() {
    let mut fixture = Fixture::spawn(
        &["ready", "hold"],
        Options {
            deadline: Duration::from_secs(1),
            ..options()
        },
    );
    let identities = fixture.await_ready(1);
    let started = Instant::now();
    let error = fixture.wait().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(fixture.watchdog_expired());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_all_gone(&identities, Duration::from_secs(2));
}

#[test]
fn unreported_watchdog_failure_panics_on_drop() {
    let failure = std::panic::catch_unwind(|| {
        let mut fixture = Fixture::spawn(
            &["hold"],
            Options {
                deadline: Duration::from_secs(1),
                ..options()
            },
        );
        // EOF arrives only after the independent watchdog kills the process.
        assert!(read_all(fixture.stdout.take().unwrap()).is_empty());
        assert!(fixture.watchdog_expired());
        drop(fixture);
    });
    assert!(failure.is_err());
}

#[test]
fn ready_without_control_channel_is_usage_error() {
    let mut fixture = Fixture::spawn(
        &["write:stdout:1", "ready"],
        Options {
            control_channel: false,
            ..options()
        },
    );
    let (status, stdout, stderr) = collect(&mut fixture);
    assert_eq!(status.code(), Some(64));
    assert!(stdout.is_empty());
    assert!(String::from_utf8_lossy(&stderr).contains("CPF_FIXTURE_CTRL"));
}

#[test]
fn unacknowledged_ready_times_out_before_executing_next_step() {
    let mut fixture = spawn(&["ready", "write:stdout:1"]);
    // Deliberately leave READY unacknowledged. The process's own five-second
    // timeout must fire before the independent eight-second watchdog.
    let (status, stdout, stderr) = collect(&mut fixture);
    assert_eq!(status.code(), Some(65));
    assert!(stdout.is_empty());
    assert!(String::from_utf8_lossy(&stderr).contains("timed out"));
    assert!(!fixture.watchdog_expired());
}

#[test]
fn missing_ready_kills_containment_and_fails_the_test() {
    let mut fixture = spawn(&["hold"]);
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture.await_ready(1);
    }));
    assert!(failure.is_err());
    assert!(!fixture.wait().unwrap().success());
    assert!(!fixture.watchdog_expired());
}

#[test]
fn watchdog_kills_descendants_after_root_exit() {
    let mut fixture = Fixture::spawn(
        &["ready", "spawn:1:ready,hold", "exit:0"],
        Options {
            deadline: Duration::from_secs(1),
            ..options()
        },
    );
    let identities = fixture.await_ready(2);
    assert!(fixture.wait().unwrap().success());
    // A descendant holds stdout open after root exit. EOF synchronizes with
    // the watchdog's tree kill, without polling or sleeps in the test.
    assert!(read_all(fixture.stdout.take().unwrap()).is_empty());
    assert_eq!(fixture.wait().unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_all_gone(&identities, Duration::from_secs(2));
}

#[test]
fn close_streams_keeps_process_alive() {
    for stream in ["stdout", "stderr"] {
        let mut fixture = spawn(&[&format!("close:{stream}"), "ready", "hold"]);
        let identities = fixture.await_ready(1);
        let closed: Box<dyn Read> = if stream == "stdout" {
            Box::new(fixture.stdout.take().unwrap())
        } else {
            Box::new(fixture.stderr.take().unwrap())
        };
        assert!(read_all(closed).is_empty());
        assert!(identities[0].is_live());
        fixture.kill_tree().unwrap();
        assert_all_gone(&identities, Duration::from_secs(2));
    }
    let mut fixture = spawn(&["close:stdin", "ready", "hold"]);
    let identities = fixture.await_ready(1);
    assert!(identities[0].is_live());
    fixture.kill_tree().unwrap();
}

#[cfg(unix)]
#[test]
fn ignore_term_does_not_block_the_harness_kill() {
    let mut fixture = spawn(&["ignore-term", "ready", "hold"]);
    let identities = fixture.await_ready(1);
    // SAFETY: signal the known live root; it installed SIG_IGN before READY.
    assert_eq!(unsafe { libc::kill(fixture.id() as i32, libc::SIGTERM) }, 0);
    assert!(identities[0].is_live());
    fixture.kill_tree().unwrap();
    assert_all_gone(&identities, Duration::from_secs(2));
}

#[test]
fn drop_kills_the_tree_and_reaps_root() {
    let identities = {
        let mut fixture = spawn(&["ready", "spawn:1:ready,hold", "hold"]);
        fixture.await_ready(2)
    };
    assert_all_gone(&identities, Duration::from_secs(2));
    #[cfg(unix)]
    {
        let mut status = 0;
        // SAFETY: WNOHANG never blocks, status is a valid writable pointer.
        assert_eq!(
            unsafe { libc::waitpid(identities[0].pid as i32, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
