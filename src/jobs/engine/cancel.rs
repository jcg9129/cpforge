use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::report::{InternalFailure, JobVerdict};
use crate::supervisor::CancelToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelSource {
    Console,
    Terminate,
    Api,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelPhase {
    Primary,
    Fact,
    Hooks,
    Settlement,
    Closure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancellation {
    pub requested_at: Duration,
    pub source: CancelSource,
    pub phase: CancelPhase,
}

#[derive(Debug)]
pub(super) struct VerdictState {
    pub intent: Option<Cancellation>,
    pub phase: CancelPhase,
    pub frozen: bool,
    pub primary: Option<JobVerdict>,
    pub internal: Option<InternalFailure>,
    first_notification: Option<Instant>,
    force_exit: bool,
    pub next_run: u32,
}

/// The verdict lock arbitrates run/check admission, derivation, V and F with
/// cancellation. Notifications after F affect only the force-exit state.
#[derive(Clone, Debug)]
pub struct CancelState {
    pub(super) state: Arc<Mutex<VerdictState>>,
    token: CancelToken,
    pub(super) start: Instant,
}

impl Default for CancelState {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelState {
    pub fn new() -> Self {
        Self::at(Instant::now())
    }

    pub(super) fn at(start: Instant) -> Self {
        Self {
            state: Arc::new(Mutex::new(VerdictState {
                intent: None,
                phase: CancelPhase::Primary,
                frozen: false,
                primary: None,
                internal: None,
                first_notification: None,
                force_exit: false,
                next_run: 0,
            })),
            token: CancelToken::new(),
            start,
        }
    }

    /// The first request wins. A subsequent notification at least 100 ms later
    /// requests force exit, which the caller implements through the supervisor.
    pub fn request_cancel(&self, source: CancelSource) {
        self.request_at(source, Instant::now());
    }

    pub(super) fn request_at(&self, source: CancelSource, now: Instant) {
        let mut state = self.lock();
        if let Some(first) = state.first_notification {
            if now.saturating_duration_since(first) >= Duration::from_millis(100) {
                state.force_exit = true;
            }
        } else {
            state.first_notification = Some(now);
            if !state.frozen {
                state.intent = Some(Cancellation {
                    requested_at: now.saturating_duration_since(self.start),
                    source,
                    phase: state.phase,
                });
                // Keep token publication ordered with intent and admission.
                self.token.cancel();
            }
        }
    }

    pub fn force_exit_requested(&self) -> bool {
        self.lock().force_exit
    }

    pub fn cancellation(&self) -> Option<Cancellation> {
        self.lock().intent
    }

    pub(super) fn token(&self) -> CancelToken {
        self.token.clone()
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, VerdictState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_debounce_and_first_source_survive_finalization() {
        let cancel = CancelState::new();
        let now = cancel.start;
        cancel.request_at(CancelSource::Console, now);
        cancel.request_at(CancelSource::Terminate, now + Duration::from_millis(99));
        assert!(!cancel.force_exit_requested());
        cancel.lock().frozen = true;
        cancel.request_at(CancelSource::Api, now + Duration::from_millis(100));
        assert!(cancel.force_exit_requested());
        assert_eq!(cancel.cancellation().unwrap().source, CancelSource::Console);
    }

    #[test]
    fn intent_after_f_is_not_recorded_but_notifications_still_force_exit() {
        let cancel = CancelState::new();
        cancel.lock().frozen = true;
        cancel.request_at(CancelSource::Api, cancel.start);
        assert!(cancel.cancellation().is_none());
        assert!(!cancel.token().is_cancelled());
        cancel.request_at(CancelSource::Api, cancel.start + Duration::from_millis(100));
        assert!(cancel.force_exit_requested());
    }
}
