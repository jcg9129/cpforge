//! Run-level, byte-bounded, lossy output transport.

use super::Stream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::{Semaphore, mpsc};

pub const EVENT_BUDGET: usize = 8 * 1024 * 1024;
pub const EVENT_CHUNK: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub struct OutputChunk {
    pub stream: Stream,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
struct Envelope {
    chunk: OutputChunk,
    _permit: tokio::sync::OwnedSemaphorePermit,
    accounting: Arc<Accounting>,
    charge: usize,
}

impl Drop for Envelope {
    fn drop(&mut self) {
        self.accounting
            .queued
            .fetch_sub(self.charge, Ordering::AcqRel);
    }
}

#[derive(Debug, Default)]
struct Accounting {
    dropped: AtomicU64,
    queued: AtomicUsize,
}

#[derive(Clone, Debug)]
pub struct EventSender {
    sender: mpsc::UnboundedSender<Envelope>,
    space: Arc<Semaphore>,
    accounting: Arc<Accounting>,
    forward: Option<ForwardOutput>,
}

type OutputCallback = dyn Fn(Stream, &[u8]) + Send + Sync;

#[derive(Clone)]
struct ForwardOutput(Arc<OutputCallback>);
impl std::fmt::Debug for ForwardOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ForwardOutput")
    }
}

#[derive(Debug)]
pub struct EventReceiver(mpsc::UnboundedReceiver<Envelope>);

impl EventSender {
    pub fn channel() -> (Self, EventReceiver) {
        Self::with_budget(EVENT_BUDGET)
    }

    /// Includes each fresh backing buffer and conservative per-message overhead.
    pub fn with_budget(bytes: usize) -> (Self, EventReceiver) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender,
                space: Arc::new(Semaphore::new(bytes.min(Semaphore::MAX_PERMITS))),
                accounting: Arc::default(),
                forward: None,
            },
            EventReceiver(receiver),
        )
    }

    /// Forward directly to a job-wide transport, which performs byte accounting
    /// and assigns sequence numbers atomically. No intermediate drops occur.
    pub fn forwarding(callback: impl Fn(Stream, &[u8]) + Send + Sync + 'static) -> Self {
        let (mut sender, _) = Self::with_budget(0);
        sender.forward = Some(ForwardOutput(Arc::new(callback)));
        sender
    }

    pub fn dropped(&self) -> u64 {
        self.accounting.dropped.load(Ordering::Acquire)
    }

    pub fn queued_bytes(&self) -> usize {
        self.accounting.queued.load(Ordering::Acquire)
    }

    /// Never waits, including for a consumer. Disconnection is a no-op.
    pub fn offer(&self, stream: Stream, bytes: &[u8]) {
        if let Some(forward) = &self.forward {
            (forward.0)(stream, bytes);
            return;
        }
        for bytes in bytes.chunks(EVENT_CHUNK) {
            if self.sender.is_closed() {
                return;
            }
            // Covers Envelope, allocator/channel bookkeeping and the backing Vec.
            let charge = bytes.len() + 256;
            let Ok(permit) = self.space.clone().try_acquire_many_owned(charge as u32) else {
                self.accounting.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            self.accounting.queued.fetch_add(charge, Ordering::AcqRel);
            let _ = self.sender.send(Envelope {
                chunk: OutputChunk {
                    stream,
                    bytes: bytes.to_vec(),
                },
                _permit: permit,
                accounting: self.accounting.clone(),
                charge,
            });
        }
    }
}

impl EventReceiver {
    pub async fn recv(&mut self) -> Option<OutputChunk> {
        self.0.recv().await.map(|mut envelope| OutputChunk {
            stream: envelope.chunk.stream,
            bytes: std::mem::take(&mut envelope.chunk.bytes),
        })
    }

    pub fn try_recv(&mut self) -> Result<OutputChunk, mpsc::error::TryRecvError> {
        self.0.try_recv().map(|mut envelope| OutputChunk {
            stream: envelope.chunk.stream,
            bytes: std::mem::take(&mut envelope.chunk.bytes),
        })
    }
}
