//! Raw stdin belongs to this short-lived invocation, including a helper that
//! may remain blocked in read until process teardown (execution contract §8.1).
use super::{Credits, NotStarted, StdinSpec, sys};
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

#[derive(Debug)]
pub(super) struct InputForward {
    #[cfg(all(
        feature = "storage-test-hooks",
        any(target_os = "linux", target_os = "macos")
    ))]
    _owned: crate::supervisor::test_hooks::OwnedResource,
    helper: Option<JoinHandle<()>>,
    start: Option<std::sync::mpsc::Sender<()>>,
    pump: tokio::task::JoinHandle<()>,
    stopped: Arc<AtomicBool>,
}

impl InputForward {
    pub(super) fn prepare(stdin: &mut StdinSpec, credit: Credits) -> Result<Self, NotStarted> {
        let (input, mut output) = sys::input_pipe().map_err(NotStarted::Setup)?;
        // Four 16 KiB chunks: at most 64 KiB waiting for the child pipe.
        let (send, mut receive) = mpsc::channel::<Vec<u8>>(4);
        let (start, ready) = std::sync::mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = stopped.clone();
        let helper = std::thread::Builder::new()
            .name("cpf-stdin".into())
            .spawn(move || {
                let _credit = credit;
                // Failed creation must never consume console input.
                if ready.recv().is_err() {
                    return;
                }
                let mut input = std::io::stdin().lock();
                while !stopping.load(Ordering::Acquire) {
                    let mut bytes = vec![0; 16 * 1024];
                    match input.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => {
                            bytes.truncate(n);
                            if stopping.load(Ordering::Acquire)
                                || send.blocking_send(bytes).is_err()
                            {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .map_err(|_| NotStarted::Worker)?;
        let stopping = stopped.clone();
        let pump = tokio::spawn(async move {
            while let Some(bytes) = receive.recv().await {
                if output.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            // EOF drops the writer, closing the child's stdin. A broken pipe
            // drops the receiver and stops the helper's next send.
            stopping.store(true, Ordering::Release);
        });
        *stdin = StdinSpec::Pipe(input);
        Ok(Self {
            #[cfg(all(
                feature = "storage-test-hooks",
                any(target_os = "linux", target_os = "macos")
            ))]
            _owned: crate::supervisor::test_hooks::OwnedResource::new(),
            helper: Some(helper),
            start: Some(start),
            pump,
            stopped,
        })
    }

    pub(super) fn start(&mut self) {
        if let Some(start) = self.start.take() {
            let _ = start.send(());
        }
    }

    pub(super) fn stop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.start.take();
        self.pump.abort();
    }

    /// The pump stops promptly once aborted; the helper may block in read.
    pub(super) fn pump_finished(&self) -> bool {
        self.pump.is_finished()
    }

    pub(super) fn resources_released(&mut self) -> bool {
        if self.helper.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.helper.take().unwrap().join();
        }
        self.helper.is_none() && self.pump.is_finished()
    }
}

impl Drop for InputForward {
    fn drop(&mut self) {
        self.stop();
    }
}
