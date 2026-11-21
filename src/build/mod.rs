//! Per-attempt build allocation, target preflight and permission to run.
//!
//! The caller owns the build area (normally `<job>/builds/`), captures sources,
//! supplies fresh execution cwds, and preflights the job's other stages before
//! launching anything. This module neither admits jobs nor captures sources.
//! [`PendingBuild`] exposes only its producer; [`finish_build`] consumes it and
//! grants access to the consumer only after outcome and artifact validation.
//! Build-free targets have an explicit, independently preflighted path.
//!
//! Identity belongs to an attempt, not to source contents. A caller must keep
//! the latest attempt's [`BuildId`] in its job and match a validated build's id
//! against it. Retaining an earlier successful build never changes the id of a
//! later failure. There is no API to relabel or attach outputs to another id.
//! The filesystem stability assumptions of storage §1 still apply: validation
//! does not make outputs immutable or prove which inputs a compiler read.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{Artifacts, Platform, TargetPlan, syntax};
use crate::exec::{
    self, Baseline, Context, InvocationDirs, PrepareErrors, PreparedCommand, Probe, Resolution,
    ResolveErrors, ResolvedStage, TargetBindings,
};
use crate::names::{Identifier, NameError};
use crate::report::{self, PostBuild, RunClass, RunInput, StreamEvidence};
use crate::storage::{self, StorageError, WalkReport};
use crate::supervisor::{self, RootOutcome, StopTrigger, SupervisorError, Termination, Violation};

/// Opaque portable attempt identity, supplied by the caller or randomly generated.
/// Uses the crate's portable identifier policy (1–64 ASCII bytes). Callers must
/// choose a new id for every attempt, including after deleting an old directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BuildId(Identifier);

impl BuildId {
    pub fn new(value: impl Into<String>) -> Result<Self, NameError> {
        Identifier::new(value).map(Self)
    }

