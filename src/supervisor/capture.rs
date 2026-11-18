//! Capture records and platform-independent pipeline policies.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug)]
pub enum Sink {
    File(PathBuf),
    Discard,
    /// Bounded in-memory retention, with no sink worker.
    Memory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exceed {
    Kill,
    Discard,
}

#[derive(Clone, Debug)]
pub struct StreamPolicy {
    pub sink: Sink,
    pub budget: u64,
    pub on_exceed: Exceed,
    pub events: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureLoss {
    BudgetDiscard,
    WriteError(i32),
    WriteStalled,
    NoEofBeforeDrainDeadline,
    PipeRead(i32),
    ForwardIncomplete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamCapture {
    pub observed: u64,
    pub retained: Vec<u8>,
    pub stored: u64,
    pub forwarded: u64,
    pub eof: bool,
    pub exceeded_at: Option<u64>,
    pub losses: Vec<CaptureLoss>,
    pub sink: Option<PathBuf>,
}

impl StreamCapture {
    pub fn complete(&self) -> bool {
        self.eof && self.losses.is_empty() && self.exceeded_at.is_none()
    }

    /// A budget loss does not mask independent infrastructure failures (§6.4).
    pub fn has_infrastructure_loss(&self) -> bool {
        self.losses
            .iter()
            .any(|loss| !matches!(loss, CaptureLoss::BudgetDiscard))
    }
}

// The pipeline itself contains no platform APIs. Future runtime backends can
// use this module unchanged; currently only Linux creates roots.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod pipeline;
pub(super) use pipeline::FailedCaptures;
#[cfg(target_os = "linux")]
pub(super) use pipeline::{Captures, DRAIN};

#[doc(hidden)]
#[derive(Debug, Default)]
pub struct SinkGate {
    open: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
    entered: std::sync::atomic::AtomicBool,
}

impl SinkGate {
    pub fn release(&self) {
        *self.open.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.wake.notify_all();
    }

    pub fn entered(&self) -> bool {
        self.entered.load(std::sync::atomic::Ordering::Acquire)
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        while !*open {
            open = self.wake.wait(open).unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct SinkTestOptions {
    pub fail_start: bool,
    pub gate: Option<std::sync::Arc<SinkGate>>,
    pub close_gate: Option<std::sync::Arc<SinkGate>>,
    pub fail_after: Option<u64>,
    pub max_write: Option<usize>,
    pub interrupt_once: bool,
    pub zero_once: bool,
    pub fail_flush: bool,
    pub fail_close: bool,
}
