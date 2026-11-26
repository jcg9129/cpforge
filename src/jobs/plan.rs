//! Read-only judged-job shape. Paths are prospective until admission; this
//! module allocates no directories, credits, reservations, or processes.
use std::fmt;
use std::path::{Path, PathBuf};

use super::engine::Stage;
use crate::config::{CheckConfig, RoleInput, TargetPlan};
use crate::report::{
    CheckMode, RoleName,
    wire::{self, ReportShape},
};
use crate::storage::inputs::roles::declarations;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionRole {
    SolutionBuild,
    RoleBuild(RoleName),
    Solution,
    Judge(RoleName),
    Hook,
}

/// All retained paths are outside the private cwd. Fixed files and assets
/// below cwd are scratch, whose exact lengths are reserved at run admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunLayout {
    pub dir: PathBuf,
    pub cwd: PathBuf,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub output: PathBuf,
    pub judge_report: PathBuf,
    pub judge_tout: PathBuf,
    pub transcript: PathBuf,
}
impl RunLayout {
    pub fn new(job: &Path, index: usize) -> Self {
        // One component per join, so no host path mixes separators.
        let dir = job.join(format!("r{index:05}"));
        let (out, judge) = (dir.join("out"), dir.join("judge"));
        Self {
            cwd: dir.join("cwd"),
            stdout: out.join("stdout"),
            stderr: out.join("stderr"),
            output: out.join("output"),
            judge_report: judge.join("report"),
            judge_tout: judge.join("tout"),
            transcript: dir.join("transcript"),
            dir,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Execution {
    pub index: u32,
    pub stage: Stage,
    pub role: ExecutionRole,
    pub test: Option<usize>,
    pub layout: RunLayout,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct JudgeOptions {
    /// Eligible hooks after trust/depth filtering.
    pub hooks: usize,
    /// The solution declares io.output; checker input is out/output.
    pub output_file: bool,
}

#[derive(Debug)]
pub struct TestJobPlan {
    pub executions: Vec<Execution>,
    pub shape: ReportShape,
    pub report_bytes: u64,
    pub tests: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    ExecutionCap,
    JudgedTestCap,
    ReportCap,
    InteractorUnsupported,
}
impl PlanError {
    pub fn exit_code(&self) -> u8 {
        7
    }
}
impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ExecutionCap => "planned runs exceed the 10 000 execution cap",
            Self::JudgedTestCap => "at most 1 000 tests per job on a problem with a checker, validator or interactor; select fewer with --case",
            Self::ReportCap => "report reservation exceeds plan cap",
            Self::InteractorUnsupported => "interactor planning unsupported in this version",
        })
    }
}
impl std::error::Error for PlanError {}

/// The single CLI execution gate for parsed judging, before admission.
/// Prepared plans remain usable through the engine API for role producers.
pub fn judging_supported(check: Option<&CheckConfig>) -> bool {
    check.is_none_or(|c| !c.has_roles())
}

/// Phase-3 answer policy check, before CLI admission and again on frozen data.
pub fn validate_answer_presence(
    check: &CheckConfig,
    answers: impl IntoIterator<Item = (String, bool)>,
) -> Result<(), String> {
    if check.kind.value == CheckMode::Compare
        || check.answer.value != crate::report::AnswerPolicy::File
    {
        return Ok(());
    }
    let missing: Vec<_> = answers
        .into_iter()
        .filter_map(|(id, present)| (!present).then_some(id))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "tests without an answer under answer = \"file\": {}",
            missing.join(", ")
        ))
    }
}

/// Counts, caps and report reservation of a test job, before admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanBounds {
    pub planned: usize,
    pub shape: ReportShape,
    pub report_bytes: u64,
}

