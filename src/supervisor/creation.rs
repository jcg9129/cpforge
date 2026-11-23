//! Job-owned creation worker. Blocking setup never runs in an async poll.

/// Creation can be cancelled after commit without knowing whether a child exists.
#[derive(Debug)]
pub enum CreationError {
    NotStarted(super::NotStarted),
    Transferred,
}

impl From<super::NotStarted> for CreationError {
    fn from(reason: super::NotStarted) -> Self {
        Self::NotStarted(reason)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::CreationError;
    use crate::supervisor::creation_worker::CreationWorker;
    use crate::supervisor::{
        Admission, CancelToken, NotStarted, ProgramSpec, SpawnOptions, Spawned, cleanup, root,
        session,
    };
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::{Arc, mpsc};
    use tokio::runtime::Handle;
    use tokio::sync::oneshot::{self, error::TryRecvError};

    /// 0: Pending, 1: committed, 2: cancel published. A single atomic orders
    /// commit against cancellation, including the second commit of a pair.
    #[derive(Debug, Default)]
    pub(in crate::supervisor) struct CreationState(AtomicU8);

    pub(in crate::supervisor) enum Cancel {
        BeforeCommit,
        Committed,
    }

    impl CreationState {
        pub(in crate::supervisor) fn cancelled(&self) -> bool {
            self.0.load(Ordering::Acquire) == 2
        }

        pub(in crate::supervisor) fn check(&self) -> Result<(), NotStarted> {
            if self.cancelled() {
                Err(NotStarted::CancelledBeforeStart)
            } else {
                Ok(())
            }
        }

        pub(in crate::supervisor) fn commit(&self) -> bool {
            // A pair commits once per root. CAS 1 -> 1 for its second root still
            // arbitrates atomically with cancel, just like the first CAS 0 -> 1.
            let mut previous = self.0.load(Ordering::Acquire);
            loop {
                if previous == 2 {
                    return false;
                }
                match self
                    .0
                    .compare_exchange(previous, 1, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) => return true,
                    Err(value) => previous = value,
                }
            }
        }

        pub(in crate::supervisor) fn cancel(&self) -> Cancel {
            // Publishing cancel after commit also prevents a session's next
            // root from committing. Repeated cancellation conservatively keeps
            // the committed path, as does a failed Pending -> Cancelled CAS.
            if self.0.swap(2, Ordering::AcqRel) == 0 {
                Cancel::BeforeCommit
            } else {
                Cancel::Committed
            }
        }
    }

    struct CreationGuard(Arc<CreationState>);
    impl Drop for CreationGuard {
        fn drop(&mut self) {
            self.0.cancel();
        }
    }

    pub(in crate::supervisor) struct RootRequest {
        spec: ProgramSpec,
        admission: Admission,
        options: SpawnOptions,
        state: Arc<CreationState>,
        handle: Handle,
        reply: oneshot::Sender<Result<Spawned, NotStarted>>,
    }

    pub(in crate::supervisor) enum Request {
        Root(Box<RootRequest>),
        /// Owned engine setup, with no process creation before its reply.
        Prepare(Box<dyn FnOnce() + Send>),
        Session {
            spec: Box<session::SessionSpec>,
            state: Arc<CreationState>,
            handle: Handle,
            reply: oneshot::Sender<session::SessionStart>,
        },
    }

    /// One FIFO creation thread per job; its credit is held until it exits.
    #[derive(Debug)]
    pub struct Creator {
        sender: Option<mpsc::Sender<Request>>,
        worker: Option<CreationWorker<cleanup::Credits>>,
    }

    impl Drop for Creator {
        fn drop(&mut self) {
            // Closing the queue ends an idle worker. Blocking setup may still
            // be running: the cleanup poller joins it without delaying Drop.
            self.sender.take();
            if let Some(worker) = self.worker.take() {
                cleanup::transfer_creation(worker);
            }
        }
    }

    impl Creator {
        pub fn start() -> Result<Self, NotStarted> {
            cleanup::init().map_err(|_| NotStarted::Worker)?;
            let credit = cleanup::global_bounds()
                .expect("initialized bounds")
                .thread_credits
                .try_reserve(1)
                .ok_or(NotStarted::Worker)?;
            Self::with_credit(credit)
        }

