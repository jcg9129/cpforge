//! Pure first-matching-row derivations from report §§2.1–2.7.

use super::OutputFile;
use super::types::*;
use crate::compare::{self, Diag};
use crate::supervisor::{CaptureLoss, Cleanup, StopTrigger, Termination};

/// Derive a class only when a finalized record exists. Missing records use
/// `ExecutionResult { class: None, .. }`, never a fabricated `RunInput`.
pub fn classify_run(record: &RunInput) -> RunClass {
    let mut causes = Vec::new();
    if let ExecutionEvidence::NotStarted(reason) = record.execution {
        if reason != NotStartedReason::CancelledBeforeStart {
            causes.push(InfrastructureCause::NotStarted { reason });
        }
    }
    if matches!(
        record.execution,
        ExecutionEvidence::Started(Termination::Unknown)
    ) {
        causes.push(InfrastructureCause::StatusUnknown);
    }
    if let Cleanup::Incomplete { survivors } = record.cleanup {
        causes.push(InfrastructureCause::CleanupIncomplete { survivors });
    }
    causes.extend(
        record
            .supervisor_errors
            .iter()
            .map(|&error| InfrastructureCause::SupervisorError { error }),
    );
    for loss in &record.stdout.losses {
        let loss = match loss {
            CaptureLoss::BudgetDiscard => continue,
            CaptureLoss::WriteError(_) => LossKind::WriteError,
            CaptureLoss::WriteStalled => LossKind::WriteStalled,
            CaptureLoss::PipeRead(_) => LossKind::PipeRead,
            CaptureLoss::NoEofBeforeDrainDeadline => LossKind::NoEofBeforeDrainDeadline,
            CaptureLoss::ForwardIncomplete => LossKind::ForwardIncomplete,
        };
        causes.push(InfrastructureCause::CaptureLoss {
            stream: Stream::Stdout,
            loss,
        });
    }
    if !causes.is_empty() {
        return RunClass::Infrastructure {
            causes: InfrastructureCauses::new(causes),
        };
    }
    if matches!(
        record.execution,
        ExecutionEvidence::Transferred
            | ExecutionEvidence::NotStarted(NotStartedReason::CancelledBeforeStart)
    ) || record.stop_trigger == Some(StopTrigger::Cancelled)
    {
        return RunClass::Cancelled;
    }
    if record.wall_violation {
        return RunClass::WallDeadline;
    }
    let stdout = record.stdout.breached()
        && (record.consumes_stdout || record.stdout.kill_on_breach)
        || record.budget_violations.stdout && record.stdout.kill_on_breach
        || record.stop_trigger
            == Some(StopTrigger::OutputBudget(crate::supervisor::Stream::Stdout));
    let stderr = (record.stderr.breached() || record.budget_violations.stderr)
        && record.stderr.kill_on_breach
        || record.stop_trigger
            == Some(StopTrigger::OutputBudget(crate::supervisor::Stream::Stderr));
    if stdout || stderr {
        return RunClass::OutputBudget {
            streams: BudgetStreams {
                stdout,
                stderr,
                output_file: false,
            },
        };
    }
    match record.execution {
        ExecutionEvidence::Started(Termination::Signalled {
            signal,
            core_dumped,
        }) => RunClass::Signal {
            signal,
            core_dumped,
        },
        ExecutionEvidence::Started(Termination::Exited { code: 0 }) => RunClass::ExitedZero,
        ExecutionEvidence::Started(Termination::Exited { code }) => RunClass::NonzeroExit { code },
        // These cases were consumed by rows 1 and 2.
        ExecutionEvidence::Started(Termination::Unknown)
        | ExecutionEvidence::NotStarted(_)
        | ExecutionEvidence::Transferred => {
            unreachable!("earlier classification row")
        }
    }
}

