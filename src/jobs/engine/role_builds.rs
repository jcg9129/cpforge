//! Adapt the frozen judged plan and ordinary build evidence to role members.
use super::{BuildReport, PlanError, PlannedExecution, Recipe, Role};
use crate::exec::Resolution;
use crate::jobs::{plan::ExecutionRole, roles::ReadyJudging};
use crate::report::{
    self, BuildCause, BuildResult, JudgeText, RoleBuild, RoleBuildCause, RoleBuildResult,
};
use crate::tests_store::FrozenManifest;

pub(super) fn plan(
    judging: &ReadyJudging<'_>,
    manifest: &FrozenManifest,
) -> Result<Vec<PlannedExecution>, PlanError> {
    let mut has_build = false;
    judging
        .plan
        .executions
        .iter()
        .filter(|e| e.role != ExecutionRole::Hook)
        .map(|e| {
            let depends_on_build = has_build;
            let test = e.test.map(|i| manifest.entries()[i].clone());
            let (role, recipe, stdin) = match e.role {
                ExecutionRole::SolutionBuild => {
                    has_build = true;
                    (Role::Build, Recipe::TargetRun, None)
                }
                ExecutionRole::RoleBuild(role) => {
                    has_build = true;
                    (Role::RoleBuild { role }, Recipe::TargetRun, None)
                }
                ExecutionRole::Solution => (
                    Role::Test {
                        test: test.as_ref().expect("test").id.to_string(),
                    },
                    Recipe::TargetRun,
                    test.as_ref().map(|t| t.input.path.clone()),
                ),
                ExecutionRole::Judge(name) => {
                    let run = judging
                        .roles
                        .iter()
                        .find(|r| r.captured.role == name)
                        .and_then(|r| r.runs.iter().find(|r| r.execution == e.index))
                        .ok_or(PlanError::InvalidIteration)?;
                    let id = test.as_ref().expect("judge test").id.to_string();
                    let role = match name {
                        report::RoleName::Checker => Role::Checker { test: id },
                        report::RoleName::Validator => Role::Validator { test: id },
                        _ => return Err(PlanError::InvalidIteration),
                    };
                    let recipe = match run.stage.as_ref() {
                        Resolution::Resolved(stage) => Recipe::Resolved(Box::new(stage.clone())),
                        Resolution::AwaitingProducer(stage) => {
                            Recipe::Deferred(Box::new(stage.clone()))
                        }
                    };
                    (role, recipe, run.stdin.clone())
                }
                ExecutionRole::Hook => unreachable!("filtered hooks"),
            };
            Ok(PlannedExecution {
                index: e.index,
                stage: e.stage,
                role,
                depends_on_build,
                recipe,
                stdin,
                test,
            })
        })
        .collect()
}

fn name(value: &std::ffi::OsStr) -> JudgeText {
    JudgeText::new(&report::wire::native_bytes(value), 128)
}

pub(super) fn report(build: BuildReport, artifact_count: usize) -> RoleBuild {
    let result = match build.result {
        BuildResult::Succeeded => RoleBuildResult::Succeeded,
        BuildResult::Cancelled => RoleBuildResult::Cancelled,
        BuildResult::Infrastructure { causes } => RoleBuildResult::Infrastructure { causes },
        BuildResult::InternalError { invariant } => RoleBuildResult::InternalError { invariant },
        BuildResult::Failed { cause } => RoleBuildResult::Failed {
            cause: Box::new(match cause {
                BuildCause::Run(class) => RoleBuildCause::Run(class),
                BuildCause::MissingOutput {
                    names,
                    names_dropped,
                } => RoleBuildCause::MissingOutput {
                    names: names.iter().map(|v| name(v)).collect(),
                    names_dropped,
                },
                BuildCause::GeneratedProgramInvalid { name: path, reason } => {
                    RoleBuildCause::GeneratedProgramInvalid {
                        name: name(&path),
                        reason,
                    }
                }
                BuildCause::OutputTooLarge { bytes, cap } => {
                    RoleBuildCause::OutputTooLarge { bytes, cap }
                }
            }),
        },
    };
    let artifacts_dropped = if matches!(result, RoleBuildResult::Succeeded) {
        artifact_count.saturating_sub(16) as u32
    } else {
        0
    };
    RoleBuild {
        execution: build.execution,
        build_id: build.build_id.as_ref().map(ToString::to_string),
        output_root: build.build_id.map(|id| format!("builds/{id}")),
        result,
        artifacts: build.artifacts.iter().map(|v| name(v)).collect(),
        artifacts_dropped,
    }
}

pub(super) fn skipped(execution: u32, reason: report::SkipReason) -> RoleBuild {
    RoleBuild {
        execution,
        build_id: None,
        output_root: None,
        result: if reason == report::SkipReason::Cancelled {
            RoleBuildResult::Cancelled
        } else {
            RoleBuildResult::Skipped { reason }
        },
        artifacts: Vec::new(),
        artifacts_dropped: 0,
    }
}

/// Preserve each test's full planned range and unexecuted judge slots when
/// producers prevented it from starting. Judge-run derivation owns started slots.
pub(super) fn skipped_tests(
    roles: &[report::RoleReport],
    executions: &[super::ExecutionReport],
    tests: &mut [super::TestReport],
) {
    for test in tests {
        let planned: Vec<_> = executions
            .iter()
            .filter(|e| e.plan.test.as_ref().is_some_and(|t| t.id == test.frozen.id))
            .collect();
        if let Some(first) = planned.first() {
            test.first_execution = first.plan.index;
            test.execution_count = planned.len() as u32;
        }
        for execution in planned {
            if !matches!(execution.result.coverage, report::Coverage::Skipped { .. }) {
                continue;
            }
            let (name, judge_role) = match execution.plan.role {
                Role::Validator { .. } => {
                    (report::RoleName::Validator, report::JudgeRole::Validator)
                }
                Role::Checker { .. } => (report::RoleName::Checker, report::JudgeRole::Checker),
                _ => continue,
            };
            let profile = roles
                .iter()
                .find(|r| r.role == name)
                .and_then(|r| r.profile)
                .expect("judge profile");
            if name == report::RoleName::Validator {
                test.validation = Some(report::Validation {
                    execution: execution.plan.index,
                    profile,
                    outcome: report::ValidationOutcome::NotRun,
                    stderr: None,
                });
            } else {
                test.judges.push(report::Judge {
                    role: judge_role,
                    execution: execution.plan.index,
                    profile,
                    result: None,
                    message: None,
                    stderr: None,
                    report: None,
                });
            }
        }
    }
}
