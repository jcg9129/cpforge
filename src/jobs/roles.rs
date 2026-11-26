//! Resolve captured judge stages and freeze their provenance before execution.
use std::ffi::OsString;
use std::path::PathBuf;

use super::{
    CaptureCheck, PrepareJobInputs, ReadyError, guard,
    plan::{self, ExecutionRole, TestJobPlan},
};
use crate::build::{self, BuildArea, BuildId, PendingBuild, PlanInputs, PlannedTarget};
use crate::config::TargetPlan;
use crate::exec::{self, Context, InvocationDirs, Resolution, ResolvedStage};
use crate::report::{JudgeProfile, RoleName};
use crate::storage::budget::Reservation;
use crate::storage::inputs::{CapturedRole, FrozenJudging, SourceRole};
use crate::tests_store::{FrozenAnswer, FrozenManifest};

#[derive(Clone, Debug)]
pub struct StagedFile {
    /// None means an intentionally empty answer, never a live input path.
    pub source: Option<PathBuf>,
    pub destination: PathBuf,
    /// Exact frozen capture length; None for immutable captured inputs/assets.
    pub bytes: Option<u64>,
}

#[derive(Debug)]
pub struct JudgeRun {
    pub execution: u32,
    /// A generated program stays deferred until its build has been validated.
    pub stage: Box<Resolution>,
    pub stdin: Option<PathBuf>,
    pub staged: Vec<StagedFile>,
}

#[derive(Debug)]
pub struct ReadyRole<'probe> {
    pub captured: CapturedRole,
    pub target: Box<TargetPlan>,
    pub capture_check: CaptureCheck,
    pub pending_build: Option<Box<PendingBuild<'probe>>>,
    pub report: crate::report::RoleReport,
    pub build_stage: Option<Box<ResolvedStage>>,
    pub runs: Vec<JudgeRun>,
}

#[derive(Debug)]
pub struct ReadyJudging<'probe> {
    pub check: Box<crate::config::CheckConfig>,
    pub plan: TestJobPlan,
    pub roles: Vec<ReadyRole<'probe>>,
    /// Held until eventual report publication; independent of run allowances.
    pub report_reservation: Reservation,
    pub judge_fingerprint: Option<[u8; 32]>,
}