/// Notes for informative observations that do not change the run class.
/// The record's original supervisor notes remain the caller's evidence.
pub fn run_notes(record: &RunInput) -> Vec<RunNote> {
    let mut notes = Vec::new();
    if matches!(record.cleanup, Cleanup::Unverified { .. }) {
        notes.push(RunNote::CleanupUnverified);
    }
    if record.cwd_overflow_bytes.is_some() {
        notes.push(RunNote::CwdOverflow);
    }
    if record.stdout.breached() && !record.consumes_stdout && !record.stdout.kill_on_breach {
        notes.push(RunNote::TruncatedStdout);
    }
    if record.stderr.breached() && !record.stderr.kill_on_breach {
        notes.push(RunNote::TruncatedStderr);
    }
    notes
}

/// Plan facts required to validate skipped coverage. These describe the
/// particular execution, including its dependency and earlier plan entries.
#[derive(Clone, Copy, Debug)]
pub struct SkipConditions {
    pub job: JobKind,
    pub depends_on_unsucceeded_build: bool,
    pub earlier_retention_refusal: bool,
    pub keep_going: bool,
    pub earlier_primary_failed: bool,
    pub intent_before_admission: bool,
    pub after_test_hook: bool,
    pub hooks_eligible: bool,
}

/// Validate skipped conditions without altering the observed coverage or class.
/// Use this before aggregating results; a caller-supplied invariant is retained.
pub fn validate_execution(
    mut execution: ExecutionResult,
    conditions: &SkipConditions,
) -> ExecutionResult {
    if execution.invariant.is_none() {
        execution.invariant = execution_invariant(&execution);
    }
    if execution.invariant.is_none() {
        if let Coverage::Skipped { reason } = execution.coverage {
            let legal = match reason {
                SkipReason::BuildNotSucceeded => conditions.depends_on_unsucceeded_build,
                SkipReason::AdmissionClosed => conditions.earlier_retention_refusal,
                SkipReason::StopPolicy => {
                    conditions.job == JobKind::Cmd
                        && !conditions.keep_going
                        && conditions.earlier_primary_failed
                }
                SkipReason::Cancelled => conditions.intent_before_admission,
                SkipReason::JobNotEligible => {
                    conditions.job == JobKind::Test
                        && conditions.after_test_hook
                        && !conditions.hooks_eligible
                }
            };
            if !legal {
                execution.invariant = Some(Invariant::SkipCondition);
            }
        }
    }
    execution
}

/// Structural evidence failures, independent of the job's plan conditions.
pub fn execution_invariant(execution: &ExecutionResult) -> Option<Invariant> {
    if let Some(invariant) = execution.invariant {
        return Some(invariant);
    }
    match (execution.coverage, execution.class.as_ref()) {
        (Coverage::Admitted, _) => Some(Invariant::RecordMissing),
        (Coverage::Skipped { .. }, None) => None,
        (Coverage::Started | Coverage::NotStarted { .. } | Coverage::Transferred, None) => {
            Some(Invariant::SummaryUnderivable)
        }
        (Coverage::Started, Some(class)) if valid_class(class) => None,
        (Coverage::Transferred, Some(RunClass::Cancelled)) => None,
        (
            Coverage::NotStarted {
                reason: NotStartedReason::CancelledBeforeStart,
            },
            Some(RunClass::Cancelled),
        ) => None,
        (Coverage::NotStarted { reason }, Some(class @ RunClass::Infrastructure { .. }))
            if reason != NotStartedReason::CancelledBeforeStart && valid_class(class) =>
        {
            None
        }
        _ => Some(Invariant::CoverageClass),
    }
}
fn valid_class(class: &RunClass) -> bool {
    match class {
        RunClass::Infrastructure { causes } => causes.primary().is_some(),
        RunClass::OutputBudget { streams } => streams.primary().is_some(),
        RunClass::NonzeroExit { code } => *code != 0,
        _ => true,
    }
}

