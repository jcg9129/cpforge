use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{ExecutionRecord, ExecutionSpec, JobFuture, RunEvents};
use crate::storage::budget::Reservation;
use crate::storage::job::JobOwner;
use crate::supervisor::CancelToken;

/// Move this guard to the physical resource owner, never merely to a finalized
/// record. A stalled writer still owns the full reservation and job lease.
#[derive(Debug)]
pub struct RunResources {
    pub(super) reservation: Reservation,
    pub(super) _owner: JobOwner,
    pub(super) retained_bytes: Arc<AtomicU64>,
}
impl RunResources {
    /// File sinks increment this after each successful write, including writes
    /// after record finalization. Release the guard only after all writers end.
    pub fn retained_counter(&self) -> Arc<AtomicU64> {
        self.retained_bytes.clone()
    }
}

impl Drop for RunResources {
    fn drop(&mut self) {
        let bytes = self.retained_bytes.load(Ordering::Acquire);
        let _ = self.reservation.commit(bytes.min(self.reservation.bytes()));
    }
}

/// Platform boundary. The future returns finalized evidence and must transfer
/// resources to cleanup before returning when physical work remains. Futures
/// need not be Send: a ready job's planning probe is thread-bound.
pub trait Executor {
    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        cancel: CancelToken,
        events: RunEvents,
        resources: RunResources,
    ) -> JobFuture<'_, ExecutionRecord>;
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug)]
pub struct SupervisorExecutor {
    creator: Arc<crate::supervisor::Creator>,
    pub options: crate::supervisor::SpawnOptions,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl SupervisorExecutor {
    /// Reserve and start the job's creation thread before any run is admitted.
    pub fn start() -> Result<Self, crate::supervisor::NotStarted> {
        Ok(Self {
            creator: Arc::new(crate::supervisor::Creator::start()?),
            options: crate::supervisor::SpawnOptions::default(),
        })
    }

    /// Connected sessions share this job's FIFO creation thread.
    pub fn creator(&self) -> Arc<crate::supervisor::Creator> {
        self.creator.clone()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Executor for SupervisorExecutor {
    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        cancel: CancelToken,
        events: RunEvents,
        resources: RunResources,
    ) -> JobFuture<'_, ExecutionRecord> {
        use crate::supervisor::{
            self, OutputSpec, ProgramSpec, RunControls, Sink, StdinSpec, StreamPolicy,
        };
        Box::pin(async move {
            #[cfg(feature = "storage-test-hooks")]
            if spec.stage == super::Stage::Hook {
                crate::storage::crash::panic_point("hook.execute");
            }
            if cancel.is_cancelled() {
                return not_started(supervisor::NotStarted::CancelledBeforeStart);
            }
            let mut admission = match supervisor::admit() {
                Ok(admission) => admission,
                Err(reason) => return not_started(reason),
            };
            let retained_bytes = resources.retained_bytes.clone();
            admission.hold_resources(resources, retained_bytes);
            let raw = matches!(spec.role, super::Role::Raw);
            let output = events.output;
            let capture = |name: &str, cap: &crate::config::StreamCap| {
                OutputSpec::Capture(StreamPolicy {
                    sink: if raw {
                        Sink::Forward(if name == "stdout" {
                            supervisor::Stream::Stdout
                        } else {
                            supervisor::Stream::Stderr
                        })
                    } else {
                        Sink::File(spec.directory.join("out").join(name))
                    },
                    budget: cap.bytes.value,
                    on_exceed: super::exceed(cap.policy),
                    events: output,
                })
            };
            let events = output.then(|| {
                supervisor::EventSender::forwarding(move |stream, bytes| {
                    events.output(stream, bytes);
                })
            });
            let mut limits = Vec::new();
            if let Some(value) = &spec.resolved.limits.address_space {
                limits.push(supervisor::LocalLimit::AddressSpace { bytes: value.value });
            }
            if let Some(value) = &spec.resolved.limits.job_commit {
                limits.push(supervisor::LocalLimit::JobCommit { bytes: value.value });
            }
            let program = ProgramSpec {
                program: spec.command.program.clone(),
                args: spec.command.args.clone(),
                cwd: spec.resolved.cwd.clone(),
                env: spec.resolved.environment.clone(),
                stdin: if raw {
                    StdinSpec::Forward
                } else {
                    spec.stdin.clone().map_or(StdinSpec::Null, StdinSpec::File)
                },
                stdout: capture("stdout", &spec.resolved.stdout),
                stderr: capture("stderr", &spec.resolved.stderr),
                limits,
                measure: vec![
                    supervisor::MetricKind::PeakRss,
                    supervisor::MetricKind::CpuTime,
                ],
            };
            match supervisor::create_root(&self.creator, program, admission, &self.options, &cancel)
                .await
            {
                Ok(root) => ExecutionRecord::Started(Box::new(
                    supervisor::wait_root(
                        root,
                        RunControls {
                            wall_deadline: std::time::Duration::from_millis(
                                spec.resolved.deadline.milliseconds.value,
                            ),
                            cancel,
                            events,
                        },
                    )
                    .await,
                )),
                Err(reason) => not_started(reason),
            }
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn not_started(reason: crate::supervisor::NotStarted) -> ExecutionRecord {
    use crate::report::NotStartedReason as R;
    use crate::supervisor::NotStarted as N;
    let mut failure = super::StartFailure {
        reason: R::Worker,
        api: None,
        os_error: None,
        invalid_limit: None,
    };
    failure.reason = match reason {
        N::Unsupported => R::Unsupported,
        N::CancelledBeforeStart => R::CancelledBeforeStart,
        N::Worker => R::Worker,
        N::CleanupCapacity => R::CleanupCapacity,
        N::UnsupportedLimit => R::UnsupportedLimit,
        N::InvalidLimit(validation) => {
            failure.invalid_limit = Some(validation);
            R::UnsupportedLimit
        }
        N::Limit(error) => {
            failure.os_error = Some(error);
            R::Limit
        }
        N::CreateFailed { api, raw_error } => {
            failure.api = Some(api);
            failure.os_error = Some(raw_error);
            R::CreateFailed
        }
        N::Setup(error) => {
            failure.os_error = error.raw_os_error();
            R::Setup
        }
    };
    ExecutionRecord::NotStarted(failure)
}
