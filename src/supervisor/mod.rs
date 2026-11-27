//! Standard-stream pipes and root-process lifecycle ownership.
//!
//! `sys` is private and is the only production module permitted to call OS
//! process/pipe APIs or contain unsafe code. Resource ownership crosses this
//! boundary through owning types, never borrowed raw descriptors or handles.
//!
//! Program-wide: never wait on -1 or a process group; SIGCHLD stays SIG_DFL,
//! with SA_NOCLDWAIT unset. Only sys may observe or consume child status.

use std::io;
use std::process::Stdio;

mod capture;
mod cleanup;
mod creation;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod creation_worker;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use creation::create_root;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use creation::post_run;
pub use creation::{CreationError, Creator};
pub(crate) mod file_io;
pub use file_io::{OutputFileSpec, StagedInput};
mod events;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod forward;
pub use capture::{
    CaptureLoss, Exceed, Sink, SinkGate, SinkTestOptions, Stream, StreamCapture, StreamPolicy,
};
pub use events::{EVENT_BUDGET, EVENT_CHUNK, EventReceiver, EventSender, OutputChunk};
mod accounting;
pub use accounting::*;
mod root;
pub mod session;
mod sys;
#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
mod test_hooks;
pub(crate) use cleanup::transfer_persistence;
pub use cleanup::{
    Admission, Counters, Credits, GlobalBounds, ThreadCredits, TransferTicket, admit, admit_pair,
    debug_counters, global_bounds,
};
pub use root::*;
#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
#[doc(hidden)]
pub use test_hooks::{
    CreationGate, Milestone, Milestones, PipeTestGuard, force_kill_active_for_test,
    persistence_handovers,
};

/// Reserve both job worker credits in one operation before either thread starts.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn job_credits() -> Result<[Credits; 2], NotStarted> {
    cleanup::init().map_err(|_| NotStarted::Worker)?;
    let mut credits = cleanup::global_bounds()
        .expect("initialized bounds")
        .thread_credits
        .try_reserve(2)
        .ok_or(NotStarted::Worker)?
        .split();
    let persistence = credits.pop().expect("two job credits");
    Ok([credits.pop().expect("two job credits"), persistence])
}

/// Persistence shares the process-wide credit bound without changing signal
/// setup. A stalled worker retains this credit until its syscall returns.
pub(crate) fn persistence_credit() -> io::Result<Credits> {
    cleanup::init()?;
    cleanup::global_bounds()
        .expect("initialized bounds")
        .thread_credits
        .try_reserve(1)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "persistence worker capacity exhausted",
            )
        })
}

/// One reader owned by the parent and one blocking writer for the child.
#[derive(Debug)]
pub struct StdPipe {
    pub parent: ParentReader,
    pub child: ChildEnd,
}

/// Create a pipe for either stdout or stderr.
///
/// Requires an entered Tokio runtime with I/O enabled. The future creation
/// thread must enter that runtime before calling this synchronous setup API.
/// Both ends start close-on-exec/non-inheritable. On Windows, the async server
/// connection is completed on the first read, after the client has been opened.
pub fn stdout_pipe() -> io::Result<StdPipe> {
    let (parent, child) = sys::pipe()?;
    Ok(StdPipe {
        parent: ParentReader { inner: parent },
        child: ChildEnd { inner: child },
    })
}

/// Non-blocking reader. Dropping it releases its owned pipe end.
#[derive(Debug)]
pub struct ParentReader {
    inner: sys::Reader,
}

impl ParentReader {
    /// Read up to 64 KiB, normalising native results per execution contract §5.1.
    /// An empty buffer is rejected rather than mistaken for EOF.
    pub async fn read_chunk(&mut self, buf: &mut [u8]) -> ReadResult {
        let len = buf.len().min(64 * 1024);
        if len == 0 {
            return ReadResult::Failed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read_chunk requires a nonempty buffer",
            ));
        }
        loop {
            match sys::read(&mut self.inner, &mut buf[..len]).await {
                Ok(0) => return ReadResult::Eof,
                Ok(n) => return ReadResult::Data(n),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return ReadResult::Failed(error),
            }
        }
    }
}

/// Normalised pipe read outcome; EOF depends on all writers closing.
#[derive(Debug)]
pub enum ReadResult {
    Data(usize),
    Eof,
    Failed(io::Error),
}

/// Owns the child writer, closing it exactly once unless ownership is moved.
#[derive(Debug)]
pub struct ChildEnd {
    inner: sys::Writer,
}

impl ChildEnd {
    /// Transfer ownership to standard I/O without changing inheritance flags.
    pub fn into_stdio(self) -> Stdio {
        sys::into_stdio(self.inner)
    }

    /// Test-only adapter for `std::process::Command` on Windows: transfer an
    /// inheritable duplicate to Stdio and close the original non-inheritable end.
    /// Production backends must manage inheritance with their handle list.
    #[cfg(windows)]
    #[doc(hidden)]
    pub fn into_stdio_for_tests(self) -> io::Result<Stdio> {
        sys::into_stdio_for_tests(self.inner)
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(test, feature = "storage-test-hooks")
))]
pub(crate) mod file_io_listing;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(test, feature = "storage-test-hooks")
))]
mod file_io_copy;

#[cfg(all(
    feature = "storage-test-hooks",
    any(test, target_os = "linux", target_os = "macos")
))]
pub(crate) mod file_io_hooks;

#[cfg(test)]
pub(crate) use sys::{ProcessSample, process_sample, thread_report};