/// Producer class precedes post-build observations. No filesystem is inspected.
pub fn build_result(execution: &ExecutionResult, post: &PostBuild) -> BuildResult {
    if let Some(invariant) = execution_invariant(execution) {
        return BuildResult::InternalError { invariant };
    }
    if let Coverage::Skipped { reason } = execution.coverage {
        return if reason == SkipReason::Cancelled {
            BuildResult::Cancelled
        } else {
            BuildResult::InternalError {
                invariant: Invariant::BuildCoverage,
            }
        };
    }
    match execution.class.as_ref() {
        Some(RunClass::Infrastructure { causes }) => BuildResult::Infrastructure {
            causes: causes.clone(),
        },
        Some(RunClass::Cancelled) => BuildResult::Cancelled,
        Some(RunClass::ExitedZero) => {
            if let Some(invariant) = post.invariant {
                return BuildResult::InternalError { invariant };
            }
            if !post.missing_outputs.is_empty() {
                let names_dropped = post
                    .missing_outputs
                    .len()
                    .saturating_sub(16)
                    .min(u32::MAX as usize) as u32;
                BuildResult::Failed {
                    cause: BuildCause::MissingOutput {
                        names: post.missing_outputs.iter().take(16).cloned().collect(),
                        names_dropped,
                    },
                }
            } else if let Some((name, reason)) = &post.generated_program_invalid {
                BuildResult::Failed {
                    cause: BuildCause::GeneratedProgramInvalid {
                        name: name.clone(),
                        reason: reason.clone(),
                    },
                }
            } else if let Some((bytes, cap)) = post.output_too_large {
                if bytes <= cap {
                    return BuildResult::InternalError {
                        invariant: Invariant::BuildEvidence,
                    };
                }
                BuildResult::Failed {
                    cause: BuildCause::OutputTooLarge { bytes, cap },
                }
            } else {
                BuildResult::Succeeded
            }
        }
        Some(class) => BuildResult::Failed {
            cause: BuildCause::Run(class.clone()),
        },
        None => BuildResult::InternalError {
            invariant: Invariant::BuildCoverage,
        },
    }
}

/// The state machine consumes a completed comparator outcome or an explicit
/// I/O/panic observation. `answer_present` includes an empty frozen answer.
/// Admission intent precedes answer absence, and intent after admission is inert.
pub fn check_outcome(
    execution: &ExecutionResult,
    admission: CheckAdmission,
    answer_present: bool,
    comparison: &Comparison,
) -> CheckOutcome {
    let outcome = |kind, diagnostic| CheckOutcome { kind, diagnostic };
    if let Some(invariant) = execution_invariant(execution) {
        return outcome(CheckKind::InternalError { invariant }, false);
    }
    if let Comparison::InternalError { invariant } = comparison {
        let diagnostic = matches!(execution.coverage, Coverage::Started)
            && execution.class.as_ref().is_some_and(RunClass::diagnostic)
            && admission != CheckAdmission::IntentBeforeAdmission;
        return outcome(
            CheckKind::InternalError {
                invariant: *invariant,
            },
            diagnostic,
        );
    }
    if let Coverage::Skipped { reason } = execution.coverage {
        return outcome(
            CheckKind::NotAdmitted {
                reason: if reason == SkipReason::Cancelled {
                    CheckNotAdmitted::Cancelled
                } else {
                    CheckNotAdmitted::RunNotEligible
                },
            },
            false,
        );
    }
    let Some(class) = &execution.class else {
        return outcome(
            CheckKind::InternalError {
                invariant: Invariant::CheckEvidence,
            },
            false,
        );
    };
    if matches!(execution.coverage, Coverage::NotStarted { .. })
        || matches!(class, RunClass::Infrastructure { .. } | RunClass::Cancelled)
    {
        return outcome(
            CheckKind::NotAdmitted {
                reason: CheckNotAdmitted::RunNotEligible,
            },
            false,
        );
    }
    if admission == CheckAdmission::IntentBeforeAdmission {
        return outcome(
            CheckKind::NotAdmitted {
                reason: CheckNotAdmitted::Cancelled,
            },
            false,
        );
    }
    let diagnostic = class.diagnostic();
    if !answer_present {
        return outcome(CheckKind::Unchecked, diagnostic);
    }
    let kind = match comparison {
        Comparison::Outcome(compare::CheckOutcome::Passed) => CheckKind::Passed,
        Comparison::Outcome(compare::CheckOutcome::Failed(diag))
            if !matches!(**diag, Diag::InvalidAnswer { .. }) =>
        {
            CheckKind::Failed { diag: diag.clone() }
        }
        Comparison::Outcome(compare::CheckOutcome::CheckerFailed(diag))
            if matches!(**diag, Diag::InvalidAnswer { .. }) =>
        {
            CheckKind::CheckerFailed { diag: diag.clone() }
        }
        Comparison::IoError { os_error } => CheckKind::CheckError {
            os_error: *os_error,
        },
        Comparison::InternalError { invariant } => CheckKind::InternalError {
            invariant: *invariant,
        },
        _ => CheckKind::InternalError {
            invariant: Invariant::CheckEvidence,
        },
    };
    outcome(kind, diagnostic)
}

