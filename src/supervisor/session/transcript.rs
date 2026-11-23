//! Independent bounded transcript queues. No forwarding operation waits on disk.
use super::*;
use crate::supervisor::{global_bounds, sys};
use serde::Serialize;
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::Instant;
use tokio::sync::oneshot;

pub(super) const BYTE_CAP: u64 = 1024 * 1024;
pub(super) const RECORD_CAP: u64 = 65_536;
const GAP_CAP: usize = 64;

#[derive(Clone, Debug)]
struct Failure {
    error: String,
    at_offset: u64,
}
#[derive(Serialize)]
struct Entry {
    kind: &'static str,
    record: u64,
    offset: u64,
    length: u64,
    time_ns: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
struct Message {
    entry: Entry,
    bytes: Box<[u8]>,
}
struct Final {
    stats: TranscriptStatistics,
    read: u64,
    forwarded: u64,
    time_ns: u64,
}
pub(super) struct Direction {
    queue: mpsc::SyncSender<Message>,
    final_record: Arc<OnceLock<Final>>,
    failure: Arc<OnceLock<Failure>>,
    stats: TranscriptStatistics,
    sequence: u64,
    dropping: bool,
    origin: Instant,
}
impl Direction {
    pub fn record(
        &mut self,
        kind: &'static str,
        offset: u64,
        length: u64,
        error: Option<String>,
        bytes: &[u8],
    ) {
        let sequence = self.sequence;
        self.sequence = self.sequence.saturating_add(1);
        if self.failure.get().is_none()
            && self.stats.retained_records < RECORD_CAP - 1
            && self.stats.retained_bytes + bytes.len() as u64 <= BYTE_CAP
        {
            let message = Message {
                entry: Entry {
                    kind,
                    record: sequence,
                    offset,
                    length,
                    time_ns: nanos(self.origin),
                    file_offset: (kind == "chunk").then_some(self.stats.retained_bytes),
                    error,
                },
                bytes: bytes.into(),
            };
            if self.queue.try_send(message).is_ok() {
                self.stats.retained_records += 1;
                self.stats.retained_bytes += bytes.len() as u64;
                self.dropping = false;
                return;
            }
        }
        let gap = TranscriptGap {
            first_record: sequence,
            last_record: sequence,
            start_offset: offset,
            end_offset: offset.saturating_add(length),
            records: 1,
            ranges: 1,
        };
        if self.dropping {
            if let Some(last) = self
                .stats
                .further_gaps
                .as_mut()
                .or_else(|| self.stats.gaps.last_mut())
            {
                merge(last, &gap);
            }
        } else if self.stats.gaps.len() < GAP_CAP {
            self.stats.gaps.push(gap);
        } else if let Some(last) = &mut self.stats.further_gaps {
            last.ranges = last.ranges.saturating_add(1);
            merge(last, &gap);
        } else {
            self.stats.further_gaps = Some(gap);
        }
        self.dropping = true;
    }
    pub fn finish(mut self, read: u64, forwarded: u64) {
        self.stats.retained_records += 1; // reserved final record, even at saturation
        let _ = self.final_record.set(Final {
            stats: self.stats,
            read,
            forwarded,
            time_ns: nanos(self.origin),
        });
    }
}
fn merge(target: &mut TranscriptGap, next: &TranscriptGap) {
    target.last_record = next.last_record;
    target.start_offset = target.start_offset.min(next.start_offset);
    target.end_offset = target.end_offset.max(next.end_offset);
    target.records = target.records.saturating_add(next.records);
}
fn nanos(origin: Instant) -> u64 {
    origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
}
#[derive(Default)]
struct Shutdown {
    cancelled: Mutex<bool>,
    wake: Condvar,
}
impl Shutdown {
    fn cancel(&self) {
        *self.cancelled.lock().unwrap() = true;
        self.wake.notify_all();
    }
    fn cancelled(&self) -> bool {
        *self.cancelled.lock().unwrap()
    }
    #[cfg(debug_assertions)]
    fn delay(&self, duration: Duration) {
        let _guard = self
            .wake
            .wait_timeout_while(self.cancelled.lock().unwrap(), duration, |cancelled| {
                !*cancelled
            })
            .unwrap();
    }
}
struct Handle {
    done: oneshot::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
    // Handle::drop joins before this field returns capacity to admission.
    _credit: crate::supervisor::Credits,
    failure: Arc<OnceLock<Failure>>,
    final_record: Arc<OnceLock<Final>>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
struct Worker {
    #[cfg(all(
        feature = "storage-test-hooks",
        any(target_os = "linux", target_os = "macos")
    ))]
    _owned: crate::supervisor::test_hooks::OwnedResource,
    file: sys::TranscriptFile,
    receiver: mpsc::Receiver<Message>,
    final_record: Arc<OnceLock<Final>>,
    failure: Arc<OnceLock<Failure>>,
    events: Option<(mpsc::Receiver<Vec<u8>>, sys::TranscriptEvents)>,
    shutdown: Arc<Shutdown>,
    test: TranscriptTestOptions,
}
impl Worker {
    fn fail(&self, error: impl ToString, at_offset: u64) {
        let _ = self.failure.set(Failure {
            error: error.to_string(),
            at_offset,
        });
    }
    fn write_events(&mut self) {
        if let Some((receiver, file)) = &mut self.events {
            for bytes in receiver.try_iter() {
                if let Err(error) = file.write(&bytes) {
                    let _ = self.failure.set(Failure {
                        error: error.to_string(),
                        at_offset: self.final_record.get().map_or(0, |f| f.read),
                    });
                }
            }
        }
    }
    fn run(mut self) {
        #[cfg(debug_assertions)]
        self.shutdown.delay(self.test.writer_delay);
        while !self.shutdown.cancelled() {
            self.write_events();
            let message = match self.receiver.recv_timeout(Duration::from_millis(2)) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if self.failure.get().is_some() {
                continue;
            }
            let result = (|| {
                #[cfg(debug_assertions)]
                if self.test.fail_after_bytes.is_some_and(|limit| {
                    message.entry.kind == "chunk"
                        && message.entry.offset + message.entry.length > limit
                }) {
                    return Err(std::io::Error::other("injected transcript write failure"));
                }
                self.file
                    .write(&message.bytes, &serde_json::to_vec(&message.entry)?)
            })();
            if let Err(error) = result {
                self.fail(error, message.entry.offset);
            }
        }
        if self.shutdown.cancelled() {
            return; // Discard the queue; never wait out an injected writer delay.
        }
        self.write_events();
        if let Some(final_record) = self
            .final_record
            .get()
            .filter(|_| self.failure.get().is_none())
        {
            if let Err(error) = write_final(&mut self.file, final_record) {
                self.fail(error, final_record.read);
            }
        }
        #[cfg(not(debug_assertions))]
        let _ = self.test;
    }
}
fn write_final(file: &mut sys::TranscriptFile, final_record: &Final) -> std::io::Result<()> {
    for gap in &final_record.stats.gaps {
        file.write(
            &[],
            &serde_json::to_vec(&serde_json::json!({"kind": "gap", "gap": gap}))?,
        )?;
    }
    if let Some(gap) = &final_record.stats.further_gaps {
        file.write(
            &[],
            &serde_json::to_vec(&serde_json::json!({"kind": "further_gaps", "gap": gap}))?,
        )?;
    }
    file.write(
        &[],
        &serde_json::to_vec(&serde_json::json!({
            "kind": "final", "read_offset": final_record.read,
            "forwarded_offset": final_record.forwarded, "time_ns": final_record.time_ns
        }))?,
    )?;
    file.finish()
}
pub(super) struct Writer {
    handles: [Option<Handle>; 2],
    events: Option<mpsc::SyncSender<Vec<u8>>>,
    event_cursor: usize,
    origin: Instant,
    shutdown: Arc<Shutdown>,
    initial_failure: Option<Failure>,
}
impl Drop for Writer {
    fn drop(&mut self) {
        // Wake delayed workers before Handle::drop joins either thread. This
        // also covers cancellation by dropping the owning session future.
        self.shutdown.cancel();
    }
}
impl Writer {
    pub fn start(
        files: sys::TranscriptFiles,
        origin: Instant,
        test: &TranscriptTestOptions,
    ) -> (Self, [Option<Direction>; 2]) {
        let mut writer = Self {
            handles: std::array::from_fn(|_| None),
            events: None,
            event_cursor: 0,
            origin,
            shutdown: Arc::new(Shutdown::default()),
            initial_failure: None,
        };
        let mut directions = [None, None];
        let Some(credits) = global_bounds().and_then(|b| b.thread_credits.try_reserve(2)) else {
            writer.initial_failure = Some(Failure {
                error: "transcript worker capacity exhausted".into(),
                at_offset: 0,
            });
            return (writer, directions);
        };
        let mut credits = credits.split().into_iter();
        let (events_tx, events_rx) = mpsc::sync_channel(32);
        writer.events = Some(events_tx);
        let mut events = Some((events_rx, files.events));
        for (index, (name, file)) in ["s2i", "i2s"].into_iter().zip(files.directions).enumerate() {
            #[cfg(debug_assertions)]
            let capacity = test.queue_records.unwrap_or(128).clamp(1, 4096);
            #[cfg(not(debug_assertions))]
            let capacity = 128;
            let (queue, receiver) = mpsc::sync_channel::<Message>(capacity);
            let (done_send, done) = oneshot::channel();
            let failure = Arc::new(OnceLock::new());
            let final_record = Arc::new(OnceLock::new());
            let worker = Worker {
                #[cfg(all(
                    feature = "storage-test-hooks",
                    any(target_os = "linux", target_os = "macos")
                ))]
                _owned: crate::supervisor::test_hooks::OwnedResource::new(),
                file,
                receiver,
                final_record: final_record.clone(),
                failure: failure.clone(),
                events: if index == 0 { events.take() } else { None },
                shutdown: writer.shutdown.clone(),
                test: test.clone(),
            };
            let credit = credits.next().unwrap();
            let started = std::thread::Builder::new()
                .name(format!("cpf-transcript-{name}"))
                .spawn(move || {
                    worker.run();
                    let _ = done_send.send(());
                });
            match started {
                Ok(thread) => {
                    writer.handles[index] = Some(Handle {
                        done,
                        thread: Some(thread),
                        _credit: credit,
                        failure: failure.clone(),
                        final_record: final_record.clone(),
                    });
                    directions[index] = Some(Direction {
                        queue,
                        final_record,
                        failure,
                        stats: TranscriptStatistics::default(),
                        sequence: 0,
                        dropping: false,
                        origin,
                    });
                }
                Err(error) => {
                    writer.initial_failure = Some(Failure {
                        error: error.to_string(),
                        at_offset: 0,
                    })
                }
            }
        }
        (writer, directions)
    }
    pub fn record_events(&mut self, record: &SessionRecord) {
        let bytes = record.transcript_events(self.origin, &mut self.event_cursor);
        if !bytes.is_empty()
            && self
                .events
                .as_ref()
                .is_some_and(|sender| sender.try_send(bytes).is_err())
        {
            self.initial_failure.get_or_insert_with(|| Failure {
                error: "transcript process-event queue unavailable".into(),
                at_offset: 0,
            });
        }
    }
    pub async fn finish(mut self, deadline: Instant) -> TranscriptOutcome {
        self.events.take();
        let mut outcome = TranscriptOutcome {
            status: TranscriptStatus::Complete,
            excerpt: false,
            directions: std::array::from_fn(|_| TranscriptStatistics::default()),
        };
        let mut failure = self.initial_failure.take();
        for (index, handle) in self.handles.iter_mut().enumerate() {
            let Some(handle) = handle else {
                continue;
            };
            if !matches!(
                tokio::time::timeout_at(deadline.into(), &mut handle.done).await,
                Ok(Ok(()))
            ) {
                self.shutdown.cancel();
                let _ = handle.failure.set(Failure {
                    error: "transcript writer did not finish within its finalization deadline"
                        .into(),
                    at_offset: handle.final_record.get().map_or(0, |f| f.read),
                });
            }
            if let Some(final_record) = handle.final_record.get() {
                outcome.directions[index] = final_record.stats.clone();
            }
            failure = failure.or_else(|| handle.failure.get().cloned());
        }
        outcome.excerpt = outcome
            .directions
            .iter()
            .any(|d| !d.gaps.is_empty() || d.further_gaps.is_some());
        if let Some(failure) = failure {
            outcome.excerpt = true;
            outcome.status = TranscriptStatus::Failed {
                error: BoundedText::new(&failure.error),
                at_offset: failure.at_offset,
            };
        } else if outcome.excerpt {
            outcome.status = TranscriptStatus::Gapped;
        }
        outcome
    }
}
