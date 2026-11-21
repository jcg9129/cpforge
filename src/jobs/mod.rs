//! Managed job preparation: the only transition from a registered job
//! (storage contract §6.2) to a ready job from which runs can be started.
//!
//! [`prepare_job`] takes a [`RegisteredJob`], the job's complete
//! [`FrozenInputs`] and the selected [`TargetPlan`], and returns a
//! [`ReadyJob`]. It consumes the registration even on failure; partial storage
//! is then reclaimed by ordinary pruning once the last lease owner drops.
//! [`prepare_build_job`] freezes an empty selected-test set plus the target's
//! captured files and calls [`prepare_job`], as a build-only command needs.
//!
//! Preparation refuses frozen inputs of another job directory, inputs whose
//! captured declarations (paths, roles, order) differ from the target, and a
//! registration whose input freeze has failed. There is no parameter through
//! which original source paths can be supplied: every `{sources}`/`{source}`
//! binding comes from the captured copies (storage contract §7). Preparation
//! creates `<job>/builds/` as the build area and `<job>/build/cwd` as the build
//! stage cwd (or `<job>/run/cwd`, the run-depth cwd against which the run
//! stage of a build-free target is checked; later runs must use
//! `<job>/<run-id>/cwd` at the same depth), plans the
//! target and applies the source capture accident guard ([`guard`]) to the
//! source-consuming stage: the build stage, or the run stage of a build-free
//! target. A guard failure is an error; a passing argv stage yields
//! [`CaptureCheck::Checked`] and a passing shell stage
//! `CaptureCheck::Unchecked { reasons: [ShellStage] }`. The guard is an accident
//! check, not isolation.
//!
//! Callers keep the ready job's owner (or a clone) alive through execution,
//! reporting and transferred cleanup, obtain source bindings for any further
//! job stage from [`ReadyJob::sources`], and allocate every consumer cwd below
//! [`ReadyJob::dir`].

pub mod engine;
pub mod guard;
pub mod persist;

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::build::{self, BuildArea, BuildId, PlanInputs, PlannedTarget};
use crate::config::{Platform, TargetPlan};
use crate::exec::{Baseline, InvocationDirs, Probe, ProblemBinding};
use crate::storage::admission::{AdmissionReport, RegisteredJob};
use crate::storage::budget::JobBudget;
use crate::storage::inputs::{CapturedSource, CapturedSources, FreezeInputsError, FrozenInputs};
use crate::storage::job::{Epoch, JobId, JobOwner};
use crate::storage::{StorageError, checked_path};
use crate::tests_store::FrozenManifest;

pub use guard::{CaptureCheck, CaptureCheckReason, GuardError};
pub(crate) mod facts;

/// No source-path parameter: managed planning always obtains bindings from the
/// completed captured sources. Cwds and build allocations are job-owned.
/// `workspace_root` must use the same spelling as the root the job was
/// registered under, so that the job directory lies lexically inside it.
pub struct PrepareJobInputs<'probe> {
    pub workspace_root: PathBuf,
    pub problem: ProblemBinding,
    pub report: engine::ReportPresentation,
    pub baseline: Baseline,
    pub platform: Platform,
    pub build_bytes: u64,
    pub probe: &'probe dyn Probe,
}

impl fmt::Debug for PrepareJobInputs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrepareJobInputs")
            .field("workspace_root", &self.workspace_root)
            .field("problem", &self.problem)
            .field("report", &self.report)
            .field("baseline", &self.baseline)
            .field("platform", &self.platform)
            .field("build_bytes", &self.build_bytes)
            .finish_non_exhaustive()
    }
}