    pub fn generate() -> io::Result<Self> {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self::new(format!("{:032x}", u128::from_le_bytes(bits))).expect("hex identifier"))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for BuildId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// An existing caller-owned directory. Its canonical path is the storage anchor;
/// no workspace lock, job discovery, lease or directory creation is implied.
#[derive(Debug)]
pub struct BuildArea {
    root: PathBuf,
}

impl BuildArea {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        let metadata = fs::symlink_metadata(path).map_err(|e| StorageError::io(path, e))?;
        if storage::is_link(&metadata) {
            return Err(StorageError::new(storage::ErrorKind::LinkInPath {
                path: path.into(),
            }));
        }
        if !metadata.is_dir() {
            return Err(StorageError::new(storage::ErrorKind::InvalidPath {
                path: path.into(),
            }));
        }
        let root = fs::canonicalize(path)
            .map(|path| storage::paths::plain(&path))
            .map_err(|e| StorageError::io(path, e))?;
        Ok(Self { root })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

/// Exclusive fresh allocation. Deliberately neither constructible from an
/// existing path nor cloneable. Planning consumes this capability exactly once.
#[derive(Debug)]
pub struct FreshBuildOutputs {
    id: BuildId,
    area: PathBuf,
    root: PathBuf,
}

impl FreshBuildOutputs {
    pub fn id(&self) -> &BuildId {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

/// Create the final component exclusively; any existing file, directory or
/// link fails. Existing components below the area are never followed.
pub fn allocate(area: &BuildArea, id: BuildId) -> Result<FreshBuildOutputs, StorageError> {
    let root = storage::checked_path(&area.root, Path::new(id.as_str()), false)?;
    fs::create_dir(&root).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            StorageError::new(storage::ErrorKind::Exists { path: root.clone() })
        } else {
            StorageError::io(&root, error)
        }
    })?;
    Ok(FreshBuildOutputs {
        id,
        area: area.root.clone(),
        root,
    })
}

#[derive(Debug)]
struct ArtifactLayout {
    file: Option<PathBuf>,
    required: Vec<PathBuf>,
}

impl ArtifactLayout {
    fn new(
        root: &Path,
        artifacts: Option<&Artifacts>,
        platform: Platform,
    ) -> Result<Self, PlanError> {
        // C1 validated output-relative names for its platform; a plan checked
        // for another platform is refused here. Join components explicitly so
        // containment does not depend on the host's separator interpretation.
        let output = |name: &str| {
            if !syntax::output_relative(name, platform) {
                return Err(PlanError::InvalidArtifactName(name.to_owned()));
            }
            Ok(name
                .split(|c| c == '/' || (platform == Platform::Windows && c == '\\'))
                .fold(root.to_path_buf(), |path, part| {
                    syntax::join(&path, OsStr::new(part), platform)
                }))
        };
        match artifacts {
            Some(Artifacts::File { name }) => {
                let mut name = name.value.clone();
                // Only the final component's extension counts, on any host.
                if platform == Platform::Windows
                    && Path::new(name.rsplit(['/', '\\']).next().expect("nonempty name"))
                        .extension()
                        .is_none()
                {
                    name.push_str(".exe");
                }
                let file = output(&name)?;
                Ok(Self {
                    required: vec![file.clone()],
                    file: Some(file),
                })
            }
            Some(Artifacts::Dir { required }) => Ok(Self {
                file: None,
                required: required
                    .iter()
                    .map(|name| output(&name.value))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            None => Ok(Self {
                file: None,
                required: Vec::new(),
            }),
        }
    }
}

/// Explicit invocation inputs. Source bindings are used verbatim, in caller
/// order, with no lookup of original sources or inclusion of support files.
#[derive(Clone)]
pub struct PlanInputs<'probe> {
    pub sources: Vec<PathBuf>,
    /// `run_cwd` is the fresh build-stage cwd, separate from the output root.
    /// For a build-free target it is the initial run cwd. Run preflight uses
    /// this cwd provisionally; every consumer launch binds its own fresh cwd.
    pub dirs: InvocationDirs,
    pub baseline: Baseline,
    pub platform: Platform,
    pub build_bytes: u64,
    pub probe: &'probe dyn Probe,
}

impl fmt::Debug for PlanInputs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanInputs")
            .field("sources", &self.sources)
            .field("dirs", &self.dirs)
            .field("baseline", &self.baseline)
            .field("platform", &self.platform)
            .field("build_bytes", &self.build_bytes)
            .finish_non_exhaustive()
    }
}

struct ProbeRef<'probe>(&'probe dyn Probe);

impl fmt::Debug for ProbeRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Probe")
    }
}

/// Independent planning failures, collected across both target stages.
#[derive(Debug)]
pub enum PlanError {
    OutputsRequired,
    UnexpectedOutputs,
    BuildCwdInsideOutputs,
    InvalidSourceBinding(PathBuf),
    /// One binding per configured source, in configured order.
    SourceCount {
        expected: usize,
        supplied: usize,
    },
    /// An artifact name that is not output-relative for the planning platform.
    InvalidArtifactName(String),
    Prepare(PrepareErrors),
    Resolve(ResolveErrors),
    Lower(Box<exec::LowerError>),
}

#[derive(Debug)]
pub struct PlanErrors {
    pub errors: Vec<PlanError>,
}

impl fmt::Display for PlanErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for error in &self.errors {
            writeln!(f, "{error:?}")?;
        }
        Ok(())
    }
}
impl std::error::Error for PlanErrors {}

/// Expanded source-consuming operands for C10a's capture guard. Argv includes
/// the expanded, unresolved program operand; shell exposes args, not the script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpandedArguments {
    Argv(Vec<OsString>),
    ShellArgs(Vec<OsString>),
}

impl ExpandedArguments {
    fn from_command(command: &PreparedCommand) -> Self {
        match command {
            PreparedCommand::Argv { program, args } => {
                let program = match program {
                    exec::ProgramRequest::Path(path) => path.as_os_str(),
                    exec::ProgramRequest::Lookup(name) => name.as_os_str(),
                };
                Self::Argv(
                    std::iter::once(program.to_owned())
                        .chain(args.iter().cloned())
                        .collect(),
                )
            }
            PreparedCommand::Shell { args, .. } => Self::ShellArgs(args.clone()),
        }
    }
}

