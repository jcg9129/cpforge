//! One ordered human stderr stream (report contract §8).
//!
//! Result payloads are lossless within the plan's proven ceiling. Progress has
//! its own lossy budget and is detached at E. The mutex protects only prepared
//! entries and counters; the writer owns every syscall outside that mutex.
use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const QUEUE_BYTES: usize = 64 * 1024;
pub const LINE_BYTES: usize = 512;
const FINAL_BYTES: usize = 128 * 1024;
const MAX_TESTS: usize = 10_000;
const PROGRESS_ENTRIES: usize = QUEUE_BYTES / 64;

#[derive(Debug)]
struct ResultLine {
    seq: u64,
    text: String,
}
#[derive(Debug)]
struct ProgressLine {
    /// Number of result lines offered before this progress line.
    after: u64,
    text: String,
}
#[derive(Debug)]
struct Batch {
    next: u64,
    lines: VecDeque<String>,
}
#[derive(Debug)]
struct State {
    results: VecDeque<ResultLine>,
    progress: VecDeque<ProgressLine>,
    spare_progress: VecDeque<ProgressLine>,
    progress_bytes: usize,
    result_bytes: usize,
    result_ceiling: usize,
    next_seq: u64,
    drops: u32,
    closed: bool,
    /// The writer stopped on a write error: later lines are discarded
    /// without counting (report contract §8, write error).
    failed: bool,
    final_batch: Option<Batch>,
    finish: bool,
}
impl State {
    fn new(planned_tests: usize) -> Self {
        Self {
            // Reserve entry storage outside the lock, never payload bytes.
            // The allowance also accommodates diagnostics before plan closure.
            results: VecDeque::with_capacity(planned_tests + 256),
            progress: VecDeque::with_capacity(PROGRESS_ENTRIES),
            spare_progress: VecDeque::with_capacity(PROGRESS_ENTRIES),
            progress_bytes: 0,
            result_bytes: 0,
            result_ceiling: FINAL_BYTES,
            next_seq: 0,
            drops: 0,
            closed: false,
            failed: false,
            final_batch: None,
            finish: false,
        }
    }
}
trait Clock: std::fmt::Debug + Send + Sync {
    fn now(&self) -> Instant;
}
#[derive(Debug)]
struct Monotonic;
impl Clock for Monotonic {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stop: AtomicBool,
    deadline: OnceLock<Instant>,
    clock: Arc<dyn Clock>,
}
impl Shared {
    // Force exit reads atomics only, including between partial writes/retries.
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
            || crate::cancel_signals::force_exit_started()
            || self
                .deadline
                .get()
                .is_some_and(|end| self.clock.now() >= *end)
    }
}
#[derive(Clone, Debug)]
pub struct ProgressQueue(Arc<Shared>);