/// The execution class remains evidence; only the test class is raised (§2.15.1).
pub fn file_test_class(class: &RunClass, output: &OutputFile) -> RunClass {
    if !matches!(output, OutputFile::TooLarge { .. }) {
        return class.clone();
    }
    match class {
        RunClass::OutputBudget { streams } => RunClass::OutputBudget {
            streams: BudgetStreams {
                output_file: true,
                ..*streams
            },
        },
        RunClass::ExitedZero | RunClass::NonzeroExit { .. } | RunClass::Signal { .. } => {
            RunClass::OutputBudget {
                streams: BudgetStreams {
                    stdout: false,
                    stderr: false,
                    output_file: true,
                },
            }
        }
        _ => class.clone(),
    }
}

/// Collection precedes answer absence and comparison, but never check admission.
/// This adapter also returns the widened test class without mutating the record.
pub fn file_test_outcome(
    execution: &ExecutionResult,
    admission: CheckAdmission,
    answer_present: bool,
    comparison: &Comparison,
    output: &OutputFile,
) -> (ExecutionResult, CheckOutcome, TestVerdict) {
    let mut test = execution.clone();
    test.class = test
        .class
        .as_ref()
        .map(|class| file_test_class(class, output));
    let mut check = check_outcome(&test, admission, false, &Comparison::NotRun);
    if matches!(check.kind, CheckKind::Unchecked) {
        if matches!(output, OutputFile::Collected { .. }) {
            check = check_outcome(&test, admission, answer_present, comparison);
        } else {
            check.kind = match (output.rejection_reason(), output) {
                (Some(reason), _) => CheckKind::OutputRejected { reason },
                (_, OutputFile::NotCollected { os_error, .. }) => CheckKind::CheckError {
                    os_error: *os_error,
                },
                _ => CheckKind::InternalError {
                    invariant: Invariant::CheckEvidence,
                },
            };
        }
    }
    let verdict = test_verdict(&test, &check);
    (test, check, verdict)
}

