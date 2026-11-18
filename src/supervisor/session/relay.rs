use super::{CancelToken, DeliveryIncomplete, RelayState, RelayStatistics};
use crate::supervisor::{ParentReader, ReadResult};
use std::collections::VecDeque;
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::net::unix::pipe::Sender;

/// Coalesced progress; byte accounting remains owned by the direction's driver.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Progress {
    pub exceeded: bool,
    pub done: bool,
    pub error: Option<String>,
    pub state: RelayState,
}

/// Each direction publishes independently, without waking the coordinator.
#[derive(Debug, Default)]
pub(super) struct Counters {
    version: AtomicU64,
    read: AtomicU64,
    forwarded: AtomicU64,
    queued: AtomicU64,
    pub last_forward_ns: AtomicU64,
}
impl Counters {
    fn publish(&self, stats: &RelayStatistics) {
        self.version.fetch_add(1, Ordering::AcqRel);
        self.read.store(stats.bytes_read, Ordering::Relaxed);
        self.forwarded
            .store(stats.bytes_forwarded, Ordering::Relaxed);
        self.queued
            .store(stats.bytes_queued_at_end, Ordering::Relaxed);
        self.version.fetch_add(1, Ordering::Release);
    }
    pub fn snapshot(&self) -> super::DialogueSnapshot {
        loop {
            let version = self.version.load(Ordering::Acquire);
            if version % 2 != 0 {
                continue;
            }
            let snapshot = super::DialogueSnapshot {
                bytes_read: self.read.load(Ordering::Acquire),
                bytes_forwarded: self.forwarded.load(Ordering::Acquire),
                bytes_queued: self.queued.load(Ordering::Acquire),
            };
            if self.version.load(Ordering::Acquire) == version {
                return snapshot;
            }
        }
    }
}