#[derive(Debug)]
struct RunRecipe {
    target: TargetPlan,
    dirs: InvocationDirs,
    baseline: Baseline,
    platform: Platform,
    bindings: TargetBindings,
}

impl RunRecipe {
    fn prepare(&self, cwd: PathBuf) -> Result<exec::PreparedStage, PrepareErrors> {
        let mut dirs = self.dirs.clone();
        dirs.run_cwd = cwd;
        let ctx = Context::target_run(&self.target, dirs, self.bindings.clone())?;
        exec::prepare(&self.target.run, &ctx, &self.baseline, self.platform, &[])
    }

    fn run(&self, stage: &ResolvedStage, cwd: PathBuf) -> Result<ResolvedStage, PrepareErrors> {
        // No placeholder depends on run_cwd. Preparing again validates/selects
        // cwd through C2, while the preflighted program selection stays frozen.
        let prepared = self.prepare(cwd)?;
        let mut stage = stage.clone();
        stage.cwd = prepared.cwd;
        Ok(stage)
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Keep both capability types directly consumable.
pub enum PlannedTarget<'probe> {
    BuildFree(PreflightedBuildFree),
    Build(PendingBuild<'probe>),
}

impl PlannedTarget<'_> {
    pub(crate) fn command_bindings(&self) -> TargetBindings {
        match self {
            Self::Build(pending) => pending.recipe.bindings.clone(),
            Self::BuildFree(free) => free.recipe.bindings.clone(),
        }
    }
}

/// A resolved build-free run recipe, with no output root or build identity.
#[derive(Debug)]
pub struct PreflightedBuildFree {
    recipe: RunRecipe,
    stage: ResolvedStage,
    arguments: ExpandedArguments,
}

impl PreflightedBuildFree {
    pub fn run(&self, per_run_cwd: PathBuf) -> Result<ResolvedStage, PrepareErrors> {
        self.recipe.run(&self.stage, per_run_cwd)
    }

    pub fn expanded_arguments(&self) -> &ExpandedArguments {
        &self.arguments
    }
}

/// Only the producer is accessible before validation, even when the consumer
/// is an already-resolved external program such as `java` or a shell.
///
/// ```compile_fail,E0599
/// use cpforge::build::PendingBuild;
/// fn premature(build: &PendingBuild<'_>, cwd: std::path::PathBuf) {
///     let _ = build.run(cwd);
/// }
/// ```
#[derive(Debug)]
pub struct PendingBuild<'probe> {
    outputs: FreshBuildOutputs,
    build: ResolvedStage,
    run: Resolution,
    recipe: RunRecipe,
    required: Vec<PathBuf>,
    arguments: ExpandedArguments,
    build_bytes: u64,
    probe: ProbeRef<'probe>,
}

impl PendingBuild<'_> {
    pub fn id(&self) -> &BuildId {
        &self.outputs.id
    }
    pub fn output_root(&self) -> &Path {
        &self.outputs.root
    }
    pub fn build_stage(&self) -> &ResolvedStage {
        &self.build
    }
    /// Select the admitted cwd while keeping the preflighted program frozen.
    pub fn stage_for_cwd(&self, cwd: PathBuf) -> Result<ResolvedStage, PrepareErrors> {
        let mut dirs = self.recipe.dirs.clone();
        dirs.run_cwd = cwd;
        let context =
            Context::target_build(&self.recipe.target, dirs, self.recipe.bindings.clone())?;
        let prepared = exec::prepare(
            self.recipe.target.build.as_ref().expect("build stage"),
            &context,
            &self.recipe.baseline,
            self.recipe.platform,
            &[],
        )?;
        let mut stage = self.build.clone();
        stage.cwd = prepared.cwd;
        Ok(stage)
    }
    pub fn expanded_arguments(&self) -> &ExpandedArguments {
        &self.arguments
    }
    pub fn required_outputs(&self) -> &[PathBuf] {
        &self.required
    }
    pub fn build_bytes_limit(&self) -> u64 {
        self.build_bytes
    }
}