/// Total over representable inputs. Illegal combinations return an internal
/// verdict with an invariant; no ordinary verdict is guessed for an orphan.
pub fn test_verdict(execution: &ExecutionResult, check: &CheckOutcome) -> TestVerdict {
    let internal = |invariant| TestVerdict::InternalError { invariant };
    // Internal checks dominate even diagnostic checks (§2.5 row 3).
    if let CheckKind::InternalError { invariant } = check.kind {
        return internal(invariant);
    }
    if let Some(invariant) = execution_invariant(execution) {
        return internal(invariant);
    }
    if let Coverage::Skipped { reason } = execution.coverage {
        if check.diagnostic {
            return internal(Invariant::DiagnosticFlag);
        }
        return match (reason, &check.kind) {
            (
                SkipReason::Cancelled,
                CheckKind::NotAdmitted {
                    reason: CheckNotAdmitted::Cancelled,
                },
            ) => TestVerdict::Cancelled,
            (
                SkipReason::BuildNotSucceeded | SkipReason::AdmissionClosed,
                CheckKind::NotAdmitted {
                    reason: CheckNotAdmitted::RunNotEligible,
                },
            ) => TestVerdict::NotRun { reason },
            (SkipReason::StopPolicy | SkipReason::JobNotEligible, _) => {
                internal(Invariant::TestCoverage)
            }
            _ => internal(Invariant::TestCheck),
        };
    }
    let Some(class) = &execution.class else {
        return internal(Invariant::SummaryUnderivable);
    };
    if matches!(class, RunClass::Infrastructure { .. } | RunClass::Cancelled) {
        if check.diagnostic {
            return internal(Invariant::DiagnosticFlag);
        }
        return match (&check.kind, class) {
            (
                CheckKind::NotAdmitted {
                    reason: CheckNotAdmitted::RunNotEligible,
                },
                RunClass::Infrastructure { .. },
            ) => TestVerdict::Infrastructure,
            (
                CheckKind::NotAdmitted {
                    reason: CheckNotAdmitted::RunNotEligible,
                },
                RunClass::Cancelled,
            ) => TestVerdict::Cancelled,
            _ => internal(Invariant::TestCheck),
        };
    }
    if matches!(
        check.kind,
        CheckKind::NotAdmitted {
            reason: CheckNotAdmitted::Cancelled
        }
    ) {
        return if check.diagnostic {
            internal(Invariant::DiagnosticFlag)
        } else {
            TestVerdict::Cancelled
        };
    }
    if check.diagnostic != class.diagnostic() {
        return internal(Invariant::DiagnosticFlag);
    }
    let ordinary = match &check.kind {
        CheckKind::Unchecked
        | CheckKind::Passed
        | CheckKind::CheckError { .. }
        | CheckKind::OutputRejected { .. } => true,
        CheckKind::Failed { diag } => !matches!(**diag, Diag::InvalidAnswer { .. }),
        CheckKind::CheckerFailed { diag } => matches!(**diag, Diag::InvalidAnswer { .. }),
        _ => false,
    };
    if !ordinary {
        return internal(Invariant::TestCheck);
    }
    match class {
        RunClass::WallDeadline => TestVerdict::WallDeadline,
        RunClass::OutputBudget { .. } => TestVerdict::OutputBudget,
        RunClass::Signal { .. } => TestVerdict::Signal,
        RunClass::NonzeroExit { .. } => TestVerdict::NonzeroExit,
        RunClass::ExitedZero => match &check.kind {
            CheckKind::CheckError { .. } => TestVerdict::Infrastructure,
            CheckKind::CheckerFailed { .. } => TestVerdict::CheckerFailed,
            CheckKind::Failed { diag } if matches!(**diag, Diag::IncompleteOutput) => {
                internal(Invariant::IncompleteZeroExit)
            }
            CheckKind::Failed { .. } | CheckKind::OutputRejected { .. } => TestVerdict::WrongAnswer,
            CheckKind::Unchecked => TestVerdict::Unchecked,
            CheckKind::Passed => TestVerdict::Passed,
            _ => internal(Invariant::TestCheck),
        },
        _ => internal(Invariant::TestCheck),
    }
}

/// Command failure for stop policy: a record exists and its class is not zero.
/// Missing or underivable evidence returns an invariant, not success/failure.
pub fn command_failed(execution: &CommandOutcome) -> Result<bool, Invariant> {
    if let Some(invariant) = execution_invariant(execution) {
        return Err(invariant);
    }
    Ok(execution
        .class
        .as_ref()
        .is_some_and(|class| !matches!(class, RunClass::ExitedZero)))
}
pub fn hook_outcome(execution: &ExecutionResult) -> HookOutcome {
    if let Some(invariant) = execution_invariant(execution) {
        return HookOutcome::InternalError { invariant };
    }
    if let Coverage::Skipped { reason } = execution.coverage {
        return HookOutcome::Skipped { reason };
    }
    match &execution.class {
        Some(RunClass::ExitedZero) => HookOutcome::Succeeded,
        Some(RunClass::Cancelled) => HookOutcome::Cancelled,
        Some(class) => HookOutcome::Failed {
            class: class.clone(),
        },
        None => HookOutcome::InternalError {
            invariant: Invariant::SummaryUnderivable,
        },
    }
}

