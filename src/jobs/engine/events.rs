//! One serialization-sized allowance per event, including its envelope. The
//! 7 KiB bound is report §3.7; output is split into fresh 4 KiB buffers.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

use super::{FinalizedReport, Role, RunId, RunSummary, Stage};
use crate::report::{JobKind, JobVerdict};
use crate::storage::job::JobId;
use crate::supervisor::Stream;

pub const EVENT_BUDGET: usize = 8 * 1024 * 1024;
pub const EVENT_BOUND: usize = 7 * 1024;

#[derive(Clone, Debug)]
pub enum EventPayload {
    JobStarted {
        job: JobId,
        kind: JobKind,
    },
    RunStarted {
        run: RunId,
        execution: u32,
        stage: Stage,
        role: Role,
        digest: Option<[u8; 32]>,
    },
    OutputChunk {
        run: RunId,
        stream: Stream,
        bytes: Vec<u8>,
    },
    RunFinished {
        run: RunId,
        summary: Arc<RunSummary>,
    },
    Progress {
        done: u32,
        total: u32,
    },
}

#[derive(Clone, Debug)]
pub struct Event {
    pub seq: u64,
    pub payload: EventPayload,
}

#[derive(Clone, Debug)]
pub struct JobFinished {
    pub seq: u64,
    pub job: JobId,
    pub verdict: JobVerdict,
    /// Set only by a later persistence service after observing publication.
    pub report_id: Option<JobId>,
    pub dropped: u64,
    pub report: Arc<FinalizedReport>,
}

#[derive(Debug)]
struct State {
    queue: VecDeque<Event>,
    next_seq: u64,
    dropped: u64,
    budget: usize,
    connected: bool,
    subscribed: bool,
    closed: bool,
    terminal: Option<JobFinished>,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    wake: Notify,
}

/// Cloneable producer. Producers may outlive E; attempts after closure are
/// ignored, so transferred cleanup can never change the frozen drop count.
#[derive(Clone, Debug)]
pub struct Events(Arc<Shared>);

#[derive(Debug)]
pub struct EventReceiver(Arc<Shared>);

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

impl Events {
    pub fn new() -> Self {
        Self::with_budget(EVENT_BUDGET)
    }

    pub(super) fn with_budget(budget: usize) -> Self {
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                next_seq: 0,
                dropped: 0,
                budget: budget.min(EVENT_BUDGET),
                connected: false,
                subscribed: false,
                closed: false,
                terminal: None,
            }),
            wake: Notify::new(),
        }))
    }

    /// Subscribe before running the engine. A disconnected consumer cannot be
    /// replaced, since the gap equation applies to one continuous connection.
    pub fn subscribe(&self) -> Option<EventReceiver> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.subscribed || state.closed || state.next_seq != 0 {
            return None;
        }
        state.subscribed = true;
        state.connected = true;
        Some(EventReceiver(self.0.clone()))
    }

    pub(super) fn offer(&self, payload: EventPayload) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        let seq = state.next_seq;
        state.next_seq += 1;
        if !state.connected {
            return;
        }
        if state.queue.len() >= state.budget / EVENT_BOUND {
            state.dropped += 1;
            return;
        }
        state.queue.push_back(Event { seq, payload });
        drop(state);
        self.0.wake.notify_one();
    }

    pub(super) fn close(&self) -> u64 {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        let dropped = state.dropped;
        drop(state);
        self.0.wake.notify_waiters();
        dropped
    }

    pub(super) fn finish(&self, report: Arc<FinalizedReport>, report_id: Option<JobId>) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(state.closed && state.terminal.is_none());
        let seq = state.next_seq;
        state.next_seq += 1;
        if state.connected {
            state.terminal = Some(JobFinished {
                seq,
                job: report.data().job.id.clone(),
                verdict: report.data().result.verdict,
                report_id,
                dropped: state.dropped,
                report,
            });
        }
        drop(state);
        self.0.wake.notify_waiters();
    }
}

impl EventReceiver {
    /// Drain until None (E), then await `terminal`. Sequence numbers are not a
    /// closure barrier and can contain gaps when the consumer falls behind.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            let ready = self.0.wake.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(event) = state.queue.pop_front() {
                    return Some(event);
                }
                if state.closed {
                    return None;
                }
            }
            ready.await;
        }
    }

    /// Returns the reserved slot only after all non-terminal events drained.
    pub async fn terminal(&mut self) -> JobFinished {
        loop {
            let ready = self.0.wake.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                assert!(
                    state.closed && state.queue.is_empty(),
                    "drain before terminal"
                );
                if let Some(terminal) = state.terminal.take() {
                    return terminal;
                }
            }
            ready.await;
        }
    }
}

/// Executor output enters the job queue directly, without a second lossy queue
/// that could consume bytes before assigning the job-wide sequence number.
#[derive(Clone, Debug)]
pub struct RunEvents {
    pub(super) events: Events,
    pub(super) run: RunId,
    pub(super) output: bool,
}
impl RunEvents {
    pub fn output(&self, stream: Stream, bytes: &[u8]) {
        if self.output {
            for bytes in bytes.chunks(4096) {
                self.events.offer(EventPayload::OutputChunk {
                    run: self.run.clone(),
                    stream,
                    bytes: bytes.to_vec(),
                });
            }
        }
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.connected = false;
        state.queue.clear();
        state.terminal = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_event_attempts_have_one_sequence_and_disconnection_is_not_a_drop() {
        let events = Events::with_budget(EVENT_BOUND * 2);
        let receiver = events.subscribe().unwrap();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let events = events.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        events.offer(EventPayload::Progress {
                            done: i,
                            total: 100,
                        });
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let state = events.0.state.lock().unwrap();
        assert_eq!(state.next_seq, 400);
        assert_eq!(state.dropped, 398);
        assert_eq!(
            state.queue.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [0, 1]
        );
        drop(state);
        drop(receiver);
        events.offer(EventPayload::Progress { done: 1, total: 1 });
        let state = events.0.state.lock().unwrap();
        assert_eq!(state.next_seq, 401);
        assert_eq!(state.dropped, 398);
        assert!(state.queue.is_empty());
    }
}