/// Each instance owns all its endpoints and state. No cross-direction locks.
pub(super) struct Relay {
    pub counters: Arc<Counters>,
    pub transcript: Option<super::transcript::Direction>,
    pub origin: Instant,
    source: Option<ParentReader>,
    destination: Option<Sender>,
    queue: VecDeque<Box<[u8]>>,
    prefix: usize,
    buffer: Box<[u8]>,
    cap: u64,
    bytes: usize,
    records: usize,
    pub stats: RelayStatistics,
    pub exceeded: bool,
    pub error: Option<String>,
}
impl Relay {
    pub fn new(
        source: ParentReader,
        destination: Sender,
        cap: u64,
        bytes: usize,
        records: usize,
    ) -> Self {
        Self {
            counters: Arc::new(Counters::default()),
            transcript: None,
            origin: Instant::now(),
            source: Some(source),
            destination: Some(destination),
            queue: VecDeque::new(),
            prefix: 0,
            buffer: vec![0; 64 * 1024].into_boxed_slice(),
            cap,
            bytes,
            records,
            stats: RelayStatistics {
                transitions: vec![RelayState::Open],
                ..RelayStatistics::default()
            },
            exceeded: false,
            error: None,
        }
    }
    /// This future stays alive while the coordinator waits on unrelated events.
    /// The session owns an abort-on-drop task guard for these endpoints.
    pub async fn run(
        mut self,
        progress: tokio::sync::watch::Sender<Progress>,
        stop: &CancelToken,
        #[cfg(debug_assertions)] start_gate: Option<&CancelToken>,
    ) -> RelayStatistics {
        #[cfg(debug_assertions)]
        if let Some(gate) = start_gate {
            tokio::select! {
                _ = gate.cancelled() => {},
                _ = stop.cancelled() => return self.finish(),
            }
        }
        let mut last_progress = Progress::default();
        let mut operations = 0;
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                _ = self.step() => {},
            }
            let update = Progress {
                exceeded: self.exceeded,
                done: self.done(),
                error: self.error.clone(),
                state: self.stats.final_state,
            };
            self.counters.publish(&self.stats);
            if update != last_progress {
                last_progress = update.clone();
                progress.send_replace(update);
            }
            // A continuously ready pipe must let the coordinator observe caps
            // within one bounded batch, without a reschedule for every chunk.
            operations += 1;
            if operations == 64 {
                operations = 0;
                tokio::task::yield_now().await;
            }
        }
        self.finish()
    }
    pub fn done(&self) -> bool {
        self.stats.final_state != RelayState::Open
    }
    pub fn finish(mut self) -> RelayStatistics {
        self.transition(RelayState::Closed);
        self.stats.delivery_incomplete = DeliveryIncomplete {
            bytes: self.stats.bytes_read
                - self.stats.bytes_forwarded
                - self.stats.discarded_over_cap,
            unread_pipe_contents: if self.stats.source_eof {
                super::UnreadPipeContents::KnownZero
            } else {
                super::UnreadPipeContents::Unknown
            },
        };
        self.counters.publish(&self.stats);
        if let Some(transcript) = self.transcript.take() {
            transcript.finish(self.stats.bytes_read, self.stats.bytes_forwarded);
        }
        self.stats
    }
    pub async fn step(&mut self) {
        if self.done() {
            std::future::pending::<()>().await;
        }
        if self.source.is_none() && self.queue.is_empty() {
            self.destination.take();
            self.transition(if self.stats.source_eof {
                RelayState::HalfClosed
            } else {
                RelayState::Closed
            });
            return;
        }
        let available = self.bytes - self.stats.bytes_queued_at_end as usize;
        // Beyond the cap no queue space is needed: keep consuming through EOF.
        let remaining = self.cap.saturating_sub(self.stats.bytes_read);
        let can_read = self.source.is_some()
            && (remaining == 0 || (available > 0 && self.queue.len() < self.records));
        let length = if remaining == 0 && !self.exceeded {
            1
        } else if remaining == 0 {
            self.buffer.len()
        } else {
            available
                .min(self.buffer.len())
                .min(remaining.min(self.buffer.len() as u64) as usize)
        };
        enum Progress {
            Read(ReadResult),
            Write(io::Result<usize>),
        }
        let progress = tokio::select! { biased;
            result = async {
                let front = self.queue.front().expect("write enabled");
                self.destination.as_mut().expect("live destination").write(&front[self.prefix..]).await
            }, if !self.queue.is_empty() => Progress::Write(result),
            result = async { self.source.as_mut().expect("read enabled").read_chunk(&mut self.buffer[..length]).await }, if can_read => Progress::Read(result),
            else => { std::future::pending::<()>().await; unreachable!() }
        };
        match progress {
            Progress::Read(ReadResult::Data(n)) => {
                let forwarded = n.min(remaining.min(n as u64) as usize);
                if let Some(transcript) = &mut self.transcript {
                    transcript.record(
                        "chunk",
                        self.stats.bytes_read,
                        n as u64,
                        None,
                        &self.buffer[..n],
                    );
                }
                self.stats.bytes_read += n as u64;
                self.stats.discarded_over_cap += (n - forwarded) as u64;
                if forwarded > 0 {
                    self.stats.bytes_queued_at_end += forwarded as u64;
                    self.queue.push_back(self.buffer[..forwarded].into());
                }
                self.stats.peak_queued_bytes = self
                    .stats
                    .peak_queued_bytes
                    .max(self.stats.bytes_queued_at_end);
                self.stats.peak_queued_records =
                    self.stats.peak_queued_records.max(self.queue.len());
                if self.stats.bytes_read > self.cap {
                    self.exceeded = true;
                }
                // Do not spend a reactor round trip discovering that a pipe
                // we just read into has a writable destination.
                self.try_forward();
            }
            Progress::Read(ReadResult::Eof) => {
                self.event("eof", self.stats.bytes_read, 0, None);
                self.stats.source_eof = true;
                self.source.take();
            }
            Progress::Read(ReadResult::Failed(error)) => {
                self.error = Some(error.to_string());
                self.source.take();
            }
            Progress::Write(result) => self.wrote(result),
        }
    }
    fn try_forward(&mut self) {
        let Some(front) = self.queue.front() else {
            return;
        };
        let result = self
            .destination
            .as_ref()
            .expect("live destination")
            .try_write(&front[self.prefix..]);
        self.wrote(result);
    }
    fn wrote(&mut self, result: io::Result<usize>) {
        match result {
            Ok(0) => self.failed(io::Error::new(
                io::ErrorKind::WriteZero,
                "relay write returned zero",
            )),
            Ok(n) => {
                self.event("forwarded", self.stats.bytes_forwarded, n as u64, None);
                self.counters.last_forward_ns.store(
                    self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                    Ordering::Release,
                );
                self.stats.bytes_forwarded += n as u64;
                self.stats.bytes_queued_at_end -= n as u64;
                self.prefix += n;
                if self.prefix == self.queue.front().unwrap().len() {
                    self.queue.pop_front();
                    self.prefix = 0;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => self.failed(error),
        }
    }
    fn event(&mut self, kind: &'static str, offset: u64, length: u64, error: Option<String>) {
        if let Some(transcript) = &mut self.transcript {
            transcript.record(kind, offset, length, error, &[]);
        }
    }
    fn transition(&mut self, state: RelayState) {
        if self.stats.final_state != state {
            if state == RelayState::HalfClosed {
                self.event("half_close", self.stats.bytes_forwarded, 0, None);
            }
            self.stats.final_state = state;
            self.stats.transitions.push(state);
            debug_assert!(self.stats.transitions.len() <= 3);
        }
    }
    fn failed(&mut self, error: io::Error) {
        self.event(
            "write_failure",
            self.stats.bytes_forwarded,
            0,
            Some(error.to_string()),
        );
        if error.kind() != io::ErrorKind::BrokenPipe {
            self.error = Some(error.to_string());
        }
        self.transition(RelayState::SinkGone);
        // Never drain-and-discard: closing this read end delivers EPIPE upstream.
        self.source.take();
        self.destination.take();
    }
}