        pub(crate) fn with_credit(credit: cleanup::Credits) -> Result<Self, NotStarted> {
            let (sender, receiver) = mpsc::channel();
            let worker = CreationWorker::spawn(credit, move || {
                while let Ok(request) = receiver.recv() {
                    match request {
                        Request::Prepare(work) => work(),
                        Request::Root(request) => {
                            let RootRequest {
                                spec,
                                admission,
                                options,
                                state,
                                handle,
                                reply,
                            } = *request;
                            let _enter = handle.enter();
                            #[cfg(feature = "storage-test-hooks")]
                            let _pipes = options
                                .fail_pipe_at
                                .map(crate::supervisor::PipeTestGuard::fail_at);
                            let result = state.check().and_then(|()| {
                                root::unix::spawn_with_state(spec, admission, &options, &state).map(
                                    |inner| Spawned {
                                        inner: Box::new(inner),
                                    },
                                )
                            });
                            // A closed receiver drops the root here, with the
                            // runtime entered, killing and transferring it.
                            let _ = reply.send(result);
                        }
                        Request::Session {
                            spec,
                            state,
                            handle,
                            reply,
                        } => {
                            let _enter = handle.enter();
                            #[cfg(feature = "storage-test-hooks")]
                            let _pipes = spec
                                .fail_pipe_at
                                .map(crate::supervisor::PipeTestGuard::fail_at);
                            let result = session::prepare_session(*spec, &state);
                            let _ = reply.send(result);
                        }
                    }
                }
            })
            .map_err(|_| NotStarted::Worker)?;
            Ok(Self {
                sender: Some(sender),
                worker: Some(worker),
            })
        }

        /// Run owned setup on this job's FIFO thread. Commit only after all
        /// blocking work returns, so cancellation never waits for setup I/O.
        pub(crate) async fn prepare<T: Send + 'static>(
            &self,
            cancel: &CancelToken,
            work: impl FnOnce() -> T + Send + 'static,
        ) -> Result<T, NotStarted> {
            if cancel.is_cancelled() {
                return Err(NotStarted::CancelledBeforeStart);
            }
            let state = Arc::new(CreationState::default());
            let worker_state = state.clone();
            let (reply, mut receiver) = oneshot::channel();
            self.send(Request::Prepare(Box::new(move || {
                let result = worker_state.check().and_then(|()| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                        .map_err(|_| NotStarted::Worker)
                });
                let result = if worker_state.commit() {
                    result
                } else {
                    Err(NotStarted::CancelledBeforeStart)
                };
                let _ = reply.send(result);
            })))?;
            let _guard = CreationGuard(state.clone());
            tokio::select! { biased;
                result = &mut receiver => result.map_err(|_| NotStarted::Worker)?,
                _ = cancel.cancelled() => match state.cancel() {
                    Cancel::BeforeCommit => Err(NotStarted::CancelledBeforeStart),
                    // Commit follows all blocking setup and immediately
                    // precedes the reply, so this await never waits for I/O.
                    Cancel::Committed => receiver.await.map_err(|_| NotStarted::Worker)?,
                },
            }
        }

        pub(in crate::supervisor) fn send(&self, request: Request) -> Result<(), NotStarted> {
            self.sender
                .as_ref()
                .expect("live creator")
                .send(request)
                .map_err(|_| NotStarted::Worker)
        }
    }

    pub(in crate::supervisor) async fn receive<T>(
        state: Arc<CreationState>,
        cancel: &CancelToken,
        mut receiver: oneshot::Receiver<T>,
    ) -> Result<T, CreationError> {
        let _guard = CreationGuard(state.clone());
        tokio::select! { biased;
            result = &mut receiver => result.map_err(|_| CreationError::NotStarted(NotStarted::Worker)),
            _ = cancel.cancelled() => {
                match state.cancel() {
                    Cancel::BeforeCommit => Err(NotStarted::CancelledBeforeStart.into()),
                    Cancel::Committed => match receiver.try_recv() {
                        // A reply that raced cancellation stays owned: the
                        // caller's wait observes the same token and kills.
                        Ok(result) => Ok(result),
                        // Dropping the receiver hands any eventual child to
                        // the worker's drop/transfer path, without waiting.
                        Err(TryRecvError::Empty) => Err(CreationError::Transferred),
                        Err(TryRecvError::Closed) => Err(NotStarted::Worker.into()),
                    },
                }
            }
        }
    }

    pub async fn create_root(
        creator: &Creator,
        spec: ProgramSpec,
        admission: Admission,
        opts: &SpawnOptions,
        cancel: &CancelToken,
    ) -> Result<Spawned, CreationError> {
        if cancel.is_cancelled() {
            return Err(NotStarted::CancelledBeforeStart.into());
        }
        let state = Arc::new(CreationState::default());
        let (reply, receiver) = oneshot::channel();
        creator.send(Request::Root(Box::new(RootRequest {
            spec,
            admission,
            options: opts.clone(),
            state: state.clone(),
            handle: Handle::current(),
            reply,
        })))?;
        receive(state, cancel, receiver).await?.map_err(Into::into)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::supervisor) use unix::{CreationState, Request, receive};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::{Creator, create_root};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Debug)]
pub struct Creator;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Creator {
    pub fn start() -> Result<Self, super::NotStarted> {
        Err(super::NotStarted::Worker)
    }
}