fn bounded_line(line: &str) -> String {
    let mut text = String::new();
    let mut truncated = false;
    for ch in line.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if text.len() + ch.len_utf8() >= LINE_BYTES {
            truncated = true;
            break;
        }
        text.push(ch);
    }
    if truncated {
        while text.len() > LINE_BYTES - "…\n".len() {
            text.pop();
        }
        text.push('…');
    }
    text.push('\n');
    text
}
impl ProgressQueue {
    /// Reserve metadata for the plan's maximum entry count before any offer.
    /// The result payload allowance is expanded only at `planned_tests`.
    pub fn new(planned_tests: usize) -> Self {
        Self::with_clock(Arc::new(Monotonic), planned_tests)
    }
    fn with_clock(clock: Arc<dyn Clock>, planned_tests: usize) -> Self {
        assert!(planned_tests <= MAX_TESTS);
        let state = State::new(planned_tests);
        Self(Arc::new(Shared {
            state: Mutex::new(state),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            deadline: OnceLock::new(),
            clock,
        }))
    }
    /// Fix the payload ceiling when the engine's plan is final.
    pub fn planned_tests(&self, tests: usize) {
        assert!(tests <= MAX_TESTS);
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        debug_assert!(state.results.capacity() >= tests + 256);
        state.result_ceiling = tests * LINE_BYTES + FINAL_BYTES;
    }
    /// Lossy progress: wait for the short queue lock, never for sink/space.
    pub fn offer(&self, line: &str) -> bool {
        let text = bounded_line(line);
        let charge = text.len().max(64);
        #[cfg(feature = "storage-test-hooks")]
        let pause = std::env::var("CPF_STORAGE_PAUSE_AT").is_ok_and(|point| point == "stderr.lock");
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        // This injection deliberately suspends a holder for the lock fixture;
        // production holders never wait or perform I/O here.
        #[cfg(feature = "storage-test-hooks")]
        if pause {
            crate::storage::crash::barrier("stderr.lock");
        }
        if state.closed || state.failed {
            return false;
        }
        if state.progress_bytes + charge > QUEUE_BYTES {
            state.drops = state.drops.saturating_add(1);
            return false;
        }
        let after = state.next_seq;
        state.progress_bytes += charge;
        state.progress.push_back(ProgressLine { after, text });
        drop(state);
        self.0.wake.notify_one();
        true
    }
    /// Lossless result, numbered in the same critical section as its enqueue.
    pub fn result(&self, line: &str) -> u64 {
        let text = bounded_line(line);
        #[cfg(feature = "storage-test-hooks")]
        let pause = std::env::var("CPF_STORAGE_PAUSE_AT").is_ok_and(|point| point == "stderr.lock");
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(feature = "storage-test-hooks")]
        if pause {
            crate::storage::crash::barrier("stderr.lock");
        }
        assert!(!state.finish, "result after final batch");
        let seq = state.next_seq;
        state.next_seq += 1;
        if state.failed {
            drop(state);
            return seq;
        }
        state.result_bytes += text.len();
        debug_assert!(state.result_bytes <= state.result_ceiling);
        debug_assert!(state.results.len() < state.results.capacity());
        state.results.push_back(ResultLine { seq, text });
        drop(state);
        self.0.wake.notify_one();
        seq
    }
    /// E: swap out all queued progress and freeze its count in one section.
    /// Deallocation of the detached holding happens after unlocking.
    pub fn close(&self) -> u32 {
        let (detached, drops) = {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return state.drops;
            }
            state.closed = true;
            if !state.failed {
                state.drops = state.drops.saturating_add(state.progress.len() as u32);
            }
            state.progress_bytes = 0;
            let State {
                progress,
                spare_progress,
                ..
            } = &mut *state;
            std::mem::swap(progress, spare_progress);
            (std::mem::take(spare_progress), state.drops)
        };
        drop(detached);
        drops
    }
    /// Rendering and allocation precede lock acquisition. The deadline is
    /// published at t_q, the end of the batch enqueue section, never at D.
    fn enqueue_final(&self, lines: &[String], timeout: Duration) -> Instant {
        self.close();
        let lines: VecDeque<_> = lines.iter().map(|line| bounded_line(line)).collect();
        let bytes: usize = lines.iter().map(String::len).sum();
        let count = lines.len() as u64;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!state.finish);
        state.result_bytes += bytes;
        debug_assert!(state.result_bytes <= state.result_ceiling);
        state.final_batch = Some(Batch {
            next: state.next_seq,
            lines,
        });
        state.next_seq += count;
        state.finish = true;
        let queued_at = self.0.clock.now();
        self.0
            .deadline
            .set(queued_at + timeout)
            .expect("one final batch");
        drop(state);
        self.0.wake.notify_one();
        #[cfg(feature = "storage-test-hooks")]
        crate::storage::crash::stderr_final_queued(queued_at);
        queued_at
    }
}

// One pop per critical section, including skipped progress. Strings and their
// allocations are returned to the caller, never destroyed under the mutex.
enum Next {
    Result(ResultLine),
    Progress(String),
    Skipped(String),
    End,
}
fn next(shared: &Shared) -> Next {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(progress) = state.progress.front() {
            let result = state.results.front();
            if result.is_none_or(|line| progress.after <= line.seq) {
                let skip = result.is_some();
                let line = state.progress.pop_front().expect("progress front");
                state.progress_bytes -= line.text.len().max(64);
                if skip {
                    state.drops = state.drops.saturating_add(1);
                    return Next::Skipped(line.text);
                }
                return Next::Progress(line.text);
            }
        }
        if let Some(line) = state.results.pop_front() {
            state.result_bytes -= line.text.len();
            return Next::Result(line);
        }
        if let Some(batch) = &mut state.final_batch {
            if let Some(text) = batch.lines.pop_front() {
                let seq = batch.next;
                batch.next += 1;
                state.result_bytes -= text.len();
                return Next::Result(ResultLine { seq, text });
            }
        }
        if state.finish
            || shared.stop.load(Ordering::Acquire)
            || crate::cancel_signals::force_exit_started()
        {
            return Next::End;
        }
        // Drop/force stop must not take this mutex to notify. A timed wait
        // also handles a stop racing the release-and-wait operation.
        state = shared
            .wake
            .wait_timeout(state, Duration::from_millis(100))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}