/// Constructible only after complete freezing and guarded target planning.
#[derive(Debug)]
#[non_exhaustive]
pub struct ReadyJob<'probe> {
    pub owner: JobOwner,
    pub workspace: PathBuf,
    pub problem: ProblemBinding,
    pub report: engine::ReportPresentation,
    pub target: TargetPlan,
    pub platform: Platform,
    pub id: JobId,
    pub started_at: std::time::SystemTime,
    pub started: std::time::Instant,
    pub dir: PathBuf,
    pub sequence: (Epoch, u64),
    pub manifest: FrozenManifest,
    /// Configured source order followed by support order, for fingerprints.
    pub sources: CapturedSources,
    pub capture_check: CaptureCheck,
    pub capture_report: engine::CaptureReport,
    pub planned_target: PlannedTarget<'probe>,
    pub budget: Arc<JobBudget>,
    pub prune_report: AdmissionReport,
}

/// Engine inputs shared by target jobs and commands that need no target.
/// Only preparation can construct this capability.
#[derive(Debug)]
pub struct ReadyExecutionJob<'probe> {
    pub(crate) owner: JobOwner,
    pub(crate) workspace: PathBuf,
    pub(crate) problem: Option<ProblemBinding>,
    pub(crate) report: Option<engine::ReportPresentation>,
    pub(crate) target: Option<TargetPlan>,
    pub(crate) platform: Platform,
    pub(crate) id: JobId,
    pub(crate) started_at: std::time::SystemTime,
    pub(crate) started: std::time::Instant,
    pub(crate) dir: PathBuf,
    pub(crate) sequence: (Epoch, u64),
    pub(crate) manifest: FrozenManifest,
    pub(crate) capture_report: Option<engine::CaptureReport>,
    pub(crate) planned_target: Option<PlannedTarget<'probe>>,
    pub(crate) budget: Arc<JobBudget>,
    pub(crate) prune_report: AdmissionReport,
    pub(crate) cwd_bytes: Option<u64>,
}

impl<'probe> From<ReadyJob<'probe>> for ReadyExecutionJob<'probe> {
    fn from(ready: ReadyJob<'probe>) -> Self {
        Self {
            owner: ready.owner,
            workspace: ready.workspace,
            problem: Some(ready.problem),
            report: Some(ready.report),
            target: Some(ready.target),
            platform: ready.platform,
            id: ready.id,
            started_at: ready.started_at,
            started: ready.started,
            dir: ready.dir,
            sequence: ready.sequence,
            manifest: ready.manifest,
            capture_report: Some(ready.capture_report),
            planned_target: Some(ready.planned_target),
            budget: ready.budget,
            prune_report: ready.prune_report,
            cwd_bytes: None,
        }
    }
}

/// Freeze a command's selected tests without resolving or capturing a target.
/// As with target preparation, the workspace lock has already been released.
pub(crate) fn prepare_command_job(
    registered: RegisteredJob,
    workspace: PathBuf,
    selected: Option<(ProblemBinding, engine::ReportPresentation)>,
    tests: &[crate::names::Identifier],
) -> Result<ReadyExecutionJob<'static>, ReadyError> {
    let manifest = if tests.is_empty() {
        FrozenManifest::empty()
    } else {
        let (problem, _) = selected
            .as_ref()
            .expect("test selection requires a problem");
        crate::tests_store::TestStore::new(problem.dir.clone())
            .freeze_into(tests, &registered.inputs())
            .map_err(|error| ReadyError::Freeze(FreezeInputsError::Tests(error)))?
    };
    registered
        .inputs()
        .ensure_active()
        .map_err(|_| ReadyError::FailedInputs)?;
    let (problem, report) = selected.map_or((None, None), |(p, r)| (Some(p), Some(r)));
    Ok(ReadyExecutionJob {
        owner: registered.owner,
        workspace,
        problem,
        report,
        target: None,
        platform: Platform::host(),
        id: registered.id,
        started_at: registered.started_at,
        started: registered.started,
        dir: registered.dir,
        sequence: registered.sequence,
        manifest,
        capture_report: None,
        planned_target: None,
        budget: registered.budget,
        prune_report: registered.prune_report,
        cwd_bytes: None,
    })
}