/// Preflight every external program in the target before returning its build.
/// Other job stages must also be preflighted by the caller before any launch.
/// A build requires a fresh allocation; a build-free target requires `None`.
/// Allocated directories remain caller-owned on planning failure. Job retention
/// owns their cleanup; only an oversized finalized build is removed here.
pub fn plan_target<'probe>(
    target: &TargetPlan,
    inputs: PlanInputs<'probe>,
    outputs: Option<FreshBuildOutputs>,
) -> Result<PlannedTarget<'probe>, PlanErrors> {
    let probe = inputs.probe;
    let build_bytes = inputs.build_bytes;
    let TargetPreflight {
        build,
        run,
        recipe,
        layout,
    } = preflight_target(
        target,
        inputs,
        outputs.as_ref().map(|outputs| outputs.root.as_path()),
    )?;
    let (run, run_arguments) = run;
    if let Some(outputs) = outputs {
        let (Resolution::Resolved(build), arguments) = build.expect("build preflight") else {
            unreachable!("C2 refuses generated build programs")
        };
        Ok(PlannedTarget::Build(PendingBuild {
            outputs,
            build,
            run,
            recipe,
            required: layout.expect("allocated layout").required,
            arguments,
            build_bytes,
            probe: ProbeRef(probe),
        }))
    } else {
        let Resolution::Resolved(stage) = run else {
            unreachable!("build-free stages have no generated program")
        };
        Ok(PlannedTarget::BuildFree(PreflightedBuildFree {
            recipe,
            stage,
            arguments: run_arguments,
        }))
    }
}

/// Read-only phase-3 validation. This value has no fresh-output capability and
/// cannot become a pending build. CLI preflight uses prospective job paths;
/// managed preparation repeats validation against the real captured inputs.
#[derive(Debug)]
pub(crate) struct TargetPreflight {
    build: Option<(Resolution, ExpandedArguments)>,
    run: (Resolution, ExpandedArguments),
    recipe: RunRecipe,
    layout: Option<ArtifactLayout>,
}
impl TargetPreflight {
    pub(crate) fn command_bindings(&self) -> TargetBindings {
        self.recipe.bindings.clone()
    }

    pub(crate) fn source_stage(&self) -> (&ResolvedStage, &ExpandedArguments) {
        let (resolution, arguments) = self.build.as_ref().unwrap_or(&self.run);
        let Resolution::Resolved(stage) = resolution else {
            unreachable!("source-consuming stage is external")
        };
        (stage, arguments)
    }
}

pub(crate) fn preflight_target(
    target: &TargetPlan,
    inputs: PlanInputs<'_>,
    output_root: Option<&Path>,
) -> Result<TargetPreflight, PlanErrors> {
    let mut errors = Vec::new();
    for source in &inputs.sources {
        if !syntax::native_absolute(source.as_os_str(), inputs.platform)
            || source.as_os_str().as_encoded_bytes().contains(&0)
        {
            errors.push(PlanError::InvalidSourceBinding(source.clone()));
        }
    }
    if inputs.sources.len() != target.sources.len() {
        errors.push(PlanError::SourceCount {
            expected: target.sources.len(),
            supplied: inputs.sources.len(),
        });
    }
    match (target.build.is_some(), output_root.is_some()) {
        (true, false) => errors.push(PlanError::OutputsRequired),
        (false, true) => errors.push(PlanError::UnexpectedOutputs),
        _ => {}
    }
    let layout = match output_root
        .map(|root| ArtifactLayout::new(root, target.artifacts.as_ref(), inputs.platform))
        .transpose()
    {
        Ok(layout) => layout,
        Err(error) => {
            errors.push(error);
            None
        }
    };
    let bindings = TargetBindings {
        sources: inputs.sources.clone(),
        artifact: layout.as_ref().and_then(|layout| layout.file.clone()),
        artifact_dir: output_root.map(Path::to_owned),
        entry: target.entry.as_ref().map(|entry| entry.value.clone()),
    };
    let build = target.build.as_ref().and_then(|stage| {
        let result = Context::target_build(target, inputs.dirs.clone(), bindings.clone())
            .and_then(|ctx| exec::prepare(stage, &ctx, &inputs.baseline, inputs.platform, &[]));
        if let (Ok(prepared), Some(root)) = (&result, output_root) {
            if prepared.cwd.starts_with(root) {
                errors.push(PlanError::BuildCwdInsideOutputs);
            }
        }
        preflight(result, inputs.probe, inputs.platform, &mut errors)
    });
    let recipe = RunRecipe {
        target: target.clone(),
        dirs: inputs.dirs,
        baseline: inputs.baseline,
        platform: inputs.platform,
        bindings,
    };
    let run = preflight(
        recipe.prepare(recipe.dirs.run_cwd.clone()),
        inputs.probe,
        recipe.platform,
        &mut errors,
    );
    if !errors.is_empty() {
        return Err(PlanErrors { errors });
    }
    Ok(TargetPreflight {
        build,
        run: run.expect("successful run preflight"),
        recipe,
        layout,
    })
}

