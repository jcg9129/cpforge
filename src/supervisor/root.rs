//! Root ownership shared with the future platform backends.

use super::cleanup::{self, Admission, TransferTicket};
use super::{
    ChildEnd, EventSender, InstalledLimit, LimitValidation, LocalLimit, Measurement, MetricKind,
    SinkTestOptions, Stream, StreamCapture, StreamPolicy,
};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "root_unix.rs"]
pub(super) mod unix;

/// Persistent cancellation intent. Watch retains the flag, so registering a
/// waiter concurrently with cancel (or dropping a waiter) cannot lose a wakeup.
#[derive(Clone, Debug)]
pub struct CancelToken(Arc<watch::Sender<Option<Instant>>>);

impl Default for CancelToken {
    fn default() -> Self {
        Self(Arc::new(watch::channel(None).0))
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.send_if_modified(|requested| {
            if requested.is_some() {
                return false;
            }
            *requested = Some(Instant::now());
            true
        });
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.borrow().is_some()
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn requested_at(&self) -> Option<Instant> {
        *self.0.borrow()
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|requested| requested.is_some()).await;
    }
}

#[derive(Clone, Debug)]
pub struct RunControls {
    pub wall_deadline: Duration,
    pub cancel: CancelToken,
    pub events: Option<EventSender>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopTrigger {
    WallDeadline,
    OutputBudget(Stream),
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum UnverifiedReason {
    NamespaceUnknown,
    ProcFiltered,
    ProcUnavailable,
    ScanFailed,
    RootNotObserved,
    ObserverFailed,
    Malformed,
    ScanIncomplete,
    ReapDeadline,
    Unsupported,
    Io,
    InfoFailed,
    Truncated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Cleanup {
    Confirmed,
    Incomplete { survivors: u32 },
    Unverified { reason: UnverifiedReason },
}

#[derive(Clone, Copy, Debug)]
pub struct TerminationEvidence {
    pub requested_at: Option<Instant>,
    pub method: Option<&'static str>,
    pub cleanup: Cleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Violation {
    WallDeadline { observed_by: Instant },
    OutputBudget { stream: Stream, offset: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallMethod {
    Observed,
}

pub const DEADLINE_GRACE: Duration = Duration::from_millis(50);

/// Complete, already resolved inputs to synchronous root creation.
#[derive(Debug)]
pub struct ProgramSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub stdin: StdinSpec,
    pub stdout: OutputSpec,
    pub stderr: OutputSpec,
    pub limits: Vec<LocalLimit>,
    pub measure: Vec<MetricKind>,
}

#[derive(Debug)]
pub enum StdinSpec {
    Null,
    File(PathBuf),
    Forward,
    Connected,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[doc(hidden)]
    Pipe(super::sys::InputEnd),
}

#[derive(Debug)]
pub enum OutputSpec {
    Null,
    Connected,
    Pipe(ChildEnd),
    Capture(StreamPolicy),
}

#[derive(Debug)]
pub enum NotStarted {
    Unsupported,
    CancelledBeforeStart,
    Worker,
    CleanupCapacity,
    UnsupportedLimit,
    InvalidLimit(LimitValidation),
    Limit(i32),
    CreateFailed { api: &'static str, raw_error: i32 },
    Setup(io::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Termination {
    Exited { code: i64 },
    Signalled { signal: i32, core_dumped: bool },
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObserverKind {
    Pidfd,
    Kqueue,
    Polling,
}

#[derive(Clone, Copy, Debug)]
pub struct RootTiming {
    pub setup: Duration,
    pub create_end: Instant,
    pub exit_observed: Option<Instant>,
    pub observer: ObserverKind,
    pub wall: Option<Duration>,
    pub method: WallMethod,
    pub grace: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Note {
    ObserverFallback,
    Transferred,
    DeadlineGrace,
    LimitLowered,
    LateTrigger(StopTrigger),
}

impl Note {
    /// Task API spelling for the contract's DeadlineGrace note.
    #[allow(non_upper_case_globals)]
    pub const DeadlineAmbiguous: Self = Self::DeadlineGrace;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorError {
    Register(i32),
    Wait(i32),
    Kill(i32),
    Reap,
    ReapTimeout,
}

#[derive(Clone, Debug)]
pub struct RootOutcome {
    pub kill_attempts: u32,
    pub stdout: Option<StreamCapture>,
    pub stderr: Option<StreamCapture>,
    pub status: Termination,
    pub stop_trigger: Option<StopTrigger>,
    pub enforced_at: Option<Duration>,
    pub termination: TerminationEvidence,
    pub violations: Vec<Violation>,
    pub measurements: Vec<Measurement>,
    pub limits: Vec<InstalledLimit>,
    pub timing: RootTiming,
    pub notes: Vec<Note>,
    pub errors: Vec<SupervisorError>,
}

#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct SpawnOptions {
    pub stdout_sink: SinkTestOptions,
    pub stderr_sink: SinkTestOptions,
    pub force_polling_observer: bool,
    /// Test injection: report EINVAL from the limit pre_exec phase.
    pub fail_limit_install: bool,
    /// Test gate: waiting begins only after this token is cancelled.
    pub supervisor_gate: Option<CancelToken>,
    pub timer_first: bool,
    pub observer_never_ready: bool,
    pub reap_never_ready: bool,
    /// Test gate: delay exit observation without delaying kill requests.
    pub exit_observation_gate: Option<CancelToken>,
    /// Test injection: slow each cleanup entry, bounded by the scan deadline.
    pub cleanup_entry_delay: Duration,
    /// Test gate: keep transferred root facts pending without blocking the service.
    pub transfer_root_gate: Option<CancelToken>,
}

/// Owns an unreaped root. Dropping this value (or its wait future) kills its
/// group while permitted and transfers the full outcome to the cleanup service.
#[must_use = "wait_root must observe and reap the root"]
#[derive(Debug)]
pub struct Spawned {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    inner: Box<unix::Root>,
}

impl Spawned {
    /// Cloneable ticket; each clone receives the same finalized outcome, or None
    /// after normal completion. Holding a ticket does not hold a cleanup slot.
    pub fn ticket(&self) -> TransferTicket {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.inner.ticket()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            unreachable!("this platform cannot spawn a root yet")
        }
    }

    /// Root pid, also the process-group id. This is identity information only;
    /// signalling authority remains private to the live-run registry.
    pub fn pid(&self) -> u32 {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.inner.pid()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            unreachable!("this platform cannot spawn a root yet")
        }
    }
}

/// Runs on the job's creation thread, with an entered I/O-enabled Tokio runtime.
/// The program path must be absolute; `env` is the complete final environment.
/// A failed spawn returns its slot once any started sink workers have exited.
pub fn spawn_root(
    spec: ProgramSpec,
    admission: Admission,
    opts: &SpawnOptions,
) -> Result<Spawned, NotStarted> {
    if matches!(spec.stdin, StdinSpec::Connected)
        || matches!(spec.stdout, OutputSpec::Connected)
        || matches!(spec.stderr, OutputSpec::Connected)
    {
        return Err(NotStarted::Unsupported);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::spawn(spec, admission, opts).map(|inner| Spawned {
            inner: Box::new(inner),
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        if !spec.limits.is_empty() {
            return Err(NotStarted::UnsupportedLimit);
        }
        let _ = (spec, admission, opts);
        Err(NotStarted::CreateFailed {
            api: "unsupported-platform",
            raw_error: 0,
        })
    }
}

/// Observe exit without consuming it, kill the group, revoke signalling, reap.
pub async fn wait_root(spawned: Spawned, controls: RunControls) -> RootOutcome {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::wait(spawned.inner, controls).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (spawned, controls);
        unreachable!("this platform cannot spawn a root yet")
    }
}

/// Restore SIGCHLD's default disposition on Unix, mark inherited descriptors 3
/// and above close-on-exec on Linux and macOS, and create the bounded cleanup
/// service.
/// Call before starting other work; failure must abort start-up cleanly
/// (execution contract §3.1, §3.3, §3.5). Idempotent and thread-safe.
pub fn init_process() -> io::Result<()> {
    static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _lock = INIT.lock().unwrap_or_else(|error| error.into_inner());
    #[cfg(unix)]
    {
        super::sys::reset_sigchld()?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            unix::init_process()?;
        }
    }
    cleanup::init()
}

/// Signal-control thread's force-exit barrier. It uses only live capabilities;
/// creation and physical cleanup are never awaited.
pub(crate) fn force_kill_active() {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    unix::force_kill_active();
}

/// Serialises a non-atomic descriptor creation (pipe, then `FD_CLOEXEC`) with
/// child creation on macOS, which has no `pipe2`; a child spawned in between
/// would otherwise inherit the descriptor.
#[cfg(target_os = "macos")]
pub(crate) fn creation_lock() -> std::sync::MutexGuard<'static, ()> {
    super::sys::macos::creation_lock()
}
