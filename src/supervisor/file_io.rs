//! File-I/O capture ownership. Blocking operations belong to creation/capture/cwd workers.
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct StagedInput {
    pub name: OsString,
    pub source: PathBuf,
}
#[derive(Clone, Debug)]
pub struct OutputFileSpec {
    pub name: OsString,
    pub budget: u64,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use implementation::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod implementation {
    use super::*;
    use crate::report::{CollectionFailure, IoScratchDetail, OutputFile};
    use crate::storage::{budget::Reservation, job::JobOwner};
    use crate::supervisor::{Cleanup, Credits, NotStarted, RootOutcome, StopTrigger, Termination};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread::JoinHandle;

    #[derive(Clone, Debug, Default)]
    pub(crate) struct FileIoState(Arc<Mutex<State>>);
    #[derive(Debug, Default)]
    struct State {
        scratch: Option<Scratch>,
        warnings: Vec<IoScratchDetail>,
    }
    impl FileIoState {
        fn lock(&self) -> std::sync::MutexGuard<'_, State> {
            self.0.lock().unwrap_or_else(|e| e.into_inner())
        }
        pub(crate) fn take(&self) -> Option<Scratch> {
            self.lock().scratch.take()
        }
        pub(crate) fn warn(&self, warning: IoScratchDetail) {
            self.lock().warn(warning);
        }
        pub(crate) fn warnings(&self) -> Vec<IoScratchDetail> {
            self.lock().warnings.clone()
        }
    }
    impl State {
        fn warn(&mut self, warning: IoScratchDetail) {
            if !self.warnings.contains(&warning) {
                self.warnings.push(warning);
            }
        }
    }