fn preflight(
    prepared: Result<exec::PreparedStage, PrepareErrors>,
    probe: &dyn Probe,
    platform: Platform,
    errors: &mut Vec<PlanError>,
) -> Option<(Resolution, ExpandedArguments)> {
    match prepared {
        Ok(prepared) => {
            let args = ExpandedArguments::from_command(&prepared.command);
            match exec::resolve(prepared, probe, platform) {
                Ok(stage) => {
                    if let Resolution::Resolved(resolved) = &stage {
                        if let Err(error) = resolved.launch_command(platform) {
                            errors.push(PlanError::Lower(error));
                            return None;
                        }
                    }
                    Some((stage, args))
                }
                Err(error) => {
                    errors.push(PlanError::Resolve(error));
                    None
                }
            }
        }
        Err(error) => {
            errors.push(PlanError::Prepare(error));
            None
        }
    }
}

/// The supervisor facts that decide producer success; constructible without
/// processes in tests. Late-trigger notes are deliberately not stop triggers.
/// Cleanup `Incomplete` (live members that may still write outputs) fails the
/// build; `Unverified` does not, as for a test pass (execution §6.4).
#[derive(Clone, Debug)]
pub struct BuildExecutionOutcome {
    pub termination: Termination,
    pub violations: Vec<Violation>,
    pub stop_trigger: Option<StopTrigger>,
    pub enforced_at: Option<Duration>,
    pub cleanup: supervisor::Cleanup,
    pub errors: Vec<SupervisorError>,
    pub stdout: StreamEvidence,
    pub stderr: StreamEvidence,
}

impl From<&RootOutcome> for BuildExecutionOutcome {
    fn from(outcome: &RootOutcome) -> Self {
        let record = RunInput::from_root(outcome, false, [supervisor::Exceed::Discard; 2]);
        Self {
            termination: outcome.status,
            violations: outcome.violations.clone(),
            stop_trigger: outcome.stop_trigger,
            enforced_at: outcome.enforced_at,
            cleanup: outcome.termination.cleanup,
            errors: outcome.errors.clone(),
            stdout: record.stdout,
            stderr: record.stderr,
        }
    }
}

impl BuildExecutionOutcome {
    /// Adapt the finalized evidence using the producer's frozen stream policies.
    /// Build streams are never consumed. `enforced_at` is timing evidence, not
    /// a classification rule; stop triggers and violations decide the class.
    pub fn run_input(&self, policies: [crate::config::CapPolicy; 2]) -> RunInput {
        let mut stdout = self.stdout.clone();
        let mut stderr = self.stderr.clone();
        stdout.kill_on_breach = policies[0] == crate::config::CapPolicy::Kill;
        stderr.kill_on_breach = policies[1] == crate::config::CapPolicy::Kill;
        RunInput {
            execution: report::ExecutionEvidence::Started(self.termination),
            cleanup: self.cleanup,
            stop_trigger: self.stop_trigger,
            wall_violation: self
                .violations
                .iter()
                .any(|v| matches!(v, Violation::WallDeadline { .. })),
            budget_violations: report::BudgetStreams {
                stdout: self.violations.iter().any(|v| {
                    matches!(
                        v,
                        Violation::OutputBudget {
                            stream: supervisor::Stream::Stdout,
                            ..
                        }
                    )
                }),
                stderr: self.violations.iter().any(|v| {
                    matches!(
                        v,
                        Violation::OutputBudget {
                            stream: supervisor::Stream::Stderr,
                            ..
                        }
                    )
                }),
            },
            supervisor_errors: self.errors.iter().copied().map(Into::into).collect(),
            stdout,
            stderr,
            consumes_stdout: false,
            cwd_overflow_bytes: None,
        }
    }

