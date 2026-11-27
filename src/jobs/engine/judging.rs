//! Adapt finalized ordinary executions to the pure batch table.
use super::{Context, ExecutionReport, PlannedExecution, Role, TestReport};
use crate::config::Comparator;
use crate::report::{self as r, Coverage, RunClass};
use crate::tests_store::{FrozenAnswer, FrozenTest};

fn same_test(a: &PlannedExecution, b: &PlannedExecution) -> bool {
    a.test
        .as_ref()
        .zip(b.test.as_ref())
        .is_some_and(|(a, b)| a.id == b.id)
}

pub(super) fn input_not_validated(plan: &PlannedExecution, earlier: &[ExecutionReport]) -> bool {
    matches!(plan.role, Role::Test { .. } | Role::Checker { .. })
        && earlier
            .iter()
            .rev()
            .find(|e| same_test(plan, &e.plan) && matches!(e.plan.role, Role::Validator { .. }))
            .is_some_and(|v| v.result.class != Some(RunClass::ExitedZero))
}

pub(super) fn checker_not_eligible(
    plan: &PlannedExecution,
    earlier: &[ExecutionReport],
    output_file: bool,
) -> bool {
    matches!(plan.role, Role::Checker { .. })
        && earlier
            .iter()
            .rev()
            .find(|e| same_test(plan, &e.plan) && matches!(e.plan.role, Role::Test { .. }))
            .is_none_or(|s| {
                (output_file && !collected(s))
                    || s.result.coverage != Coverage::Started
                    || s.result.class.as_ref().is_none_or(|c| {
                        matches!(c, RunClass::Infrastructure { .. } | RunClass::Cancelled)
                    })
            })
}

/// Report §2.15: with `io.output` a checker runs only on a collected file.
fn collected(solution: &ExecutionReport) -> bool {
    matches!(
        solution.evidence.as_deref(),
        Some(super::ExecutionRecord::Started(root))
            if matches!(root.output_file, Some(r::OutputFile::Collected { .. }))
    )
}

pub(super) fn input(execution: &ExecutionReport, profile: r::JudgeProfile) -> r::JudgeInput {
    r::JudgeInput {
        execution: execution.result.clone(),
        profile,
        cleanup_unverified: execution
            .summary
            .as_ref()
            .is_some_and(|s| s.notes.contains(&r::RunNote::CleanupUnverified)),
        read: execution
            .judge_evidence
            .as_ref()
            .map_or(r::JudgeRead::Invalid(r::JudgeReason::ReportMissing), |e| {
                e.read.clone()
            }),
    }
}

fn own_result(judge: &r::JudgeInput, scoring: r::Scoring) -> Option<r::JudgeResult> {
    if judge.execution.coverage != Coverage::Started {
        return None;
    }
    match judge.execution.class.as_ref()? {
        RunClass::Cancelled => {
            return Some(r::JudgeResult::NotRead {
                reason: r::JudgeNotRead::Stopped,
            });
        }
        RunClass::Infrastructure { .. } => {
            return Some(r::JudgeResult::NotRead {
                reason: r::JudgeNotRead::CleanupIncomplete,
            });
        }
        _ => {}
    }
    if matches!(judge.read, r::JudgeRead::Io { .. }) {
        return Some(r::JudgeResult::NotRead {
            reason: r::JudgeNotRead::ExtractionFailed,
        });
    }
    // The same table derives the judge's own outcome, independently of a
    // diagnostic solution failure (B16) or cancellation before admission (B13).
    let solution = r::ExecutionResult {
        coverage: Coverage::Started,
        class: Some(RunClass::ExitedZero),
        invariant: None,
    };
    let (check, _) = r::batch_outcome(&r::BatchInput {
        validator: None,
        solution: &solution,
        checker: Some(judge),
        scoring,
        admission: r::CheckAdmission::NoIntent,
        answer_present: true,
        comparison: &r::Comparison::NotRun,
        output_file: None,
    });
    Some(match check.kind {
        r::CheckKind::Passed => r::JudgeResult::Passed,
        r::CheckKind::Judged { outcome } => match *outcome {
            r::JudgedCheck::Rejected { fail, .. } => r::JudgeResult::Rejected { fail },
            r::JudgedCheck::JudgeFailed { class, reason, .. } => {
                r::JudgeResult::JudgeFailed { class, reason }
            }
            r::JudgedCheck::Scored { raw, scale, .. } => r::JudgeResult::Scored { raw, scale },
            r::JudgedCheck::Unchecked { .. } => r::JudgeResult::Unverified,
        },
        _ => return None,
    })
}

