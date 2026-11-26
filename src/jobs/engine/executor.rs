use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{ExecutionRecord, ExecutionSpec, JobFuture, Recipe, RunEvents, StartFailure};
use crate::build::{self, BuildExecutionOutcome, PendingBuild, ValidatedBuild};
use crate::exec::{DeferredStage, NativeProbe, ResolvedStage};
use crate::report::{NotStartedReason, PostBuild};
use crate::storage::budget::Reservation;
use crate::storage::job::JobOwner;
use crate::supervisor::CancelToken;

/// Move this guard to the physical resource owner, never merely to a finalized
/// record. A stalled writer still owns the full reservation and job lease.
#[derive(Debug)]
pub struct RunResources {
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        all(test, feature = "storage-test-hooks")
    ))]
    pub(crate) file_io: Option<Box<crate::supervisor::file_io::FileIoSetup>>,
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

/// Owned inputs for directory creation and per-run recipe resolution.
#[derive(Debug)]
pub struct RunPreparation {
    pub(super) directory: PathBuf,
    pub(super) staged: Vec<crate::jobs::roles::StagedFile>,
    pub(super) budget: Arc<crate::storage::budget::JobBudget>,
    pub(super) recipe: Option<PreparationRecipe>,
    // A cancelled caller may return while the creation thread still uses paths.
    pub(super) owner: JobOwner,
    #[cfg(feature = "storage-test-hooks")]
    pub(super) observation: Option<super::Observation>,
    #[cfg(feature = "storage-test-hooks")]
    pub(super) execution: u32,
}

#[derive(Debug)]
pub(super) enum PreparationRecipe {
    Stage(Recipe),
    Target(Box<build::StageRecipe>),
}

/// Keep directory writability separate from recipe resolution: a failed mkdir
/// retains its Setup record even when the recipe can still be resolved.
#[derive(Debug)]
pub struct PreparedRun {
    pub stage: Option<ResolvedStage>,
    pub failure: Option<StartFailure>,
    pub writable: bool,
    /// Cancellation stopped preparation before the stage was resolved: the
    /// report's `program` and `spec` are null (report §4.5).
    pub interrupted: bool,
    pub(super) scratch: Option<super::judge_files::Scratch>,
}

impl PreparedRun {
    fn failed(failure: StartFailure) -> Self {
        Self {
            stage: None,
            interrupted: matches!(failure.reason, NotStartedReason::CancelledBeforeStart),
            failure: Some(failure),
            scratch: None,
            writable: false,
        }
    }

    /// Cancelled before any run directory was created.
    pub(super) fn interrupted() -> Self {
        Self::failed(cancelled())
    }
}