/// Config §8.2 in its required order: the run count (builds, per-test role
/// and solution runs, eligible hooks) with checked arithmetic, then the
/// judged-test cap, then the report reservation cap. Allocates nothing, so a
/// caller can refuse before admission. Compare-only plans count exactly as
/// before (tests + solution build + hooks).
pub fn validate_test_plan(
    target: &TargetPlan,
    check: Option<&CheckConfig>,
    tests: usize,
    hooks: usize,
    output_file: bool,
) -> Result<PlanBounds, PlanError> {
    let interactive = check.is_some_and(|c| c.kind.value == CheckMode::Interactive);
    let checker = check.is_some_and(|c| c.checker.is_some());
    let validator = check.is_some_and(|c| c.validator.is_some());
    let builds = usize::from(target.build.is_some())
        + check.map_or(0, |c| declarations(c).filter(|(_, r)| has_build(r)).count());
    let per_test = 1 + usize::from(checker) + usize::from(validator) + usize::from(interactive);
    let planned = tests
        .checked_mul(per_test)
        .and_then(|n| n.checked_add(builds))
        .and_then(|n| n.checked_add(hooks))
        .filter(|&n| n <= 10_000)
        .ok_or(PlanError::ExecutionCap)?;
    if let Some(check) = check {
        check
            .validate_test_count(tests)
            .map_err(|_| PlanError::JudgedTestCap)?;
    }
    let shape = ReportShape {
        validator,
        judges: u8::from(checker) + u8::from(interactive),
        interactive,
        output_file,
    };
    let report_bytes = wire::report_reservation(planned, tests, 0, Some(shape))
        .map_err(|_| PlanError::ReportCap)?;
    Ok(PlanBounds {
        planned,
        shape,
        report_bytes,
    })
}

fn has_build(role: &crate::config::RoleSpec) -> bool {
    matches!(&role.input, RoleInput::Built(b) if b.build.is_some())
}

/// Includes eligible hooks, supplied after trust/depth filtering. Validate the
/// execution count before the judged-test and report caps, without allocating
/// a vector proportional to an unchecked input population.
pub fn plan_test_job(
    target: &TargetPlan,
    check: Option<&CheckConfig>,
    tests: usize,
    hooks: usize,
    output_file: bool,
    job_dir: &Path,
) -> Result<TestJobPlan, PlanError> {
    let PlanBounds {
        planned,
        shape,
        report_bytes,
    } = validate_test_plan(target, check, tests, hooks, output_file)?;
    if shape.interactive {
        return Err(PlanError::InteractorUnsupported);
    }
    let mut executions = Vec::with_capacity(planned);
    let mut push = |stage, role, test| {
        let index = executions.len();
        executions.push(Execution {
            index: index as u32,
            stage,
            role,
            test,
            layout: RunLayout::new(job_dir, index),
        });
    };
    if target.build.is_some() {
        push(Stage::Build, ExecutionRole::SolutionBuild, None);
    }
    if let Some(check) = check {
        for (name, spec) in declarations(check) {
            if has_build(spec) {
                push(Stage::Build, ExecutionRole::RoleBuild(name), None);
            }
        }
    }
    for test in 0..tests {
        if shape.validator {
            push(
                Stage::Validator,
                ExecutionRole::Judge(RoleName::Validator),
                Some(test),
            );
        }
        push(Stage::Run, ExecutionRole::Solution, Some(test));
        if shape.judges != 0 {
            push(
                Stage::Checker,
                ExecutionRole::Judge(RoleName::Checker),
                Some(test),
            );
        }
    }
    for _ in 0..hooks {
        push(Stage::Hook, ExecutionRole::Hook, None);
    }
    Ok(TestJobPlan {
        executions,
        shape,
        report_bytes,
        tests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_keeps_retained_evidence_outside_private_cwd() {
        let layout = RunLayout::new(Path::new("job"), 42);
        assert_eq!(layout.cwd, Path::new("job/r00042/cwd"));
        for path in [
            &layout.stdout,
            &layout.stderr,
            &layout.output,
            &layout.judge_report,
            &layout.judge_tout,
            &layout.transcript,
        ] {
            assert!(path.starts_with(&layout.dir));
            assert!(!path.starts_with(&layout.cwd));
        }
    }
}