fn write_line(writer: &mut impl Write, mut bytes: &[u8], shared: &Shared) -> bool {
    while !bytes.is_empty() {
        if shared.stopped() {
            return false;
        }
        match writer.write(bytes) {
            Ok(0) => return false,
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

#[derive(Debug)]
pub struct ProgressWriter {
    queue: ProgressQueue,
    worker: Option<JoinHandle<()>>,
}
impl ProgressWriter {
    pub fn new(writer: impl Write + Send + 'static, planned_tests: usize) -> io::Result<Self> {
        Self::with_queue(writer, ProgressQueue::new(planned_tests))
    }
    fn with_queue(
        mut writer: impl Write + Send + 'static,
        queue: ProgressQueue,
    ) -> io::Result<Self> {
        let shared = queue.0.clone();
        let worker = std::thread::Builder::new()
            .name("cpf-stderr".into())
            .spawn(move || {
                #[cfg(feature = "storage-test-hooks")]
                let mut first_write = true;
                let mut expected_seq = 0;
                while !shared.stopped() {
                    let text = match next(&shared) {
                        Next::Result(line) => {
                            debug_assert_eq!(line.seq, expected_seq);
                            expected_seq += 1;
                            line.text
                        }
                        Next::Progress(text) => text,
                        Next::Skipped(text) => {
                            drop(text);
                            continue;
                        }
                        Next::End => break,
                    };
                    #[cfg(feature = "storage-test-hooks")]
                    if first_write {
                        first_write = false;
                        crate::storage::crash::barrier("stderr.write");
                    }
                    if !write_line(&mut writer, text.as_bytes(), &shared) {
                        if !shared.stopped() {
                            // A write error: discard every later line
                            // uncounted. Freeing happens outside the lock.
                            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                            state.failed = true;
                        }
                        return;
                    }
                }
                if !shared.stopped() {
                    let _ = writer.flush();
                }
            })?;
        Ok(Self {
            queue,
            worker: Some(worker),
        })
    }
    pub fn queue(&self) -> ProgressQueue {
        self.queue.clone()
    }
    pub fn finish(self, summary: &str) -> Result<(), crate::error::Error> {
        self.finish_lines(&[summary.to_owned()])
    }
    pub fn finish_lines(self, lines: &[String]) -> Result<(), crate::error::Error> {
        self.finish_lines_with_deadline(lines, Duration::from_secs(5))
    }
    /// Separate from the supervisor's console flush clock (drain + 2 s).
    pub fn finish_raw_lines(self, lines: &[String]) -> Result<(), crate::error::Error> {
        self.finish_lines_with_deadline(lines, Duration::from_secs(2))
    }
    fn finish_lines_with_deadline(
        mut self,
        lines: &[String],
        timeout: Duration,
    ) -> Result<(), crate::error::Error> {
        self.queue.enqueue_final(lines, timeout);
        self.wait()
    }
    fn wait(&mut self) -> Result<(), crate::error::Error> {
        if let Some(worker) = self.worker.take() {
            while !worker.is_finished() {
                if self.queue.0.stopped() {
                    self.queue.0.stop.store(true, Ordering::Release);
                    return Ok(()); // A syscall in progress belongs to the detached thread.
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            worker
                .join()
                .map_err(|_| crate::error::Error::Internal("stderr worker panicked".into()))?;
        }
        Ok(())
    }
}
impl Drop for ProgressWriter {
    fn drop(&mut self) {
        // In particular, an expired final drain must never reacquire the queue
        // lock on its way out. Shared owns the detached worker's memory.
        self.queue.0.stop.store(true, Ordering::Release);
        self.queue.0.wake.notify_one();
    }
}

/// Controlled stderr writer shared by portable full-engine fixtures.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct StalledWriter {
    writer: ProgressWriter,
    pub(crate) queue: ProgressQueue,
    clock: Arc<TestClock>,
    release: std::sync::mpsc::Sender<()>,
    pub(crate) output: Arc<Mutex<Vec<u8>>>,
}
#[cfg(test)]
#[derive(Debug)]
struct TestClock(Mutex<Instant>);
#[cfg(test)]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}
#[cfg(test)]
impl StalledWriter {
    pub(crate) fn new() -> Self {
        #[derive(Debug)]
        struct Sink {
            entered: Option<std::sync::mpsc::Sender<()>>,
            release: std::sync::mpsc::Receiver<()>,
            output: Arc<Mutex<Vec<u8>>>,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                    self.release
                        .recv_timeout(Duration::from_secs(7200))
                        .unwrap();
                }
                self.output.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let clock = Arc::new(TestClock(Mutex::new(Instant::now())));
        let queue = ProgressQueue::with_clock(clock.clone(), 0);
        let (entered, waiting) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = ProgressWriter::with_queue(
            Sink {
                entered: Some(entered),
                release: gate,
                output: output.clone(),
            },
            queue.clone(),
        )
        .unwrap();
        queue.result("startup diagnostic");
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        Self {
            writer,
            queue,
            clock,
            release,
            output,
        }
    }
    pub(crate) fn finish(mut self, lines: &[String]) -> Vec<u8> {
        let tq = self.queue.enqueue_final(lines, Duration::from_secs(5));
        // No results can drain while the first write is blocked: this is the
        // peak, with the whole final batch queued inside the fixed allowance.
        assert!(self.queue.0.state.lock().unwrap().result_bytes <= FINAL_BYTES);
        *self.clock.0.lock().unwrap() += Duration::from_secs(1);
        assert_eq!(self.clock.now().duration_since(tq), Duration::from_secs(1));
        self.release.send(()).unwrap();
        self.writer.wait().unwrap();
        self.output.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::mpsc::{self, Receiver, Sender};

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[derive(Debug)]
    struct ManualClock {
        origin: Instant,
        millis: AtomicU64,
    }
    impl ManualClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                origin: Instant::now(),
                millis: AtomicU64::new(0),
            })
        }
        fn advance(&self, millis: u64) {
            self.millis.fetch_add(millis, Ordering::AcqRel);
        }
    }
    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.origin + Duration::from_millis(self.millis.load(Ordering::Acquire))
        }
    }

    #[derive(Debug)]
    struct GatedSink {
        entered: Option<Sender<()>>,
        release: Receiver<()>,
        output: Arc<Mutex<Vec<u8>>>,
        calls: Arc<AtomicU64>,
    }
    impl Write for GatedSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release
                    .recv_timeout(TEST_TIMEOUT)
                    .expect("release sink gate");
            }
            self.output.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    type Gated = (
        ProgressWriter,
        Receiver<()>,
        Sender<()>,
        Arc<Mutex<Vec<u8>>>,
        Arc<AtomicU64>,
    );
    fn gated(queue: ProgressQueue) -> Gated {
        let (entered, waiting) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicU64::new(0));
        let writer = ProgressWriter::with_queue(
            GatedSink {
                entered: Some(entered),
                release: gate,
                output: output.clone(),
                calls: calls.clone(),
            },
            queue,
        )
        .unwrap();
        (writer, waiting, release, output, calls)
    }

    #[test]
    fn blocked_sink_keeps_results_and_detaches_progress_once_at_e() {
        let queue = ProgressQueue::new(3000);
        queue.planned_tests(3000);
        let (writer, entered, release, output, _) = gated(queue.clone());
        queue.result("startup");
        entered.recv_timeout(TEST_TIMEOUT).unwrap();
        for n in 0..3000 {
            assert_eq!(
                queue.result(&format!("case{n:04}  OK {}", "x".repeat(40))),
                n + 1
            );
            queue.offer("execution progress");
        }
        assert_eq!(queue.close(), 3000);
        assert!(!queue.offer("after E"));
        assert_eq!(queue.close(), 3000);
        // E permits further result offers until the final batch at t_q.
        assert_eq!(queue.result("after E result"), 3001);
        queue.enqueue_final(
            &["warning".into(), "summary".into()],
            Duration::from_secs(5),
        );
        assert_eq!(queue.close(), 3000);
        release.send(()).unwrap();
        let mut writer = writer;
        writer.wait().unwrap();
        let bytes = output.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 3004);
        for (n, line) in lines[1..3001].iter().enumerate() {
            assert!(line.starts_with(&format!("case{n:04}  OK ")));
        }
        assert_eq!(&lines[3001..], &["after E result", "warning", "summary"]);
        assert_eq!(queue.close(), 3000);
    }

    #[test]
    fn progress_budget_charges_at_least_64_bytes_and_skips_only_overtaken_lines() {
        let queue = ProgressQueue::new(0);
        for _ in 0..PROGRESS_ENTRIES {
            assert!(queue.offer("p"));
        }
        assert!(!queue.offer("overflow"));
        // A result after the progress makes the writer skip it, one pop at a
        // time; a result before progress is always written first.
        queue.result("result");
        for _ in 0..PROGRESS_ENTRIES {
            assert!(matches!(next(&queue.0), Next::Skipped(_)));
        }
        assert!(matches!(
            next(&queue.0),
            Next::Result(ResultLine { seq: 0, .. })
        ));
        assert!(queue.offer("taken before E"));
        assert!(matches!(next(&queue.0), Next::Progress(_)));
        assert_eq!(queue.close(), 1025);
        assert_eq!(queue.close(), 1025);
    }

    #[test]
    fn progress_offer_order_and_result_sequence_share_one_stream() {
        let queue = ProgressQueue::new(0);
        queue.result("r0");
        queue.offer("p0");
        assert!(matches!(
            next(&queue.0),
            Next::Result(ResultLine { seq: 0, .. })
        ));
        assert!(matches!(next(&queue.0), Next::Progress(_)));
        queue.offer("skip");
        queue.result("r1");
        queue.offer("p1");
        assert!(matches!(next(&queue.0), Next::Skipped(_)));
        assert!(matches!(
            next(&queue.0),
            Next::Result(ResultLine { seq: 1, .. })
        ));
        assert!(matches!(next(&queue.0), Next::Progress(_)));
        assert_eq!(queue.close(), 1);
    }

    #[test]
    fn full_result_ceiling_and_final_allowance_are_lossless() {
        // First the exact contract composition, then every byte of the full
        // allowance (including its 19 spare entries).
        for allowance_entries in [64 + 29 + 16 + 128, FINAL_BYTES / LINE_BYTES] {
            let queue = ProgressQueue::new(MAX_TESTS);
            queue.planned_tests(MAX_TESTS);
            let (writer, entered, release, output, _) = gated(queue.clone());
            assert!(queue.offer("in flight"));
            entered.recv_timeout(TEST_TIMEOUT).unwrap();
            let line = "x".repeat(LINE_BYTES - 1);
            let diagnostics = if allowance_entries == 237 { 64 } else { 0 };
            for _ in 0..diagnostics {
                queue.result(&line);
            }
            for n in 0..MAX_TESTS {
                let text = format!("{n:05} {}", "x".repeat(505));
                assert_eq!(bounded_line(&text).len(), LINE_BYTES);
                assert_eq!(queue.result(&text), (n + diagnostics) as u64);
            }
            let final_lines = vec![line; allowance_entries - diagnostics];
            queue.enqueue_final(&final_lines, Duration::from_secs(5));
            let expected_bytes = (MAX_TESTS + allowance_entries) * LINE_BYTES;
            {
                let state = queue.0.state.lock().unwrap();
                assert_eq!(state.result_bytes, expected_bytes);
                assert!(state.result_bytes <= 5_251_072);
                assert_eq!(state.next_seq, (MAX_TESTS + allowance_entries) as u64);
            }
            release.send(()).unwrap();
            let mut writer = writer;
            writer.wait().unwrap();
            let bytes = output.lock().unwrap();
            assert!(bytes.starts_with(b"in flight\n"));
            assert_eq!(bytes.len(), expected_bytes + b"in flight\n".len());
            let tests = &bytes[b"in flight\n".len() + diagnostics * LINE_BYTES..];
            for (n, line) in tests.chunks_exact(LINE_BYTES).take(MAX_TESTS).enumerate() {
                assert!(line.starts_with(format!("{n:05} ").as_bytes()));
                assert_eq!(line.last(), Some(&b'\n'));
            }
        }
    }

    #[test]
    fn build_free_summary_uses_final_allowance() {
        let queue = ProgressQueue::new(0);
        queue.planned_tests(0);
        queue.enqueue_final(&["build succeeded".into()], Duration::from_secs(5));
        assert!(matches!(
            next(&queue.0),
            Next::Result(ResultLine { seq: 0, .. })
        ));
        assert!(matches!(next(&queue.0), Next::End));
    }

    #[test]
    fn lock_contention_waits_without_drops_and_stop_never_takes_lock() {
        for result in [false, true] {
            let queue = ProgressQueue::new(0);
            let guard = queue.0.state.lock().unwrap();
            let (started, waiting) = mpsc::channel();
            let (done, completed) = mpsc::channel();
            let producer = queue.clone();
            let thread = std::thread::spawn(move || {
                started.send(()).unwrap();
                if result {
                    producer.result("result");
                } else {
                    assert!(producer.offer("progress"));
                }
                done.send(()).unwrap();
            });
            waiting.recv_timeout(TEST_TIMEOUT).unwrap();
            assert!(completed.try_recv().is_err());
            // This is also the local final-drain stop path; force exit uses the
            // process atomic read by the same predicate, with no queue access.
            queue.0.stop.store(true, Ordering::Release);
            assert!(queue.0.stopped());
            drop(guard);
            completed.recv_timeout(TEST_TIMEOUT).unwrap();
            thread.join().unwrap();
            let line = next(&queue.0);
            assert!(if result {
                matches!(line, Next::Result(_))
            } else {
                matches!(line, Next::Progress(_))
            });
            assert_eq!(queue.close(), 0);
        }
    }

    #[derive(Debug)]
    struct Scripted {
        clock: Arc<ManualClock>,
        millis_per_call: u64,
        partial: bool,
        interrupt: bool,
        calls: usize,
        bytes: Vec<u8>,
    }
    impl Write for Scripted {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.clock.advance(self.millis_per_call);
            self.calls += 1;
            if self.interrupt && self.calls % 2 == 1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let n = if self.partial { 1 } else { bytes.len() };
            self.bytes.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn scripted(clock: Arc<ManualClock>, millis: u64, partial: bool, interrupt: bool) -> Scripted {
        Scripted {
            clock,
            millis_per_call: millis,
            partial,
            interrupt,
            calls: 0,
            bytes: Vec::new(),
        }
    }

    #[test]
    fn partial_writes_and_eintr_are_byte_identical_to_unrestricted_sink() {
        for (partial, interrupt) in [(false, false), (true, false), (false, true), (true, true)] {
            let clock = ManualClock::new();
            let queue = ProgressQueue::with_clock(clock.clone(), 0);
            let mut sink = scripted(clock, 0, partial, interrupt);
            let text = bounded_line("é\nresult");
            assert!(write_line(&mut sink, text.as_bytes(), &queue.0));
            assert_eq!(sink.bytes, text.as_bytes());
        }
    }

    #[test]
    fn one_absolute_deadline_covers_partial_writes_and_eintr() {
        for interrupt in [false, true] {
            let clock = ManualClock::new();
            let queue = ProgressQueue::with_clock(clock.clone(), 0);
            // Work before t_q (rendering, delivery or queue-lock contention)
            // cannot consume the final drain's budget.
            clock.advance(1000);
            let tq = queue.enqueue_final(&["summary".into()], Duration::from_secs(5));
            assert_eq!(tq, clock.origin + Duration::from_secs(1));
            let mut sink = scripted(clock.clone(), 10, true, interrupt);
            assert!(!write_line(&mut sink, &[b'x'; 1024], &queue.0));
            assert_eq!(sink.calls, 500);
            assert_eq!(clock.now(), tq + Duration::from_secs(5));
            assert_eq!(sink.bytes.len(), if interrupt { 250 } else { 500 });
            assert!(!write_line(&mut sink, b"later", &queue.0));
            assert_eq!(sink.calls, 500);
        }
    }

    #[test]
    fn lock_delay_before_final_enqueue_does_not_consume_final_drain() {
        let clock = ManualClock::new();
        let queue = ProgressQueue::with_clock(clock.clone(), 0);
        let guard = queue.0.state.lock().unwrap();
        let producer = queue.clone();
        let (started, waiting) = mpsc::channel();
        let (finished, queued) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            started.send(()).unwrap();
            let tq = producer.enqueue_final(&["summary".into()], Duration::from_secs(5));
            finished.send(tq).unwrap();
        });
        waiting.recv_timeout(TEST_TIMEOUT).unwrap();
        assert!(queue.0.deadline.get().is_none());
        clock.advance(1000); // Intentional lock hold between D and t_q.
        drop(guard);
        let tq = queued.recv_timeout(TEST_TIMEOUT).unwrap();
        assert_eq!(tq, clock.origin + Duration::from_secs(1));
        assert_eq!(
            *queue.0.deadline.get().unwrap(),
            clock.origin + Duration::from_secs(6)
        );
        thread.join().unwrap();
    }

    #[test]
    fn raw_final_wait_has_its_own_enqueue_origin() {
        let clock = ManualClock::new();
        let queue = ProgressQueue::with_clock(clock.clone(), 0);
        clock.advance(2000); // Earlier console flush deadline has elapsed.
        let tq = queue.enqueue_final(&["child exited with code 1".into()], Duration::from_secs(2));
        let mut sink = scripted(clock.clone(), 10, true, false);
        assert!(!write_line(&mut sink, &[b'x'; 1024], &queue.0));
        assert_eq!(sink.calls, 200);
        assert_eq!(clock.now(), tq + Duration::from_secs(2));
    }

    #[test]
    fn final_deadline_abandons_blocked_call_without_acquiring_queue_lock() {
        let clock = ManualClock::new();
        let queue = ProgressQueue::with_clock(clock.clone(), 0);
        let (mut writer, entered, release, output, calls) = gated(queue.clone());
        queue.enqueue_final(&["first".into(), "summary".into()], Duration::from_secs(5));
        entered.recv_timeout(TEST_TIMEOUT).unwrap();
        // Holding this lock proves both the deadline exit and Drop do not
        // depend on it. The sink gate keeps the syscall blocked at expiry.
        let guard = queue.0.state.lock().unwrap();
        clock.advance(5000);
        writer.wait().unwrap();
        drop(writer);
        assert!(queue.0.stop.load(Ordering::Acquire));
        assert!(output.lock().unwrap().is_empty());
        assert_eq!(calls.load(Ordering::Acquire), 1);
        drop(guard);
        release.send(()).unwrap();
    }

    #[test]
    fn stop_between_partial_writes_begins_no_further_call() {
        struct StopAfterByte<'a> {
            shared: &'a Shared,
            calls: usize,
        }
        impl Write for StopAfterByte<'_> {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                self.shared.stop.store(true, Ordering::Release);
                Ok(1)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let queue = ProgressQueue::new(0);
        let guard = queue.0.state.lock().unwrap();
        let mut sink = StopAfterByte {
            shared: &queue.0,
            calls: 0,
        };
        assert!(!write_line(&mut sink, b"summary\n", &queue.0));
        assert_eq!(sink.calls, 1);
        drop(guard);
    }

    #[test]
    fn epipe_at_first_or_hundredth_line_stops_the_writer_without_status_error() {
        struct Fails {
            fail_at: u64,
            calls: Arc<AtomicU64>,
        }
        impl Write for Fails {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let n = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
                if n == self.fail_at {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for fail_at in [1, 100] {
            let queue = ProgressQueue::new(0);
            for _ in 0..200 {
                queue.result("result");
            }
            let calls = Arc::new(AtomicU64::new(0));
            let writer = ProgressWriter::with_queue(
                Fails {
                    fail_at,
                    calls: calls.clone(),
                },
                queue,
            )
            .unwrap();
            writer.finish("summary").unwrap();
            assert_eq!(calls.load(Ordering::Acquire), fail_at);
        }
    }

    #[test]
    fn lines_after_a_write_error_are_discarded_without_counting() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let queue = ProgressQueue::new(1);
        queue.result("first");
        let mut writer = ProgressWriter::with_queue(Broken, queue.clone()).unwrap();
        // Joining the writer is the synchronization: it has stopped on the error.
        writer.worker.take().unwrap().join().unwrap();
        for _ in 0..2 * PROGRESS_ENTRIES {
            assert!(!queue.offer("progress after the error"));
        }
        assert_eq!(queue.result("later result"), 1);
        assert_eq!(queue.close(), 0);
        writer.finish("summary").unwrap();
        assert_eq!(queue.close(), 0);
    }

    #[test]
    fn bounds_preserve_utf8_and_one_line() {
        let line = bounded_line(&"é\n".repeat(512));
        assert!(line.len() <= LINE_BYTES);
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with("…\n"));
        assert_eq!(bounded_line(&"x".repeat(511)).len(), LINE_BYTES);
    }

    #[test]
    fn worker_panics_are_internal() {
        struct Panics;
        impl Write for Panics {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                panic!("injected stderr panic")
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let error = ProgressWriter::new(Panics, 0)
            .unwrap()
            .finish("summary")
            .unwrap_err();
        assert_eq!(error.exit_code(), crate::exit::ExitCode::Internal);
    }
}