impl RunPreparation {
    /// With a cancel token, stop at the next step boundary once it is set and
    /// report an interrupted `CancelledBeforeStart`; work that completes keeps
    /// its result, so the engine's own intent check decides as it does after
    /// inline setup.
    pub(super) fn prepare(self, cancel: Option<&CancelToken>) -> PreparedRun {
        let owner = self.owner;
        let stop = || cancel.is_some_and(CancelToken::is_cancelled);
        let mut result = PreparedRun {
            stage: None,
            failure: None,
            scratch: None,
            writable: true,
            interrupted: false,
        };
        #[cfg(feature = "storage-test-hooks")]
        if let Some(observer) = &self.observation {
            (observer.0)(super::Point::BeforeRunMkdir(self.execution));
        }
        // Never inspect, write or remove any part of a colliding run tree.
        for path in [
            &self.directory,
            &self.directory.join("cwd"),
            &self.directory.join("out"),
        ] {
            if stop() {
                return PreparedRun::interrupted();
            }
            if let Err(error) = fs::create_dir(path) {
                result.writable = false;
                result.failure = Some(StartFailure {
                    os_error: error.raw_os_error(),
                    ..setup_failure()
                });
                break;
            }
        }
        let judge = !self.staged.is_empty();
        if result.writable && judge {
            let mut scratch = super::judge_files::Scratch::new(owner.clone());
            if let Err(error) = scratch.stage(&self.staged, &self.budget, cancel) {
                result.failure = Some(StartFailure {
                    os_error: error.raw_os_error(),
                    ..setup_failure()
                });
            }
            result.scratch = Some(scratch);
        }
        let _owner = owner;
        let mut interrupted = false;
        result.stage = catch_unwind(AssertUnwindSafe(|| {
            if stop() {
                interrupted = true;
                return None;
            }
            let cwd = self.directory.join("cwd");
            match self.recipe? {
                PreparationRecipe::Target(recipe) => recipe.prepare(cwd).ok(),
                PreparationRecipe::Stage(recipe) => {
                    let mut stage = match recipe {
                        Recipe::Resolved(stage) => *stage,
                        Recipe::Deferred(stage) => {
                            #[cfg(feature = "storage-test-hooks")]
                            if let Some(observer) = &self.observation {
                                (observer.0)(super::Point::BeforeRecipeCompletion(self.execution));
                            }
                            if stop() {
                                interrupted = true;
                                return None;
                            }
                            stage.complete(&NativeProbe).ok()?
                        }
                        Recipe::TargetRun => return None,
                    };
                    if judge {
                        let args = match &mut stage.command {
                            crate::exec::ResolvedCommand::Argv { args, .. }
                            | crate::exec::ResolvedCommand::Shell { args, .. } => args,
                        };
                        // Appended profile paths were planned before admission.
                        // Run ids count admissions, so skipped slots require a
                        // component-wise rebase into this run's actual cwd.
                        for arg in args {
                            if let Ok(relative) = std::path::Path::new(arg).strip_prefix(&stage.cwd)
                            {
                                *arg = relative
                                    .components()
                                    .fold(cwd.clone(), |p, c| p.join(c))
                                    .into();
                            }
                        }
                    }
                    if !stage.shared_cwd {
                        stage.cwd = cwd;
                    }
                    Some(stage)
                }
            }
        }))
        .ok()
        .flatten();
        if interrupted {
            // No judge will run: remove staged copies on this worker now, so
            // a reply that cancellation discards cannot strand them in cwd/.
            if let Some(scratch) = &mut result.scratch {
                let _ = scratch.clear();
            }
            // Cancellation outranks a directory failure here, so the record
            // matches the null program and spec; a fully created tree stays
            // writable for the cancellation record.
            return PreparedRun {
                stage: None,
                failure: Some(cancelled()),
                scratch: result.scratch,
                writable: result.writable,
                interrupted: true,
            };
        }
        result
    }
}

fn setup_failure() -> StartFailure {
    StartFailure {
        reason: NotStartedReason::Setup,
        api: None,
        os_error: None,
        invalid_limit: None,
    }
}

fn cancelled() -> StartFailure {
    StartFailure {
        reason: NotStartedReason::CancelledBeforeStart,
        ..setup_failure()
    }
}

/// Finalized post-build observations and the resulting consumer capability.
#[derive(Debug)]
pub struct BuildCompletion {
    pub post: PostBuild,
    pub validated: Option<ValidatedBuild>,
}

fn complete_build(
    validation: Result<ValidatedBuild, build::BuildFailure>,
    generated: Vec<DeferredStage>,
) -> BuildCompletion {
    let mut post = PostBuild::from_validation(validation.as_ref());
    let mut validated = validation.ok();
    if let Some(build) = &validated {
        for generated in generated {
            if let Err(error) = generated.complete(&NativeProbe) {
                post = PostBuild::from_failure_kind(
                    &build::BuildFailureKind::GeneratedProgramNotLaunchable(error),
                    build.output_root(),
                );
                validated = None;
                break;
            }
        }
    }
    BuildCompletion { post, validated }
}

