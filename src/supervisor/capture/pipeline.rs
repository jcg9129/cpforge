use super::*;
use crate::supervisor::{
    self, Admission, Credits, EventSender, NotStarted, OutputSpec, ParentReader, ReadResult,
    RootOutcome, SpawnOptions, StopTrigger, Violation, sys,
};
use std::io;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc, watch};

const QUEUE_BYTES: usize = 1024 * 1024;
const READ_BYTES: usize = 64 * 1024;
pub(in crate::supervisor) const DRAIN: Duration = Duration::from_millis(500);
const FINALISE: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct State {
    record: StreamCapture,
    reader_stopped: bool,
    worker_done: bool,
    failed: bool,
    frozen: bool,
    events: Option<EventSender>,
}

impl State {
    fn loss(&mut self, loss: CaptureLoss) {
        if !self.frozen
            && self.record.losses.len() < 8
            && !self
                .record
                .losses
                .iter()
                .any(|old| std::mem::discriminant(old) == std::mem::discriminant(&loss))
        {
            self.record.losses.push(loss);
        }
    }
}

type Shared = Arc<Mutex<State>>;

#[derive(Debug)]
struct Queued {
    bytes: Vec<u8>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Runtime shutdown also stops admission and closes the worker's queue.
struct ReaderGuard(Shared);
impl Drop for ReaderGuard {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.reader_stopped = true;
        if !state.record.eof
            && !state
                .record
                .losses
                .iter()
                .any(|loss| matches!(loss, CaptureLoss::PipeRead(_)))
        {
            state.loss(CaptureLoss::NoEofBeforeDrainDeadline);
        }
    }
}

#[derive(Debug)]
struct PendingReader {
    pipe: ParentReader,
    sender: mpsc::UnboundedSender<Queued>,
    space: Arc<Semaphore>,
    deadline: watch::Receiver<Option<Instant>>,
    budget: watch::Sender<Option<Stream>>,
}

#[derive(Debug)]
struct Capture {
    stream: Stream,
    policy: StreamPolicy,
    state: Shared,
    drain: watch::Sender<Option<Instant>>,
    reader: Option<tokio::task::JoinHandle<()>>,
    pending: Option<PendingReader>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug)]
pub(in crate::supervisor) struct Captures {
    streams: Vec<Capture>,
    budget: watch::Receiver<Option<Stream>>,
    _budget_sender: watch::Sender<Option<Stream>>,
    drain_start: Option<Instant>,
    transfer_cutoff: Option<Instant>,
    admission: Option<Admission>,
    activate: watch::Sender<bool>,
}

