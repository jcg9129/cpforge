//! All native process/pipe operations and unsafe code belong under this private
//! module. Tokio owns reactor registration and overlapped-operation lifetimes.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(super) use unix::{Reader, Writer, into_stdio, pipe, read, reset_sigchld};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::{Reader, Writer, into_stdio, into_stdio_for_tests, pipe, read};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::{
    CleanupScan, Observer, cleanup_scan, exit_check, init_process, kill_group, kill_root, observe,
    observe_status, observer, reap, spawn,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use unix::direct_pipe;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use unix::{InputEnd, console_write, input_pipe};

mod capture_file;
pub(super) use capture_file::CaptureFile;

#[cfg(target_os = "linux")]
mod limits;
#[cfg(target_os = "linux")]
pub(super) use limits::{prepare_limits, probe_capabilities};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod transcript_file;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use transcript_file::{TranscriptEvents, TranscriptFile, TranscriptFiles};

#[cfg(all(target_os = "linux", debug_assertions))]
pub(super) use unix::resize_pipe;

#[cfg(target_os = "macos")]
pub(in crate::supervisor) mod macos;

#[cfg(any(target_os = "linux", target_os = "macos"))]
/// Root plus waited descendants: max RSS in bytes, summed CPU durations.
#[derive(Clone, Copy, Debug)]
pub(in crate::supervisor) struct RawRusage {
    pub maxrss_bytes: u64,
    pub user: std::time::Duration,
    pub system: std::time::Duration,
}