/// Platform boundary. The future returns finalized evidence and must transfer
/// resources to cleanup before returning when physical work remains. Futures
/// need not be Send: a ready job's planning probe is thread-bound.
pub trait Executor {
    /// Fakes may prepare inline; the supervisor uses the job's creation thread.
    fn prepare_run(
        &mut self,
        preparation: RunPreparation,
        _cancel: CancelToken,
    ) -> JobFuture<'_, PreparedRun> {
        // Preserve the original inline setup behaviour for fake executors.
        Box::pin(async move { preparation.prepare(None) })
    }

    /// Consume the build only on the setup worker in production. Inline fakes
    /// retain the planning probe, which need not be Send or Sync.
    fn finish_build<'a>(
        &'a mut self,
        pending: PendingBuild<'a>,
        outcome: BuildExecutionOutcome,
        generated: Vec<DeferredStage>,
        owner: JobOwner,
    ) -> JobFuture<'a, Result<BuildCompletion, StartFailure>> {
        Box::pin(async move {
            let _owner = owner;
            Ok(complete_build(
                build::finish_build(pending, outcome),
                generated,
            ))
        })
    }

    /// Judge evidence and scratch removal (E §2.1 steps 1-2) when no post-run
    /// worker could be admitted: run on the job's creation thread, waiting no
    /// later than `deadline` (`cwd_unmeasured_timeout` past it).
    fn finish_judge(
        &mut self,
        work: Box<dyn FnOnce() + Send>,
        _deadline: tokio::time::Instant,
    ) -> JobFuture<'_, Result<(), crate::report::RunNote>> {
        Box::pin(async move {
            work();
            Ok(())
        })
    }

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
    /// Reserve and start a creation thread only (callers supply persistence).
    pub fn start() -> Result<Self, crate::supervisor::NotStarted> {
        Ok(Self::with_creator(crate::supervisor::Creator::start()?))
    }

    /// Job start: reserve the creation and persistence credits in one step,
    /// then start creation. Returns the persistence worker's credit; on
    /// failure no credit stays reserved (execution contract §3.3).
    pub fn start_job() -> Result<(Self, crate::supervisor::Credits), crate::supervisor::NotStarted>
    {
        let [creation, persistence] = crate::supervisor::job_credits()?;
        let creator = crate::supervisor::Creator::with_credit(creation)?;
        Ok((Self::with_creator(creator), persistence))
    }

    fn with_creator(creator: crate::supervisor::Creator) -> Self {
        Self {
            creator: Arc::new(creator),
            options: crate::supervisor::SpawnOptions::default(),
        }
    }

    /// Connected sessions share this job's FIFO creation thread.
    pub fn creator(&self) -> Arc<crate::supervisor::Creator> {
        self.creator.clone()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Executor for SupervisorExecutor {
    fn prepare_run(
        &mut self,
        preparation: RunPreparation,
        cancel: CancelToken,
    ) -> JobFuture<'_, PreparedRun> {
        Box::pin(async move {
            let worker_cancel = cancel.clone();
            match self
                .creator
                .prepare(&cancel, move || preparation.prepare(Some(&worker_cancel)))
                .await
            {
                Ok(prepared) => prepared,
                Err(reason) => PreparedRun::failed(start_failure(reason)),
            }
        })
    }

    fn finish_judge(
        &mut self,
        work: Box<dyn FnOnce() + Send>,
        deadline: tokio::time::Instant,
    ) -> JobFuture<'_, Result<(), crate::report::RunNote>> {
        Box::pin(async move {
            use crate::report::RunNote;
            // Own token: removal is attempted even after cancellation, and the
            // deadline bounds the wait. Past it the creation thread keeps the
            // work and its accounting until it returns (or drops it unstarted).
            let token = CancelToken::new();
            match tokio::time::timeout_at(deadline, self.creator.prepare(&token, work)).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(RunNote::CwdUnmeasuredIo),
                Err(_) => Err(RunNote::CwdUnmeasuredTimeout),
            }
        })
    }

    fn finish_build<'a>(
        &'a mut self,
        pending: PendingBuild<'a>,
        outcome: BuildExecutionOutcome,
        generated: Vec<DeferredStage>,
        owner: JobOwner,
    ) -> JobFuture<'a, Result<BuildCompletion, StartFailure>> {
        let finish = pending.native_finisher();
        Box::pin(async move {
            // Job cancellation does not interrupt validation: an exited-zero
            // build's result is its post-build check (report §2.3). The wait
            // is on the creation thread, so the runtime stays responsive.
            self.creator
                .prepare(&CancelToken::new(), move || {
                    let _owner = owner;
                    complete_build(finish(outcome), generated)
                })
                .await
                .map_err(start_failure)
        })
    }

    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        cancel: CancelToken,
        events: RunEvents,
        mut resources: RunResources,
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
            admission.file_io = resources.file_io.take();
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
                Err(supervisor::CreationError::NotStarted(reason)) => not_started(reason),
                Err(supervisor::CreationError::Transferred) => ExecutionRecord::Transferred,
            }
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn not_started(reason: crate::supervisor::NotStarted) -> ExecutionRecord {
    ExecutionRecord::NotStarted(start_failure(reason))
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    all(test, feature = "storage-test-hooks")
))]
pub(super) fn start_failure(reason: crate::supervisor::NotStarted) -> StartFailure {
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
    failure
}