impl Captures {
    /// Reserve every worker credit before creating any worker or child.
    pub(in crate::supervisor) fn prepare(
        stdout: &mut OutputSpec,
        stderr: &mut OutputSpec,
        opts: &SpawnOptions,
        mut admission: Admission,
    ) -> Result<Self, NotStarted> {
        let count = [&*stdout, &*stderr]
            .iter()
            .filter(|spec| {
                matches!(
                    spec,
                    OutputSpec::Capture(StreamPolicy {
                        sink: Sink::File(_),
                        ..
                    })
                )
            })
            .count();
        let mut credits = admission.worker_credits(count)?;
        let (budget_tx, budget) = watch::channel(None);
        let (activate, _) = watch::channel(false);
        let mut captures = Self {
            streams: Vec::with_capacity(2),
            budget,
            _budget_sender: budget_tx.clone(),
            drain_start: None,
            transfer_cutoff: None,
            admission: Some(admission),
            activate,
        };
        for (stream, output, test) in [
            (Stream::Stdout, stdout, &opts.stdout_sink),
            (Stream::Stderr, stderr, &opts.stderr_sink),
        ] {
            if !matches!(output, OutputSpec::Capture(_)) {
                continue;
            }
            let OutputSpec::Capture(policy) = std::mem::replace(output, OutputSpec::Null) else {
                unreachable!()
            };
            let pipe = supervisor::stdout_pipe().map_err(NotStarted::Setup)?;
            let state = Arc::new(Mutex::new(State {
                record: StreamCapture {
                    observed: 0,
                    retained: Vec::new(),
                    stored: 0,
                    forwarded: 0,
                    eof: false,
                    exceeded_at: None,
                    losses: Vec::with_capacity(8),
                    sink: match &policy.sink {
                        Sink::File(path) => Some(path.clone()),
                        Sink::Discard | Sink::Memory => None,
                    },
                },
                reader_stopped: false,
                worker_done: matches!(policy.sink, Sink::Discard | Sink::Memory),
                failed: false,
                frozen: false,
                events: None,
            }));
            let (sender, receiver) = mpsc::unbounded_channel();
            let space = Arc::new(Semaphore::new(QUEUE_BYTES));
            let worker = if let Sink::File(path) = &policy.sink {
                if test.fail_start {
                    return Err(NotStarted::Worker);
                }
                let writer = sys::CaptureFile::create(path).map_err(NotStarted::Setup)?;
                let state = state.clone();
                let space = space.clone();
                let credit = credits.pop().expect("reserved sink credit");
                let test = test.clone();
                Some(
                    std::thread::Builder::new()
                        .name("cpf-sink".into())
                        .spawn(move || worker(writer, receiver, state, space, credit, test))
                        .map_err(|_| NotStarted::Worker)?,
                )
            } else {
                drop(receiver);
                None
            };
            let (drain, deadline) = watch::channel(None);
            let pending = Some(PendingReader {
                pipe: pipe.parent,
                sender,
                space,
                deadline,
                budget: budget_tx.clone(),
            });
            captures.streams.push(Capture {
                stream,
                policy,
                state,
                drain,
                reader: None,
                pending,
                worker,
            });
            *output = OutputSpec::Pipe(pipe.child);
        }
        Ok(captures)
    }

    pub(in crate::supervisor) fn take_admission(&mut self) -> Admission {
        self.admission.take().expect("setup owns admission")
    }

    pub(in crate::supervisor) fn start_readers(&mut self) {
        for capture in &mut self.streams {
            let pending = capture.pending.take().expect("start once");
            capture.reader = Some(tokio::spawn(read(
                pending.pipe,
                capture.policy.clone(),
                capture.stream,
                capture.state.clone(),
                pending.sender,
                pending.space,
                pending.deadline,
                pending.budget,
                self.activate.subscribe(),
            )));
        }
    }

    pub(in crate::supervisor) fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    pub(in crate::supervisor) fn set_events(&self, events: Option<EventSender>) {
        for capture in &self.streams {
            if capture.policy.events {
                capture
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .events = events.clone();
            }
        }
        self.activate.send_replace(true);
    }

    pub(in crate::supervisor) fn budget_receiver(&self) -> watch::Receiver<Option<Stream>> {
        self.budget.clone()
    }

    pub(in crate::supervisor) fn start_drain(&mut self, transfer_cutoff: Option<Instant>) {
        self.activate.send_replace(true);
        if let Some(cutoff) = transfer_cutoff {
            self.transfer_cutoff = Some(cutoff);
        }
        let start = *self.drain_start.get_or_insert_with(Instant::now);
        for capture in &self.streams {
            capture.drain.send_replace(Some(start + DRAIN));
        }
    }