    fn successful(&self, policies: [crate::config::CapPolicy; 2]) -> bool {
        report::classify_run(&self.run_input(policies)) == RunClass::ExitedZero
    }
}

/// Failure owns diagnostics and identity, never a consumer stage or recipe.
///
/// ```compile_fail,E0599
/// use cpforge::build::BuildFailure;
/// fn failed(build: &BuildFailure, cwd: std::path::PathBuf) {
///     let _ = build.run(cwd);
/// }
/// ```
#[derive(Debug)]
pub struct BuildFailure {
    pub id: BuildId,
    pub output_root: PathBuf,
    pub kind: Box<BuildFailureKind>,
    /// Independent overflow and cleanup evidence, even when another cause wins.
    pub over_cap: Option<Box<BuildOutputOverflow>>,
    /// Finalized observations made before any overflow removal. Paths name
    /// declared outputs, including when an ancestor is missing or is a link.
    pub post_build: Box<PostBuild>,
}

#[derive(Debug)]
pub struct BuildOutputOverflow {
    pub bytes: u64,
    pub limit: u64,
    /// Retains inspection errors when measured bytes already prove overflow.
    pub measurement: WalkReport,
    /// Errors or skipped boundaries in an Ok report mean incomplete removal.
    pub removal: Result<WalkReport, StorageError>,
}

#[derive(Debug)]
pub enum BuildFailureKind {
    ProducerFailed(Box<BuildExecutionOutcome>),
    OutputMissing(PathBuf),
    OutputNotRegular(PathBuf),
    OutputThroughLink(PathBuf),
    OutputInspection(StorageError),
    Measurement(StorageError),
    IncompleteMeasurement(WalkReport),
    BuildOutputTooLarge,
    GeneratedProgramNotLaunchable(ResolveErrors),
}

impl fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "build {}: {:?}", self.id, self.kind)?;
        if let Some(overflow) = &self.over_cap {
            write!(f, "; over cap: {overflow:?}")?;
        }
        Ok(())
    }
}
impl std::error::Error for BuildFailure {}

/// Validated whole output tree and frozen consumer selection. Each `run` binds
/// a fresh caller-owned cwd; validation and external lookup are never repeated.
/// (Positive control for the `compile_fail` examples on [`PendingBuild`] and
/// [`BuildFailure`]: the same call compiles here.)
///
/// ```
/// use cpforge::build::ValidatedBuild;
/// fn ready(build: &ValidatedBuild, cwd: std::path::PathBuf) {
///     let _ = build.run(cwd);
/// }
/// ```
#[derive(Debug)]
pub struct ValidatedBuild {
    outputs: FreshBuildOutputs,
    recipe: RunRecipe,
    stage: ResolvedStage,
}

impl ValidatedBuild {
    pub fn id(&self) -> &BuildId {
        &self.outputs.id
    }
    pub fn output_root(&self) -> &Path {
        &self.outputs.root
    }
    pub fn run(&self, per_run_cwd: PathBuf) -> Result<ResolvedStage, PrepareErrors> {
        self.recipe.run(&self.stage, per_run_cwd)
    }
}