#[cfg(test)]
mod real_clock_tests {
    use super::*;
    use std::sync::mpsc;

    fn allowance() -> Duration {
        let scale = if cfg!(target_os = "macos") {
            4
        } else if std::env::var_os("GITHUB_ACTIONS").is_some() {
            2
        } else {
            1
        };
        Duration::from_millis(100 * scale)
    }

    #[test]
    fn final_drain_real_clock_abandons_stalled_call_and_third_two_second_write() {
        for stalled in [true, false] {
            struct Service {
                time: Duration,
                interrupt: mpsc::Receiver<()>,
                bytes: Arc<Mutex<Vec<u8>>>,
                stopped: mpsc::Sender<()>,
            }
            impl Write for Service {
                fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                    // Scripted syscall service time, not a synchronization
                    // guess. The gate permits cleanup after abandonment.
                    match self.interrupt.recv_timeout(self.time) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            self.bytes.lock().unwrap().extend_from_slice(bytes);
                            Ok(bytes.len())
                        }
                        _ => Err(io::ErrorKind::BrokenPipe.into()),
                    }
                }
                fn flush(&mut self) -> io::Result<()> {
                    Ok(())
                }
            }
            impl Drop for Service {
                fn drop(&mut self) {
                    let _ = self.stopped.send(());
                }
            }
            let (release, interrupt) = mpsc::channel();
            let (stopped, joined) = mpsc::channel();
            let bytes = Arc::new(Mutex::new(Vec::new()));
            let mut writer = ProgressWriter::new(
                Service {
                    time: Duration::from_secs(if stalled { 30 } else { 2 }),
                    interrupt,
                    bytes: bytes.clone(),
                    stopped,
                },
                0,
            )
            .unwrap();
            let tq = writer.queue.enqueue_final(
                &[
                    "first".into(),
                    "second".into(),
                    "third".into(),
                    "summary".into(),
                ],
                Duration::from_secs(5),
            );
            writer.wait().unwrap();
            assert!(tq.elapsed() >= Duration::from_secs(5));
            assert!(tq.elapsed() <= Duration::from_secs(5) + allowance());
            assert_eq!(
                &*bytes.lock().unwrap(),
                if stalled {
                    b"".as_slice()
                } else {
                    b"first\nsecond\n".as_slice()
                }
            );
            release.send(()).unwrap();
            joined.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }

    #[test]
    fn final_drain_real_clock_partial_and_permanent_eintr_do_not_restart_deadline() {
        for interrupted in [false, true] {
            struct Slow {
                timer: mpsc::Receiver<()>,
                interrupted: bool,
                bytes: Arc<Mutex<Vec<u8>>>,
                stopped: mpsc::Sender<()>,
            }
            impl Write for Slow {
                fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                    assert!(matches!(
                        self.timer.recv_timeout(Duration::from_millis(10)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ));
                    if self.interrupted {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    self.bytes.lock().unwrap().push(bytes[0]);
                    Ok(1)
                }
                fn flush(&mut self) -> io::Result<()> {
                    Ok(())
                }
            }
            impl Drop for Slow {
                fn drop(&mut self) {
                    let _ = self.stopped.send(());
                }
            }
            let (_timer, waiting) = mpsc::channel();
            let (stopped, joined) = mpsc::channel();
            let bytes = Arc::new(Mutex::new(Vec::new()));
            let mut writer = ProgressWriter::new(
                Slow {
                    timer: waiting,
                    interrupted,
                    bytes: bytes.clone(),
                    stopped,
                },
                0,
            )
            .unwrap();
            let tq = writer.queue.enqueue_final(
                &["x".repeat(LINE_BYTES - 1), "summary".into()],
                Duration::from_secs(5),
            );
            writer.wait().unwrap();
            assert!(tq.elapsed() >= Duration::from_secs(5));
            assert!(tq.elapsed() <= Duration::from_secs(5) + allowance());
            joined.recv_timeout(Duration::from_secs(5)).unwrap();
            let bytes = bytes.lock().unwrap();
            if interrupted {
                assert!(bytes.is_empty());
            } else {
                assert!(!bytes.is_empty());
                assert!(bytes.len() < LINE_BYTES);
                assert!(!bytes.ends_with(b"\n"));
            }
        }
    }
}
