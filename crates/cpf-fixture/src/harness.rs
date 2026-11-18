//! Test harness independent of the supervisor. All fixtures have piped standard
//! streams and an independent containment watchdog, including fixtures that do
//! not use `ready`.

use crate::platform::{self, HANDSHAKE_TIMEOUT};
use std::io;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Locate the binary using `CPF_FIXTURE_BIN`, or beside the current test's
/// `deps/` directory (`target/<profile>/cpf-fixture[.exe]`). This also supports
/// target triples and custom target directories. Run `cargo test --workspace`
/// to build it, or `cargo build -p cpf-fixture --bin cpf-fixture` before running
/// only root-package tests. See the crate README for an explicit-path helper.
pub fn fixture_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CPF_FIXTURE_BIN") {
        return path.into();
    }
    let exe = std::env::current_exe().expect("locate current test executable");
    let directory = exe.parent().expect("test executable has a parent");
    let profile = if directory.file_name().is_some_and(|name| name == "deps") {
        directory.parent().expect("deps has a profile parent")
    } else {
        directory
    };
    profile.join(format!("cpf-fixture{}", std::env::consts::EXE_SUFFIX))
}

/// Spawn configuration. Standard streams are always piped; tests take the
/// corresponding public handles from `Fixture` to read or close them.
#[derive(Debug)]
pub struct Options {
    pub deadline: Duration,
    /// Disable only for tests of a missing control channel. The watchdog and
    /// containment remain active.
    pub control_channel: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(30),
            control_channel: true,
        }
    }
}

/// A process identity pinned by the READY/ACK handshake. On Windows this owns a
/// shared process handle acquired before ACK. Zombies count as gone on Unix.
#[derive(Clone, Debug)]
pub struct Identity {
    pub pid: u32,
    pub start_id: u64,
    process: platform::Process,
}

impl Identity {
    pub fn is_live(&self) -> bool {
        self.process.is_live(self.pid, self.start_id)
    }
}

/// Owns the root process, control channel, and independent watchdog. Drop kills
/// containment and reaps the root even when a test panics. Unix processes that
/// call `setsid()` escape this containment and must be cleaned up by the test.
#[derive(Debug)]
pub struct Fixture {
    child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    control: platform::Server,
    tree: Arc<platform::Tree>,
    expired: Arc<AtomicBool>,
    reported: bool,
    stop: Option<mpsc::Sender<()>>,
    watchdog: Option<JoinHandle<()>>,
}

impl Fixture {
    pub fn spawn(steps: &[&str], opts: Options) -> Self {
        Self::try_spawn(steps, opts).expect("spawn cpf-fixture (build with cargo test --workspace)")
    }

    pub fn try_spawn(steps: &[&str], opts: Options) -> io::Result<Self> {
        let (control, inherited) = platform::channel()?;
        let mut tree = platform::Tree::new()?;
        let mut command = Command::new(fixture_path());
        command
            .args(steps)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("CPF_FIXTURE_CTRL")
            .env_remove("CPF_FIXTURE_ACK")
            .env_remove("CPF_FIXTURE_GATE");
        tree.configure(&mut command);
        if opts.control_channel {
            inherited.configure(&mut command);
        }
        let mut child = command.spawn()?;
        if let Err(error) = tree.attach(&child) {
            let _ = tree.kill();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        drop(inherited);
        let tree = Arc::new(tree);
        let expired = Arc::new(AtomicBool::new(false));
        let (stop, receive) = mpsc::channel();
        let watchdog_tree = Arc::clone(&tree);
        let watchdog_expired = Arc::clone(&expired);
        let watchdog = match std::thread::Builder::new()
            .name("cpf-fixture-watchdog".into())
            .spawn(move || {
                if matches!(
                    receive.recv_timeout(opts.deadline),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    watchdog_expired.store(true, Ordering::SeqCst);
                    let _ = watchdog_tree.kill();
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = tree.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child,
            control,
            tree,
            expired,
            reported: false,
            stop: Some(stop),
            watchdog: Some(watchdog),
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Receive and pin each identity before acknowledging it. PID-addressed
    /// ACKs are peeked by all descendants but consumed only by their addressee;
    /// repeated `ready` steps cannot steal another process's acknowledgement.
    /// Any protocol failure kills containment and fails the test.
    pub fn await_ready(&mut self, count: usize) -> Vec<Identity> {
        let result = (|| -> io::Result<Vec<Identity>> {
            let mut identities = Vec::with_capacity(count);
            for _ in 0..count {
                let line = self.control.receive(HANDSHAKE_TIMEOUT)?;
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() != 3 || fields[0] != "READY" || !line.ends_with('\n') {
                    return Err(io::Error::other(format!("invalid READY: {line:?}")));
                }
                let pid = fields[1].parse().map_err(io::Error::other)?;
                let start_id = fields[2].parse().map_err(io::Error::other)?;
                if pid == 0 || (cfg!(unix) && pid > i32::MAX as u32) {
                    return Err(io::Error::other("invalid READY pid"));
                }
                let process = platform::Process::open(pid, start_id)?;
                identities.push(Identity {
                    pid,
                    start_id,
                    process,
                });
                self.control.acknowledge(pid)?;
            }
            Ok(identities)
        })();
        match result {
            Ok(identities) => identities,
            Err(error) => {
                let _ = self.tree.kill();
                panic!("cpf-fixture readiness failed: {error}");
            }
        }
    }

    pub fn kill_tree(&self) -> io::Result<()> {
        self.tree.kill()
    }

    pub fn watchdog_expired(&self) -> bool {
        self.expired.load(Ordering::SeqCst)
    }

    fn check_watchdog(&mut self) -> io::Result<()> {
        if self.watchdog_expired() {
            self.reported = true;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "cpf-fixture watchdog deadline exceeded",
            ));
        }
        Ok(())
    }

    /// Wait for the root. A watchdog kill is an error rather than a successful
    /// observed exit. Receiving this error counts as reporting the failure.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        // Match Child::wait: release any stdin handle still owned here so a
        // script reading to EOF can complete. Taken handles belong to the caller.
        drop(self.stdin.take());
        let status = self.child.wait()?;
        self.check_watchdog()?;
        Ok(status)
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        self.check_watchdog()?;
        Ok(status)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Cancel and join the timer before the final kill so no timer can race
        // after teardown and accidentally target a subsequently reused PGID.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
        let _ = self.tree.kill();
        let _ = self.child.wait();
        if self.watchdog_expired() && !self.reported && !std::thread::panicking() {
            panic!("cpf-fixture watchdog deadline exceeded (unreported)");
        }
    }
}

/// Assert no supplied identity is live before the deadline. This does not assert
/// disappearance of Unix process table entries: orphan zombies may remain until
/// reaped by the system's reaper.
pub fn assert_all_gone(identities: &[Identity], within: Duration) {
    let until = Instant::now() + within;
    loop {
        let live: Vec<_> = identities
            .iter()
            .filter(|identity| identity.is_live())
            .collect();
        if live.is_empty() {
            return;
        }
        assert!(
            Instant::now() < until,
            "fixture identities still live: {live:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