/// Primary executions include the build and, for `hook` jobs, the hooks.
/// `after_test` executions belong only in `coverage_counts`' hooks population.
#[derive(Clone, Copy, Debug)]
pub struct JobInput<'a> {
    pub kind: JobKind,
    pub build: Option<&'a BuildResult>,
    pub primary: &'a [ExecutionResult],
    pub tests: &'a [TestVerdict],
    pub selected_tests: u32,
    pub internal_before_freeze: Option<&'a InternalFailure>,
    pub internal_after_freeze: Option<&'a InternalFailure>,
}
/// Compute the primary verdict and final override separately. Cancellation
/// intent by itself is deliberately absent: only affected evidence changes V.
pub fn job_verdict(input: &JobInput<'_>) -> JobResult {
    let primary_verdict = primary_verdict(input);
    let internal = input
        .internal_before_freeze
        .cloned()
        .or_else(|| {
            // In the absence of a recorded failure, discover malformed evidence in
            // plan order. Callers supply the first recorded failure when available.
            input
                .primary
                .iter()
                .enumerate()
                .find_map(|(index, execution)| {
                    execution_invariant(execution).map(|invariant| InternalFailure {
                        invariant,
                        locus: InternalLocus::Execution {
                            index: index as u32,
                        },
                    })
                })
        })
        .or(match input.build {
            Some(BuildResult::InternalError { invariant }) => Some(InternalFailure {
                invariant: *invariant,
                locus: InternalLocus::Build,
            }),
            _ => None,
        })
        .or_else(|| {
            input.tests.iter().find_map(|test| match test {
                TestVerdict::InternalError { invariant } => Some(InternalFailure {
                    invariant: *invariant,
                    locus: InternalLocus::Job,
                }),
                _ => None,
            })
        })
        .or_else(|| {
            (primary_verdict == JobVerdict::InternalError).then_some(InternalFailure {
                invariant: Invariant::JobEvidence,
                locus: InternalLocus::Job,
            })
        })
        .or_else(|| input.internal_after_freeze.cloned());
    JobResult {
        primary_verdict,
        verdict: if input.internal_after_freeze.is_some() {
            JobVerdict::InternalError
        } else {
            primary_verdict
        },
        internal,
    }
}
fn primary_verdict(input: &JobInput<'_>) -> JobVerdict {
    if input.kind == JobKind::Build && input.build.is_none() && !input.primary.is_empty()
        || input.internal_before_freeze.is_some()
        || input
            .primary
            .iter()
            .any(|e| execution_invariant(e).is_some())
        || matches!(input.build, Some(BuildResult::InternalError { .. }))
        || input
            .tests
            .iter()
            .any(|v| matches!(v, TestVerdict::InternalError { .. }))
        || input.kind != JobKind::Test && !input.tests.is_empty()
        || input.kind == JobKind::Test && (input.selected_tests == 0) != input.tests.is_empty()
    {
        return JobVerdict::InternalError;
    }
    let has_class = |token: &str| {
        input
            .primary
            .iter()
            .any(|e| e.class.as_ref().is_some_and(|c| c.wire_token() == token))
    };
    let has_test = |token: &str| input.tests.iter().any(|v| v.wire_token() == token);
    let relevant = |token: &str| {
        if input.kind == JobKind::Test {
            has_test(token)
        } else {
            has_class(token)
        }
    };
    if matches!(input.build, Some(BuildResult::Infrastructure { .. })) || relevant("infrastructure")
    {
        return JobVerdict::Infrastructure;
    }
    if matches!(input.build, Some(BuildResult::Cancelled))
        || relevant("cancelled")
        || input.primary.iter().any(|e| e.coverage.is_cancelled())
    {
        return JobVerdict::Cancelled;
    }
    if matches!(input.build, Some(BuildResult::Failed { .. })) {
        return JobVerdict::BuildFailed;
    }
    for (token, verdict) in [
        ("wall_deadline", JobVerdict::WallDeadline),
        ("output_budget", JobVerdict::OutputBudget),
        ("signal", JobVerdict::Signal),
        ("nonzero_exit", JobVerdict::NonzeroExit),
        ("checker_failed", JobVerdict::CheckerFailed),
        ("wrong_answer", JobVerdict::WrongAnswer),
        ("unchecked", JobVerdict::Unchecked),
    ] {
        if relevant(token) {
            return verdict;
        }
    }
    if input.kind == JobKind::Test {
        if input.selected_tests == 0 {
            JobVerdict::NoTests
        } else if input.tests.iter().all(|v| matches!(v, TestVerdict::Passed)) {
            JobVerdict::Passed
        } else {
            JobVerdict::InternalError
        }
    } else {
        JobVerdict::Succeeded
    }
}

