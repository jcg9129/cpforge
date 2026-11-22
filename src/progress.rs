//! Bounded, non-blocking human progress on stderr (report contract §8).
//!
//! Lines are sanitized and cut to 512 bytes including the newline, then
//! offered to a 64 KiB queue charged in encoded bytes. Producers never wait: a
//! line that does not fit, or meets queue-lock contention, is dropped and
//! counted. `ProgressQueue::close` (phase E) freezes the count, reported as the
//! `progress_dropped` warning. The final summary goes through the same writer
//! thread after the queued lines. Stderr errors are ignored.
use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub const QUEUE_BYTES: usize = 64 * 1024;
pub const LINE_BYTES: usize = 512;
const CLOSED: u64 = 1 << 63;

#[derive(Debug, Default)]
struct State {
    lines: VecDeque<String>,
    bytes: usize,
    summary: Option<String>,
    finish: bool,
}
#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    drops: AtomicU64,
}
#[derive(Clone, Debug, Default)]
pub struct ProgressQueue(Arc<Shared>);

fn bounded_line(line: &str) -> String {
    // Bound allocation and producer work as well as the encoded line. Progress
    // cannot inject additional lines or terminal control sequences.
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
    fn dropped(&self) {
        // A hand-written update loop: `fetch_update` is deprecated on current
        // toolchains while its replacement is newer than the supported 1.85.
        let drops = &self.0.drops;
        let mut value = drops.load(Ordering::Acquire);
        while value & CLOSED == 0 {
            let next = value.saturating_add(1).min(CLOSED - 1);
            match drops.compare_exchange_weak(value, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(actual) => value = actual,
            }
        }
    }
    /// Never waits for either the writer or queue space.
    pub fn offer(&self, line: &str) -> bool {
        if self.0.drops.load(Ordering::Acquire) & CLOSED != 0 {
            return false;
        }
        let line = bounded_line(line);
        let Ok(mut state) = self.0.state.try_lock() else {
            self.dropped();
            return false;
        };
        if self.0.drops.load(Ordering::Acquire) & CLOSED != 0 {
            return false;
        }
        if state.bytes + line.len() > QUEUE_BYTES {
            self.dropped();
            return false;
        }
        state.bytes += line.len();
        state.lines.push_back(line);
        drop(state);
        self.0.wake.notify_one();
        true
    }
    /// Phase E barrier: later offers cannot change the finalized count.
    pub fn close(&self) -> u32 {
        let _state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let dropped = self.0.drops.fetch_or(CLOSED, Ordering::AcqRel) & !CLOSED;
        dropped.min(u32::MAX as u64) as u32
    }
}

#[derive(Debug)]
pub struct ProgressWriter {
    queue: ProgressQueue,
    worker: Option<JoinHandle<()>>,
}
impl ProgressWriter {
    pub fn new(mut writer: impl Write + Send + 'static) -> std::io::Result<Self> {
        let queue = ProgressQueue::default();
        let shared = queue.0.clone();
        let worker = std::thread::Builder::new()
            .name("cpf-stderr".into())
            .spawn(move || {
                #[cfg(feature = "storage-test-hooks")]
                let mut first_write = true;
                loop {
                    let line = {
                        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(line) = state.lines.pop_front() {
                                state.bytes -= line.len();
                                break Some(line);
                            }
                            if let Some(summary) = state.summary.take() {
                                break Some(summary);
                            }
                            if state.finish {
                                break None;
                            }
                            state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    let Some(line) = line else {
                        break;
                    };
                    #[cfg(feature = "storage-test-hooks")]
                    if first_write {
                        first_write = false;
                        crate::storage::crash::barrier("stderr.write");
                    }
                    if !crate::cancel_signals::force_exit_started() {
                        let _ = writer.write_all(line.as_bytes());
                    }
                }
                if !crate::cancel_signals::force_exit_started() {
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
    /// Call after delivery. It waits for queued lines and this summary, through
    /// the same writer. Stderr errors never affect the command's status.
    pub fn finish(self, summary: &str) -> Result<(), crate::error::Error> {
        self.finish_lines(&[summary.to_owned()])
    }

    /// Final warnings and summary, each bounded and escaped like progress.
    /// These lines follow phase D and do not enter the closed progress queue.
    pub fn finish_lines(self, lines: &[String]) -> Result<(), crate::error::Error> {
        self.finish_lines_with_deadline(lines, None)
    }

    /// Raw mode may abandon a stalled console at process teardown. Bound the
    /// final diagnostic too, so it cannot undo the supervisor's flush bound.
    pub fn finish_raw_lines(self, lines: &[String]) -> Result<(), crate::error::Error> {
        self.finish_lines_with_deadline(lines, Some(std::time::Duration::from_secs(2)))
    }

    fn finish_lines_with_deadline(
        mut self,
        lines: &[String],
        timeout: Option<std::time::Duration>,
    ) -> Result<(), crate::error::Error> {
        self.queue.close();
        {
            let mut state = self.queue.0.state.lock().unwrap_or_else(|e| e.into_inner());
            state.summary = Some(lines.iter().map(String::as_str).map(bounded_line).collect());
            state.finish = true;
        }
        self.queue.0.wake.notify_one();
        if let Some(worker) = self.worker.take() {
            if let Some(timeout) = timeout {
                let deadline = std::time::Instant::now() + timeout;
                while !worker.is_finished() {
                    if std::time::Instant::now() >= deadline {
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
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
        self.queue.close();
        self.queue
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finish = true;
        self.queue.0.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_writer_drops_exactly_and_summary_follows_the_queue() {
        #[derive(Debug)]
        struct Blocked {
            entered: Option<std::sync::mpsc::Sender<()>>,
            release: std::sync::mpsc::Receiver<()>,
            bytes: Arc<Mutex<Vec<u8>>>,
        }
        impl Write for Blocked {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                self.bytes.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (entered, wait) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = ProgressWriter::new(Blocked {
            entered: Some(entered),
            release: blocked,
            bytes: bytes.clone(),
        })
        .unwrap();
        let queue = writer.queue();
        while !queue.offer("first") {
            std::thread::yield_now();
        }
        wait.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let startup_drops = queue.0.drops.load(Ordering::Acquire) as u32;
        for _ in 0..130 {
            queue.offer(&"x".repeat(511));
        }
        assert_eq!(queue.close(), startup_drops + 2);
        queue.offer("ignored");
        assert_eq!(queue.close(), startup_drops + 2);
        release.send(()).unwrap();
        writer.finish("last").unwrap();
        let bytes = bytes.lock().unwrap();
        assert!(bytes.starts_with(b"first\n"));
        assert!(bytes.ends_with(b"last\n"));
        assert!(
            bytes
                .split(|b| *b == b'\n')
                .all(|line| line.len() < LINE_BYTES)
        );
    }
    #[test]
    fn bounds_preserve_utf8_and_one_line() {
        let line = bounded_line(&"é\n".repeat(512));
        assert!(line.len() <= LINE_BYTES);
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with("…\n"));
    }
    #[test]
    fn stderr_errors_are_ignored_but_worker_panics_are_internal() {
        #[derive(Debug)]
        struct Fails(bool);
        impl Write for Fails {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                assert!(!self.0, "injected stderr panic");
                Err(std::io::Error::other("injected stderr error"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("injected flush error"))
            }
        }
        ProgressWriter::new(Fails(false))
            .unwrap()
            .finish("summary")
            .unwrap();
        let error = ProgressWriter::new(Fails(true))
            .unwrap()
            .finish("summary")
            .unwrap_err();
        assert_eq!(error.exit_code(), crate::exit::ExitCode::Internal);
    }
}