    /// Nonblocking; the same freeze operation is used by supervisor and service.
    pub(in crate::supervisor) fn poll_freeze(&mut self, outcome: &mut RootOutcome) -> bool {
        let Some(start) = self.drain_start else {
            return false;
        };
        let now = Instant::now();
        let cutoff = self.transfer_cutoff.unwrap_or(start + FINALISE);
        let mut all = true;
        for capture in &mut self.streams {
            // The cleanup poller must never wait for a reader/worker lock.
            let mut state = match capture.state.try_lock() {
                Ok(state) => state,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    all = false;
                    continue;
                }
            };
            if !state.reader_stopped && (now >= start + DRAIN || now >= cutoff) {
                // The stopped flag under this lock cuts off admission, even
                // for a concurrently-ready read whose abort is still pending.
                if let Some(reader) = &capture.reader {
                    reader.abort();
                }
                state.reader_stopped = true;
                state.loss(CaptureLoss::NoEofBeforeDrainDeadline);
            }
            if !state.frozen && state.reader_stopped && (state.worker_done || now >= cutoff) {
                if !state.worker_done {
                    state.loss(CaptureLoss::WriteStalled);
                }
                state.frozen = true;
                let slot = match capture.stream {
                    Stream::Stdout => &mut outcome.stdout,
                    Stream::Stderr => &mut outcome.stderr,
                };
                *slot = Some(state.record.clone());
                if let Some(offset) = state.record.exceeded_at {
                    let violation = Violation::OutputBudget {
                        stream: capture.stream,
                        offset,
                    };
                    if !outcome.violations.contains(&violation) {
                        outcome.violations.push(violation);
                    }
                    if capture.policy.on_exceed == Exceed::Kill
                        && outcome.stop_trigger != Some(StopTrigger::OutputBudget(capture.stream))
                    {
                        let note = supervisor::Note::LateTrigger(StopTrigger::OutputBudget(
                            capture.stream,
                        ));
                        if outcome.notes.len() < 16 && !outcome.notes.contains(&note) {
                            outcome.notes.push(note);
                        }
                    }
                }
            }
            all &= state.frozen;
        }
        all
    }

    pub(in crate::supervisor) fn resources_released(&mut self) -> bool {
        resources_released(&mut self.streams)
    }

    pub(in crate::supervisor) async fn finish(
        &mut self,
        outcome: &mut RootOutcome,
        transferred: bool,
    ) {
        self.start_drain(transferred.then(|| Instant::now() + DRAIN));
        while !self.poll_freeze(outcome) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // Let completed workers release their thread credits before normal
        // return. A frozen stalled worker stays owned for service transfer.
        let deadline = self
            .transfer_cutoff
            .unwrap_or(self.drain_start.unwrap() + FINALISE);
        while !self.resources_released() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}

impl Drop for Captures {
    fn drop(&mut self) {
        for capture in &mut self.streams {
            if let Some(reader) = &capture.reader {
                reader.abort();
            }
            // Closing an unstarted reader's queue lets the worker finish, but
            // even an empty sink can block during flush or close.
            capture.pending.take();
        }
        if let Some(admission) = self.admission.take() {
            if self.streams.iter().any(|capture| capture.worker.is_some()) {
                admission.transfer_failed_captures(FailedCaptures {
                    streams: std::mem::take(&mut self.streams),
                });
            }
        }
    }
}

/// Failed setup retains its slot, worker credits and files until workers return.
#[derive(Debug)]
pub(in crate::supervisor) struct FailedCaptures {
    streams: Vec<Capture>,
}

impl FailedCaptures {
    pub(in crate::supervisor) fn resources_released(&mut self) -> bool {
        resources_released(&mut self.streams)
    }
}

fn resources_released(streams: &mut [Capture]) -> bool {
    let mut done = true;
    for capture in streams {
        if capture.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = capture.worker.take().unwrap().join();
        }
        done &= capture.worker.is_none()
            && capture
                .reader
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished);
    }
    done
}