pub(super) fn prepare_roles<'probe>(
    frozen: FrozenJudging,
    manifest: &FrozenManifest,
    target: &TargetPlan,
    registered: &crate::storage::admission::RegisteredJob,
    inputs: &PrepareJobInputs<'probe>,
) -> Result<ReadyJudging<'probe>, ReadyError> {
    let plan = plan::plan_test_job(
        target,
        Some(&frozen.check),
        manifest.entries().len(),
        inputs.judge_options.hooks,
        inputs.judge_options.output_file,
        &registered.dir,
    )
    .map_err(ReadyError::JudgingPlan)?;
    plan::validate_answer_presence(
        &frozen.check,
        manifest.entries().iter().map(|test| {
            (
                test.id.to_string(),
                matches!(test.answer, FrozenAnswer::Present { .. }),
            )
        }),
    )
    .map_err(ReadyError::Judging)?;
    // Retention, not configuration: refused like the capture manifest's.
    let report_reservation = registered.budget.reserve(plan.report_bytes).map_err(|e| {
        ReadyError::Storage(crate::storage::StorageError::io(
            &registered.dir,
            std::io::Error::other(e.to_string()),
        ))
    })?;
    let area = BuildArea::new(registered.dir.join("builds")).map_err(ReadyError::Storage)?;
    let mut roles = Vec::new();
    for captured in frozen.roles {
        // Boxed at once: the plan is held across every per-run resolution.
        let role_target = Box::new(TargetPlan::for_role(&captured.spec));
        for stage in role_target
            .build
            .iter()
            .chain(std::iter::once(&role_target.run))
        {
            let errors = exec::limit_errors(&stage.limits);
            if !errors.is_empty() {
                return Err(ReadyError::Judging(errors.join("\n")));
            }
        }
        let build = plan
            .executions
            .iter()
            .find(|e| e.role == ExecutionRole::RoleBuild(captured.role));
        let outputs = build
            .map(|e| {
                build::allocate(
                    &area,
                    BuildId::new(format!("r{:05}", e.index)).expect("plan build id"),
                )
            })
            .transpose()
            .map_err(ReadyError::Storage)?;
        let source_cwd = build
            .or_else(|| {
                plan.executions
                    .iter()
                    .find(|e| e.role == ExecutionRole::Judge(captured.role))
            })
            .map_or_else(
                || registered.dir.join("role").join("cwd"),
                |e| e.layout.cwd.clone(),
            );
        let dirs = |cwd| InvocationDirs {
            run_cwd: cwd,
            problem: Some(inputs.problem.clone()),
            workspace: Some(inputs.workspace_root.clone()),
        };
        let sources = captured.files.source_bindings();
        let preflight = build::preflight_target(
            &role_target,
            PlanInputs {
                sources,
                dirs: dirs(source_cwd),
                baseline: inputs.baseline.clone(),
                platform: inputs.platform,
                build_bytes: inputs.build_bytes,
                probe: inputs.probe,
            },
            outputs.as_ref().map(|o| o.path()),
        )
        .map_err(ReadyError::Plan)?;
        let (stage, args) = preflight.source_stage();
        let capture_check = guard::check_capture(
            role_target.build.as_ref().unwrap_or(&role_target.run),
            args,
            &stage.cwd,
            &inputs.workspace_root,
            &registered.dir,
            inputs.platform,
        )
        .map_err(ReadyError::Guard)?;
        let build_stage = role_target.build.as_ref().map(|_| Box::new(stage.clone()));
        let mut runs = Vec::new();
        for execution in plan
            .executions
            .iter()
            .filter(|e| e.role == ExecutionRole::Judge(captured.role))
        {
            let test_index = execution.test.expect("judge test");
            let test = &manifest.entries()[test_index];
            let cwd = &execution.layout.cwd;
            let mut staged: Vec<_> = captured
                .files
                .files()
                .iter()
                .filter(|f| f.role == SourceRole::Asset)
                .map(|f| StagedFile {
                    source: Some(f.copy_path.clone()),
                    bytes: None,
                    destination: f
                        .problem_path
                        .components()
                        .iter()
                        .fold(cwd.clone(), |p, c| p.join(c)),
                })
                .collect();
            staged.push(StagedFile {
                source: Some(test.input.path.clone()),
                bytes: None,
                destination: cwd.join("input"),
            });
            let mut extra: Vec<OsString> = Vec::new();
            let stdin = if captured.role == RoleName::Validator {
                if captured.spec.profile.value == JudgeProfile::Simple {
                    extra.push(cwd.join("input").into());
                }
                Some(cwd.join("input"))
            } else {
                let solution = plan
                    .executions
                    .iter()
                    .find(|e| e.test == Some(test_index) && e.role == ExecutionRole::Solution)
                    .expect("solution run");
                staged.push(StagedFile {
                    source: Some(if plan.shape.output_file {
                        solution.layout.output.clone()
                    } else {
                        solution.layout.stdout.clone()
                    }),
                    destination: cwd.join("output"),
                    bytes: None,
                });
                // Absent is reachable only under `answer = "none"` (checked above).
                let answer = match &test.answer {
                    FrozenAnswer::Present { path, .. } => Some(path.clone()),
                    FrozenAnswer::Absent => None,
                };
                staged.push(StagedFile {
                    source: answer,
                    bytes: None,
                    destination: cwd.join("answer"),
                });
                extra.extend([
                    cwd.join("input").into(),
                    cwd.join("output").into(),
                    cwd.join("answer").into(),
                ]);
                if captured.spec.profile.value == JudgeProfile::Testlib {
                    extra.extend([cwd.join("report").into(), OsString::from("-appes")]);
                }
                None
            };
            let ctx = Context::target_run(
                &role_target,
                dirs(cwd.clone()),
                preflight.command_bindings(),
            )
            .map_err(|e| ReadyError::Judging(e.to_string()))?;
            let prepared = exec::prepare(
                &role_target.run,
                &ctx,
                &inputs.baseline,
                inputs.platform,
                &extra,
            )
            .map_err(|e| ReadyError::Judging(e.to_string()))?;
            let stage = exec::resolve(prepared, inputs.probe, inputs.platform)
                .map_err(|e| ReadyError::Judging(e.to_string()))?;
            if let Resolution::Resolved(stage) = &stage {
                stage
                    .launch_command(inputs.platform)
                    .map_err(|e| ReadyError::Judging(e.to_string()))?;
            }
            runs.push(JudgeRun {
                execution: execution.index,
                stage: Box::new(stage),
                stdin,
                staged,
            });
        }
        let report = super::role_provenance::snapshot(&captured, &capture_check)
            .map_err(|e| ReadyError::Judging(e.to_string()))?;
        let pending_build = match preflight.into_planned(outputs, inputs.build_bytes, inputs.probe)
        {
            PlannedTarget::Build(pending) => Some(Box::new(pending)),
            PlannedTarget::BuildFree(_) => None,
        };
        roles.push(ReadyRole {
            captured,
            target: role_target,
            capture_check,
            pending_build,
            report,
            build_stage,
            runs,
        });
    }
    let judge_fingerprint =
        fingerprint(&frozen.check, &roles).map_err(|e| ReadyError::Judging(e.to_string()))?;
    Ok(ReadyJudging {
        check: frozen.check,
        plan,
        roles,
        report_reservation,
        judge_fingerprint,
    })
}

fn fingerprint(
    check: &crate::config::CheckConfig,
    roles: &[ReadyRole<'_>],
) -> std::io::Result<Option<[u8; 32]>> {
    use crate::storage::facts::fingerprint::{JudgeFingerprintRole, judge_fingerprint_from_copies};
    let mut slots = [None, None, None];
    let mut copies: [Vec<_>; 3] = std::array::from_fn(|_| Vec::new());
    for role in roles {
        let index = match role.captured.role {
            RoleName::Checker => 0,
            RoleName::Validator => 1,
            RoleName::Interactor => 2,
            _ => unreachable!("judging role"),
        };
        copies[index] = role.captured.files.files().iter().collect();
        slots[index] = Some(JudgeFingerprintRole::from_spec(&role.captured.spec));
    }
    judge_fingerprint_from_copies(&check.report_config(), &slots, &copies)
}