/// Consume one producer attempt. C2 completes any deferred generated program
/// using the planning probe; external programs were resolved during planning.
pub fn finish_build(
    pending: PendingBuild<'_>,
    outcome: BuildExecutionOutcome,
) -> Result<ValidatedBuild, BuildFailure> {
    let fail = |kind| BuildFailure {
        id: pending.outputs.id.clone(),
        output_root: pending.outputs.root.clone(),
        post_build: Box::new(PostBuild::from_failure_kind(&kind, &pending.outputs.root)),
        kind: Box::new(kind),
        over_cap: None,
    };
    if !outcome.successful([pending.build.stdout.policy, pending.build.stderr.policy]) {
        return Err(fail(BuildFailureKind::ProducerFailed(Box::new(outcome))));
    }
    let root = &pending.outputs.root;
    storage::checked_path(&pending.outputs.area, root, false)
        .map_err(|error| fail(output_error(error)))?;
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| fail(output_error(StorageError::io(root, error))))?;
    if !metadata.is_dir() {
        return Err(fail(BuildFailureKind::OutputNotRegular(root.clone())));
    }
    let measured = storage::measure_owned_tree(&pending.outputs.area, root)
        .map_err(|error| fail(BuildFailureKind::Measurement(error)))?;
    let limit = pending.build_bytes;
    let over_cap = measured.bytes > limit;
    if !over_cap && (!measured.errors.is_empty() || !measured.skipped.is_empty()) {
        return Err(fail(BuildFailureKind::IncompleteMeasurement(measured)));
    }
    let mut primary = None;
    let mut post_build = PostBuild::default();
    for path in &pending.required {
        if let Err(kind) = inspect_output(&pending.outputs.area, path) {
            match &kind {
                BuildFailureKind::OutputMissing(_)
                | BuildFailureKind::OutputNotRegular(_)
                | BuildFailureKind::OutputThroughLink(_) => post_build.missing_outputs.push(
                    path.strip_prefix(root)
                        .expect("declared output beneath root")
                        .as_os_str()
                        .to_owned(),
                ),
                _ => post_build.invariant = Some(report::Invariant::BuildEvidence),
            }
            if primary.is_none() {
                primary = Some(kind);
            }
        }
    }
    let stage = match pending.run {
        Resolution::Resolved(stage) => Some(stage),
        Resolution::AwaitingProducer(stage) => match stage.complete(pending.probe.0) {
            Ok(stage) => Some(stage),
            Err(error) => {
                let kind = BuildFailureKind::GeneratedProgramNotLaunchable(error);
                let generated = PostBuild::from_failure_kind(&kind, root);
                post_build.generated_program_invalid = generated.generated_program_invalid;
                post_build.invariant = post_build.invariant.or(generated.invariant);
                if primary.is_none() {
                    primary = Some(kind);
                }
                None
            }
        },
    };
    // Validation must observe the original tree. Overflow cleanup is retained
    // independently of whichever earlier contract row supplies the cause.
    let overflow = if over_cap {
        post_build.output_too_large = Some((measured.bytes, limit));
        Some(Box::new(BuildOutputOverflow {
            bytes: measured.bytes,
            limit,
            measurement: measured,
            removal: storage::remove_owned_tree(&pending.outputs.area, root),
        }))
    } else {
        None
    };
    if let Some(kind) = primary.or(over_cap.then_some(BuildFailureKind::BuildOutputTooLarge)) {
        return Err(BuildFailure {
            id: pending.outputs.id,
            output_root: pending.outputs.root,
            kind: Box::new(kind),
            over_cap: overflow,
            post_build: Box::new(post_build),
        });
    }
    Ok(ValidatedBuild {
        outputs: pending.outputs,
        recipe: pending.recipe,
        stage: stage.expect("generated program validated"),
    })
}

fn inspect_output(area: &Path, path: &Path) -> Result<(), BuildFailureKind> {
    storage::checked_path(area, path, false).map_err(output_error)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| output_error(StorageError::io(path, error)))?;
    if !metadata.is_file() {
        return Err(BuildFailureKind::OutputNotRegular(path.to_path_buf()));
    }
    Ok(())
}

fn output_error(error: StorageError) -> BuildFailureKind {
    match &error.kind {
        storage::ErrorKind::LinkInPath { path } => {
            BuildFailureKind::OutputThroughLink(path.clone())
        }
        storage::ErrorKind::Io { path, source }
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            BuildFailureKind::OutputMissing(path.clone())
        }
        _ => BuildFailureKind::OutputInspection(error),
    }
}

#[cfg(test)]
mod tests;