#[derive(Debug)]
pub enum ReadyError {
    WrongJob {
        registered: PathBuf,
        frozen: PathBuf,
    },
    FailedInputs,
    /// Capture declarations must match this target, including roles and order.
    WrongTarget,
    Freeze(FreezeInputsError),
    Storage(StorageError),
    Plan(build::PlanErrors),
    Guard(GuardError),
}
impl fmt::Display for ReadyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongJob { registered, frozen } => write!(
                f,
                "frozen job {} differs from registered job {}",
                frozen.display(),
                registered.display()
            ),
            Self::FailedInputs => f.write_str("this job's input freeze has failed"),
            Self::WrongTarget => {
                f.write_str("captured file declarations differ from the selected target")
            }
            Self::Freeze(error) => write!(f, "{error}"),
            Self::Storage(error) => write!(f, "{error}"),
            Self::Plan(error) => write!(f, "{error}"),
            Self::Guard(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for ReadyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Freeze(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::Plan(error) => Some(error),
            Self::Guard(error) => Some(error),
            _ => None,
        }
    }
}

/// Consume registration on success and failure. Failure exposes no planned
/// execution capability; partial storage is left for ordinary inactive pruning.
pub fn prepare_job<'probe>(
    registered: RegisteredJob,
    frozen: FrozenInputs,
    target: &TargetPlan,
    inputs: PrepareJobInputs<'probe>,
) -> Result<ReadyJob<'probe>, ReadyError> {
    if frozen.job_dir() != registered.dir.as_path() {
        return Err(ReadyError::WrongJob {
            registered: registered.dir.clone(),
            frozen: frozen.job_dir().to_owned(),
        });
    }
    registered
        .inputs()
        .ensure_active()
        .map_err(|_| ReadyError::FailedInputs)?;
    let declarations = CapturedSource::from_target(target);
    if !declarations
        .iter()
        .map(|file| (&file.role, &file.problem_path))
        .eq(frozen
            .sources()
            .files()
            .iter()
            .map(|file| (&file.role, &file.problem_path)))
    {
        return Err(ReadyError::WrongTarget);
    }
    let create = |name: &str| -> Result<PathBuf, ReadyError> {
        let path = checked_path(&registered.dir, std::path::Path::new(name), false)
            .map_err(ReadyError::Storage)?;
        fs::create_dir(&path)
            .map_err(|error| ReadyError::Storage(StorageError::io(&path, error)))?;
        Ok(path)
    };
    // Every run directory sits at `<job>/<run>/cwd` (storage contract §6.1).
    // The accident guard resolves relative operands lexically against the
    // stage cwd, so the cwd it checks must have the depth of the cwd the
    // stage really uses: the build runs in `<job>/build/cwd`, and the run
    // stage of a build-free target is checked at `<job>/run/cwd`, the depth
    // of every later `<job>/<run-id>/cwd`.
    let stage_dir = if target.build.is_some() {
        "build"
    } else {
        "run"
    };
    create(stage_dir)?;
    let cwd = create(&format!("{stage_dir}/cwd"))?;
    let area = BuildArea::new(create("builds")?).map_err(ReadyError::Storage)?;
    let outputs = if target.build.is_some() {
        let id = BuildId::new("r00000").expect("first build admission id");
        Some(build::allocate(&area, id).map_err(ReadyError::Storage)?)
    } else {
        None
    };
    let planned_target = build::plan_target(
        target,
        PlanInputs {
            sources: frozen.sources().source_bindings(),
            dirs: InvocationDirs {
                run_cwd: cwd.clone(),
                problem: Some(inputs.problem.clone()),
                workspace: Some(inputs.workspace_root.clone()),
            },
            baseline: inputs.baseline,
            platform: inputs.platform,
            build_bytes: inputs.build_bytes,
            probe: inputs.probe,
        },
        outputs,
    )
    .map_err(ReadyError::Plan)?;
    let (stage, arguments, stage_cwd) = match &planned_target {
        PlannedTarget::Build(pending) => (
            target.build.as_ref().expect("build stage"),
            pending.expanded_arguments(),
            pending.build_stage().cwd.clone(),
        ),
        PlannedTarget::BuildFree(run) => (
            &target.run,
            run.expanded_arguments(),
            run.run(cwd)
                .map_err(|error| {
                    ReadyError::Plan(build::PlanErrors {
                        errors: vec![build::PlanError::Prepare(error)],
                    })
                })?
                .cwd,
        ),
    };
    let capture_check = guard::check_capture(
        stage,
        arguments,
        &stage_cwd,
        &inputs.workspace_root,
        &registered.dir,
        inputs.platform,
    )
    .map_err(ReadyError::Guard)?;
    let (manifest, sources) = frozen.into_parts();
    let mut capture_report =
        engine::CaptureReport::from_sources(capture_check.clone(), sources.files());
    let lists = [
        crate::storage::inputs::SourceRole::Source,
        crate::storage::inputs::SourceRole::Support,
    ]
    .map(|role| {
        sources
            .files()
            .iter()
            .filter(|file| file.role == role)
            .map(|file| file.problem_path.components().iter().collect::<PathBuf>())
            .collect::<Vec<_>>()
    });
    let bytes = crate::report::wire::encode_capture_manifest(
        persist::platform(),
        &registered.id,
        &capture_check,
        &lists[0],
        &lists[1],
        &capture_report.lists_sha256,
    )
    .map_err(|error| {
        ReadyError::Storage(StorageError::io(
            &registered.dir,
            std::io::Error::other(error.to_string()),
        ))
    })?;
    let allowance = (bytes.len() as u64).checked_mul(2).ok_or_else(|| {
        ReadyError::Storage(StorageError::io(
            &registered.dir,
            std::io::Error::other("capture manifest size overflow"),
        ))
    })?;
    let mut reservation = registered.budget.reserve(allowance).map_err(|error| {
        ReadyError::Storage(StorageError::io(
            &registered.dir,
            std::io::Error::other(error.to_string()),
        ))
    })?;
    let guard = crate::storage::WorkspaceGuard::leased(&registered.owner);
    guard
        .created
        .borrow_mut()
        .push(registered.dir.join("inputs"));
    let published = crate::storage::publish_file_noclobber(&guard, "inputs/capture.json", &bytes);
    persist::account(&mut reservation, bytes.len() as u64, &published);
    let published = published.map_err(ReadyError::Storage)?;
    drop(guard);
    capture_report.manifest = Some("inputs/capture.json".into());
    let mut prune_report = registered.prune_report;
    prune_report
        .publications
        .push((registered.dir.join("inputs/capture.json"), published));

    Ok(ReadyJob {
        owner: registered.owner,
        workspace: inputs.workspace_root,
        problem: inputs.problem,
        report: inputs.report,
        target: target.clone(),
        platform: inputs.platform,
        id: registered.id,
        started_at: registered.started_at,
        started: registered.started,
        dir: registered.dir,
        sequence: registered.sequence,
        manifest,
        sources,
        capture_check,
        capture_report,
        planned_target,
        budget: registered.budget,
        prune_report,
    })
}

/// Build and raw run capture only the target, with an empty selected-test set.
pub fn prepare_build_job<'probe>(
    registered: RegisteredJob,
    target: &TargetPlan,
    inputs: PrepareJobInputs<'probe>,
) -> Result<ReadyJob<'probe>, ReadyError> {
    let frozen = registered
        .inputs()
        .freeze_sources(&CapturedSource::from_target(target), &inputs.problem.dir)
        .map_err(ReadyError::Freeze)?;
    prepare_job(registered, frozen, target, inputs)
}