    #[derive(Debug)]
    pub(crate) struct FileIoSetup {
        pub(crate) cwd: PathBuf,
        pub(crate) input: Option<StagedInput>,
        pub(crate) output: Option<OutputFileSpec>,
        pub(crate) destination: PathBuf,
        pub(crate) reservation: Option<Reservation>,
        pub(crate) owner: JobOwner,
        pub(crate) state: FileIoState,
    }
    #[derive(Debug)]
    pub(crate) struct Scratch {
        directory: crate::supervisor::sys::file_io::Directory,
        input: Option<OsString>,
        output: Option<OsString>,
        collected: bool,
        preparing: bool,
        reservation: Option<Reservation>,
        bytes: u64,
        _owner: JobOwner,
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            // Failed preparation is still owned by the creation thread.
            if self.preparing && self.reservation.is_some() {
                self.clear_input();
            }
            if let Some(mut reservation) = self.reservation.take() {
                let _ = reservation.commit(self.bytes);
            }
        }
    }
    impl Scratch {
        fn clear_input(&mut self) -> bool {
            if let Some(name) = &self.input {
                if self.directory.remove(name, true).is_err() {
                    return false;
                }
            }
            self.bytes = 0;
            drop(self.reservation.take());
            true
        }
        /// Called only on the post-run cwd worker, before measuring through the path.
        pub(crate) fn clear(&mut self, cwd: &std::path::Path, state: &FileIoState) -> (bool, u64) {
            if !self.clear_input() {
                state.warn(IoScratchDetail::StagedInputKept);
            }
            if self.collected {
                if let Some(name) = &self.output {
                    if self.directory.remove(name, false).is_err() {
                        state.warn(IoScratchDetail::OutputKept);
                    }
                }
            }
            let same = self.directory.matches(cwd);
            if !same {
                state.warn(IoScratchDetail::NotMeasured);
            }
            (same, self.bytes)
        }
    }

    #[derive(Debug)]
    struct Finished {
        scratch: Scratch,
        outcome: Option<OutputFile>,
    }
    #[derive(Debug)]
    pub(crate) struct FileCapture {
        state: FileIoState,
        scratch: Option<Scratch>,
        start: Option<mpsc::Sender<bool>>,
        result: mpsc::Receiver<Finished>,
        worker: Option<JoinHandle<()>>,
        abandoned: Arc<AtomicBool>,
        frozen: bool,
    }
    impl FileCapture {
        pub(in crate::supervisor) fn prepare(
            setup: FileIoSetup,
            credit: Option<Credits>,
            retained: Arc<AtomicU64>,
            creation: &crate::supervisor::creation::CreationState,
        ) -> Result<Self, NotStarted> {
            creation.check()?;
            let directory = crate::supervisor::sys::file_io::Directory::open(&setup.cwd)
                .map_err(NotStarted::Setup)?;
            let bytes = setup.reservation.as_ref().map_or(0, Reservation::bytes);
            let mut scratch = Scratch {
                directory,
                input: setup.input.as_ref().map(|i| i.name.clone()),
                output: setup.output.as_ref().map(|o| o.name.clone()),
                collected: false,
                preparing: true,
                reservation: setup.reservation,
                bytes,
                _owner: setup.owner,
            };
            if let Some(input) = setup.input {
                if let Err(e) = scratch.directory.stage(&input.name, &input.source, bytes) {
                    // Staging removed its own partial file; a kept one stays charged
                    // until the job's owner is released. Nothing else is touched.
                    scratch.preparing = false;
                    if !e.kept {
                        scratch.bytes = 0;
                        drop(scratch.reservation.take());
                    }
                    return Err(NotStarted::Setup(e.error));
                }
                if let Err(e) = creation.check() {
                    scratch.clear_input();
                    return Err(e);
                }
            }
            let (tx, result) = mpsc::channel();
            let abandoned = Arc::new(AtomicBool::new(false));
            let mut capture = Self {
                state: setup.state,
                scratch: None,
                start: None,
                result,
                worker: None,
                abandoned: abandoned.clone(),
                frozen: false,
            };
            if let Some(output) = setup.output {
                let (start, ready) = mpsc::channel();
                capture.start = Some(start);
                let capture_state = capture.state.clone();
                capture.worker = Some(
                    std::thread::Builder::new()
                        .name("cpf-output-file".into())
                        .spawn(move || {
                            let _credit = credit;
                            let eligible = match ready.recv() {
                                Ok(eligible) => eligible,
                                Err(_) => {
                                    scratch.clear_input();
                                    drop(scratch);
                                    return;
                                }
                            };
                            scratch.preparing = false;
                            let mut warnings = Vec::new();
                            let outcome = if eligible {
                                scratch.directory.collect(
                                    &output.name,
                                    output.budget,
                                    &setup.destination,
                                    &retained,
                                    &mut warnings,
                                )
                            } else {
                                OutputFile::NotAttempted
                            };
                            scratch.collected = matches!(outcome, OutputFile::Collected { .. });
                            let mut state = capture_state.lock();
                            for warning in warnings {
                                state.warn(warning);
                            }
                            if abandoned.load(Ordering::Acquire) {
                                drop(state);
                                // The physical worker owns partial-copy removal after a logical timeout.
                                if let OutputFile::Collected { bytes, .. } = &outcome {
                                    if std::fs::remove_file(&setup.destination).is_ok() {
                                        retained.fetch_sub(*bytes, Ordering::AcqRel);
                                    }
                                }
                                // H closes here, before the credit returns with this thread.
                                drop(scratch);
                                return;
                            }
                            let _ = tx.send(Finished {
                                scratch,
                                outcome: Some(outcome),
                            });
                            drop(state);
                        })
                        .map_err(|_| NotStarted::Worker)?,
                );
            } else {
                capture.scratch = Some(scratch);
            }
            Ok(capture)
        }

        pub(crate) fn poll(
            &mut self,
            outcome: &mut RootOutcome,
            cleanup_ready: bool,
            expired: bool,
        ) -> bool {
            if self.frozen {
                return true;
            }
            if !cleanup_ready {
                if !expired {
                    return false;
                }
                // No cleanup result by the deadline: nothing is collected. An
                // input-only scratch still goes to the post-run cwd/ step.
                let mut state = match self.state.0.try_lock() {
                    Ok(state) => state,
                    Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                    Err(std::sync::TryLockError::WouldBlock) => return false,
                };
                self.abandoned.store(true, Ordering::Release);
                if let Some(start) = self.start.take() {
                    let _ = start.send(false);
                    outcome.output_file = Some(OutputFile::NotAttempted);
                    state.warn(IoScratchDetail::NotMeasured);
                }
                if let Some(mut scratch) = self.scratch.take() {
                    scratch.preparing = false;
                    state.scratch = Some(scratch);
                }
                self.frozen = true;
                return true;
            }
            if let Some(start) = self.start.take() {
                let eligible = outcome.status != Termination::Unknown
                    && !matches!(outcome.termination.cleanup, Cleanup::Incomplete { .. })
                    && outcome.stop_trigger != Some(StopTrigger::Cancelled);
                let _ = start.send(eligible);
            }
            // Never wait for the worker: the cleanup poller calls this too.
            let mut state = match self.state.0.try_lock() {
                Ok(state) => state,
                Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return false,
            };
            let finished = self.result.try_recv().ok().or_else(|| {
                self.scratch.take().map(|scratch| Finished {
                    scratch,
                    outcome: None,
                })
            });
            if let Some(mut finished) = finished {
                finished.scratch.preparing = false;
                outcome.output_file = finished.outcome;
                state.scratch = Some(finished.scratch);
                self.frozen = true;
            } else if expired {
                self.abandoned.store(true, Ordering::Release);
                outcome.output_file = Some(OutputFile::NotCollected {
                    reason: CollectionFailure::Timeout,
                    os_error: None,
                });
                state.warn(IoScratchDetail::NotMeasured);
                self.frozen = true;
            }
            self.frozen
        }
        pub(crate) fn created(&mut self) {
            if let Some(scratch) = &mut self.scratch {
                scratch.preparing = false;
            }
        }
        /// On the creation thread when no process was created: the staged file
        /// is removed before the outcome is reported (E §5.6 handoff). The
        /// output-file worker is still waiting for its start, so the join is
        /// bounded by its own removal.
        pub(in crate::supervisor) fn abort_setup(&mut self) {
            drop(self.start.take());
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            if let Some(scratch) = &mut self.scratch {
                scratch.clear_input();
                scratch.preparing = false;
            }
        }
        pub(crate) fn released(&mut self) -> bool {
            if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                let _ = self.worker.take().unwrap().join();
            }
            self.worker.is_none()
        }
    }
    impl Drop for FileCapture {
        fn drop(&mut self) {
            self.start.take();
        }
    }
}