/// Counts are over the bounded plan (at most 10,000 executions). Invalidity is
/// orthogonal to coverage and class; neither observation is rewritten.
pub fn coverage_counts(
    primary: &[ExecutionResult],
    hooks: &[ExecutionResult],
    tests: &[TestVerdict],
) -> Counts {
    fn cov(executions: &[ExecutionResult]) -> Cov {
        let mut counts = Cov::default();
        for execution in executions {
            counts.planned += 1;
            match execution.coverage {
                Coverage::Started => counts.started += 1,
                Coverage::Transferred => counts.transferred += 1,
                Coverage::NotStarted { .. } => counts.not_started += 1,
                Coverage::Skipped { .. } => counts.skipped += 1,
                Coverage::Admitted => counts.admitted += 1,
            }
            if execution_invariant(execution).is_some() {
                counts.invalid += 1;
            }
        }
        counts
    }
    let mut counts = Counts {
        primary: cov(primary),
        hooks: cov(hooks),
        ..Counts::default()
    };
    counts.partial = counts.primary.started != counts.primary.planned;
    for execution in primary {
        let field = match &execution.class {
            Some(RunClass::ExitedZero) => &mut counts.by_class.exited_zero,
            Some(RunClass::NonzeroExit { .. }) => &mut counts.by_class.nonzero_exit,
            Some(RunClass::Signal { .. }) => &mut counts.by_class.signal,
            Some(RunClass::WallDeadline) => &mut counts.by_class.wall_deadline,
            Some(RunClass::OutputBudget { .. }) => &mut counts.by_class.output_budget,
            Some(RunClass::Cancelled) => &mut counts.by_class.cancelled,
            Some(RunClass::Infrastructure { .. }) => &mut counts.by_class.infrastructure,
            None if matches!(execution.coverage, Coverage::Skipped { .. }) => {
                &mut counts.by_class.skipped
            }
            None => &mut counts.by_class.unknown,
        };
        *field += 1;
    }
    for verdict in tests {
        counts.tests += 1;
        let field = match verdict {
            TestVerdict::Passed => &mut counts.by_verdict.passed,
            TestVerdict::WrongAnswer => &mut counts.by_verdict.wrong_answer,
            TestVerdict::Unchecked => &mut counts.by_verdict.unchecked,
            TestVerdict::CheckerFailed => &mut counts.by_verdict.checker_failed,
            TestVerdict::WallDeadline => &mut counts.by_verdict.wall_deadline,
            TestVerdict::OutputBudget => &mut counts.by_verdict.output_budget,
            TestVerdict::Signal => &mut counts.by_verdict.signal,
            TestVerdict::NonzeroExit => &mut counts.by_verdict.nonzero_exit,
            TestVerdict::Cancelled => &mut counts.by_verdict.cancelled,
            TestVerdict::Infrastructure => &mut counts.by_verdict.infrastructure,
            TestVerdict::NotRun { .. } => &mut counts.by_verdict.not_run,
            TestVerdict::InternalError { .. } => &mut counts.by_verdict.internal_error,
        };
        *field += 1;
    }
    counts
}