pub(super) fn finish_test(
    context: &Context<'_>,
    test: FrozenTest,
    executions: &[ExecutionReport],
    comparator: &Comparator,
    config: &r::CheckConfig,
    roles: &[r::RoleReport],
) -> TestReport {
    let runs: Vec<_> = executions
        .iter()
        .filter(|e| e.plan.test.as_ref().is_some_and(|t| t.id == test.id))
        .collect();
    let solution = runs
        .iter()
        .find(|e| matches!(e.plan.role, Role::Test { .. }))
        .copied()
        .expect("planned solution");
    let validator = runs
        .iter()
        .find(|e| matches!(e.plan.role, Role::Validator { .. }))
        .copied();
    let checker = runs
        .iter()
        .find(|e| matches!(e.plan.role, Role::Checker { .. }))
        .copied();
    let profile = |role| {
        roles
            .iter()
            .find(|r| r.role == role)
            .and_then(|r| r.profile)
            .expect("planned judge profile")
    };
    // Report §4.6 outcomes, by the same profile reading as B5–B7.
    let validation = validator.map(|v| {
        let validator_profile = profile(r::RoleName::Validator);
        r::Validation {
            execution: v.plan.index,
            profile: validator_profile,
            outcome: match (v.result.coverage, v.result.class.as_ref()) {
                (Coverage::Started, Some(RunClass::ExitedZero)) => r::ValidationOutcome::Valid,
                (
                    Coverage::Started,
                    Some(
                        class @ (RunClass::Signal { .. }
                        | RunClass::NonzeroExit { .. }
                        | RunClass::WallDeadline
                        | RunClass::OutputBudget { .. }),
                    ),
                ) => {
                    if validator_profile == r::JudgeProfile::Testlib {
                        r::ValidationOutcome::ValidationFailed
                    } else if matches!(class, RunClass::NonzeroExit { code: 1 }) {
                        r::ValidationOutcome::Invalid
                    } else {
                        r::ValidationOutcome::ValidatorFailed
                    }
                }
                _ => r::ValidationOutcome::NotRun,
            },
            stderr: v.judge_stderr.clone(),
        }
    });
    let v = validator.map(|v| input(v, profile(r::RoleName::Validator)));
    let c = checker.map(|c| input(c, profile(r::RoleName::Checker)));
    let output_file = context.output_file(solution);
    let (admission, comparison) = if checker.is_none()
        && v.as_ref()
            .is_none_or(|v| v.execution.class == Some(RunClass::ExitedZero))
    {
        context.comparison(solution, &test.answer, comparator)
    } else {
        (r::CheckAdmission::NoIntent, r::Comparison::NotRun)
    };
    #[cfg(any(test, feature = "storage-test-hooks"))]
    context.phase_point(super::Point::BeforeTestVerdict(solution.plan.index));
    let (check, verdict) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _state = context.cancel.lock();
        r::batch_outcome(&r::BatchInput {
            validator: v.as_ref(),
            solution: &solution.result,
            checker: c.as_ref(),
            scoring: config.scoring,
            admission,
            answer_present: matches!(test.answer, FrozenAnswer::Present { .. }),
            comparison: &comparison,
            output_file: output_file.as_ref(),
        })
    }))
    .unwrap_or((
        r::CheckOutcome {
            kind: r::CheckKind::InternalError {
                invariant: r::Invariant::CheckEvidence,
            },
            diagnostic: false,
        },
        r::TestVerdict::InternalError {
            invariant: r::Invariant::TestCheck,
        },
    ));
    let judges = checker
        .zip(c.as_ref())
        .map(|(execution, input)| r::Judge {
            role: r::JudgeRole::Checker,
            execution: execution.plan.index,
            profile: input.profile,
            result: own_result(input, config.scoring),
            message: execution
                .judge_evidence
                .as_ref()
                .and_then(|e| e.message.clone()),
            stderr: execution.judge_stderr.clone(),
            report: execution.judge_evidence.as_ref().map(|e| e.report.clone()),
        })
        .into_iter()
        .collect();
    let raw = match &check.kind {
        r::CheckKind::Judged { outcome } => match outcome.as_ref() {
            r::JudgedCheck::Scored { raw, .. } => Some(*raw),
            _ => None,
        },
        _ => None,
    };
    let score = r::test_score(config.scoring, &verdict, raw).ok().flatten();
    TestReport {
        frozen: test,
        first_execution: runs[0].plan.index,
        execution_count: runs.len() as u32,
        class: context.test_result(solution).class,
        check: Some(check),
        verdict: Some(verdict),
        output_file,
        score,
        validation,
        judges,
    }
}

pub(super) fn display(
    test: &TestReport,
    executions: &[ExecutionReport],
    comparator: &Comparator,
    target: &crate::config::TargetPlan,
    config: &r::CheckConfig,
    io: Option<&r::FileIo>,
) -> r::DisplayLabel {
    let solution = executions.iter().find(|e| {
        e.plan.test.as_ref().is_some_and(|t| t.id == test.frozen.id)
            && matches!(e.plan.role, Role::Test { .. })
    });
    let judged = test
        .judges
        .first()
        .and_then(|judge| {
            r::JudgedDisplayEvidence::from_judge(
                judge,
                executions
                    .iter()
                    .find(|e| e.plan.index == judge.execution)
                    .and_then(|e| e.result.class.clone()),
            )
        })
        .map(Box::new);
    r::label(
        r::LabelSubject::Test {
            verdict: test.verdict.as_ref().expect("derived test"),
            class: test.class.as_ref(),
            check: test.check.as_ref(),
        },
        &r::DisplayContext {
            output_file: io
                .and_then(|io| io.output.as_ref())
                .zip(test.output_file.as_ref())
                .map(|(name, output)| (name.as_str().into(), output.clone())),
            judged,
            comparator: (config.kind == r::CheckMode::Compare)
                .then(|| r::comparator_token(comparator)),
            test_points: match (test.score, config.scoring) {
                (Some(raw), r::Scoring::Points { scale, .. }) => Some((raw, scale)),
                _ => None,
            },
            budgets: vec![
                (r::Stream::Stdout, target.run.stdout.bytes.value),
                (r::Stream::Stderr, target.run.stderr.bytes.value),
            ],
            cleanup_unverified: solution
                .and_then(|s| s.summary.as_ref())
                .is_some_and(|s| s.notes.contains(&r::RunNote::CleanupUnverified)),
            wall_deadline: solution.and_then(|s| s.summary.as_ref().map(|s| s.deadline)),
            measured_wall: solution
                .and_then(|s| s.evidence.as_deref())
                .and_then(|e| match e {
                    super::ExecutionRecord::Started(root) => root.timing.wall,
                    _ => None,
                }),
            ..r::DisplayContext::default()
        },
    )
}