async fn drain_deadline(receiver: &mut watch::Receiver<Option<Instant>>) {
    loop {
        let deadline = *receiver.borrow_and_update();
        if let Some(deadline) = deadline {
            tokio::time::sleep_until(deadline.into()).await;
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn read(
    mut pipe: ParentReader,
    policy: StreamPolicy,
    stream: Stream,
    state: Shared,
    sender: mpsc::UnboundedSender<Queued>,
    space: Arc<Semaphore>,
    mut drain: watch::Receiver<Option<Instant>>,
    budget: watch::Sender<Option<Stream>>,
    mut activate: watch::Receiver<bool>,
) {
    let _guard = ReaderGuard(state.clone());
    if activate.wait_for(|ready| *ready).await.is_err() {
        return;
    }
    let mut buffer = vec![0; READ_BYTES];
    loop {
        let result = async {
            let discard = {
                let state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.failed
                    || state.record.observed >= policy.budget
                    || matches!(policy.sink, Sink::Discard | Sink::Memory)
            };
            // Reserve before reading: every byte observed for storage has queue
            // space already, including the final clipped prefix at drain expiry.
            let permit = if discard {
                None
            } else {
                space
                    .clone()
                    .acquire_many_owned(READ_BYTES as u32)
                    .await
                    .ok()
            };
            let result = pipe.read_chunk(&mut buffer).await;
            (result, permit)
        };
        let (result, mut permit) = tokio::select! { biased;
            _ = drain_deadline(&mut drain) => break,
            result = result => result,
        };
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        if state.frozen || state.reader_stopped {
            break;
        }
        match result {
            ReadResult::Data(n) => {
                let before = state.record.observed;
                let Some(observed) = before.checked_add(n as u64) else {
                    state.loss(CaptureLoss::PipeRead(0));
                    break;
                };
                state.record.observed = observed;
                if observed > policy.budget && state.record.exceeded_at.is_none() {
                    state.record.exceeded_at = policy.budget.checked_add(1);
                    if policy.on_exceed == Exceed::Kill {
                        budget.send_if_modified(|value| {
                            if value.is_none() {
                                *value = Some(stream);
                                true
                            } else {
                                false
                            }
                        });
                    }
                }
                if observed > policy.budget && policy.on_exceed == Exceed::Discard {
                    state.loss(CaptureLoss::BudgetDiscard);
                }
                let admitted = (policy.budget.saturating_sub(before)).min(n as u64) as usize;
                if admitted != 0 {
                    if matches!(policy.sink, Sink::Memory) {
                        state.record.retained.extend_from_slice(&buffer[..admitted]);
                        state.record.stored += admitted as u64;
                    }
                    if let Some(events) = &state.events {
                        events.offer(stream, &buffer[..admitted]);
                    }
                    if !state.failed {
                        if let Some(mut permit) = permit.take() {
                            let used = permit.split(admitted).expect("reserved chunk");
                            let _ = sender.send(Queued {
                                bytes: buffer[..admitted].to_vec(),
                                _permit: used,
                            });
                        }
                    }
                }
            }
            ReadResult::Eof => {
                state.record.eof = true;
                break;
            }
            ReadResult::Failed(error) => {
                state.loss(CaptureLoss::PipeRead(error.raw_os_error().unwrap_or(0)));
                break;
            }
        }
    }
}

fn worker(
    mut writer: sys::CaptureFile,
    mut receiver: mpsc::UnboundedReceiver<Queued>,
    state: Shared,
    space: Arc<Semaphore>,
    _credit: Credits,
    mut test: SinkTestOptions,
) {
    let result = (|| -> io::Result<()> {
        let mut written = 0_u64;
        while let Some(chunk) = receiver.blocking_recv() {
            if let Some(gate) = test.gate.take() {
                gate.wait();
            }
            let mut bytes = &chunk.bytes[..];
            while !bytes.is_empty() {
                let result = if std::mem::take(&mut test.interrupt_once) {
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else if std::mem::take(&mut test.zero_once) {
                    Ok(0)
                } else if test.fail_after.is_some_and(|limit| written >= limit) {
                    Err(io::Error::from_raw_os_error(5))
                } else {
                    let limit = test
                        .max_write
                        .unwrap_or(bytes.len())
                        .max(1)
                        .min(bytes.len());
                    let limit = test
                        .fail_after
                        .map_or(limit, |end| limit.min((end - written) as usize));
                    writer.write(&bytes[..limit])
                };
                match result {
                    Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                    Ok(n) => {
                        written = written
                            .checked_add(n as u64)
                            .ok_or_else(|| io::Error::other("stored byte count overflow"))?;
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        if !state.frozen {
                            state.record.stored = written;
                        }
                        bytes = &bytes[n..];
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
        }
        if test.fail_flush {
            return Err(io::Error::from_raw_os_error(5));
        }
        if let Some(gate) = test.close_gate.take() {
            gate.wait();
        }
        writer.finish()?;
        if test.fail_close {
            return Err(io::Error::from_raw_os_error(5));
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.failed = true;
        state.loss(CaptureLoss::WriteError(error.raw_os_error().unwrap_or(0)));
    }
    // Close the queue and wake readers waiting for capacity before discarding
    // queued buffers. No sink error can leave the child back-pressured.
    receiver.close();
    space.close();
    drop(receiver);
    // writer (including error paths) is closed before completion is published.
    drop(writer);
    state.lock().unwrap_or_else(|e| e.into_inner()).worker_done = true;
}
