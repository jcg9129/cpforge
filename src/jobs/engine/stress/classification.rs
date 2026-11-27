//! Pure batch retirement classification (stress §4.1). Judging and comparison
//! supply finalized observations; admission and cleanup never invent verdicts.
use super::{BatchRole, IterationClass, LiveIteration};
use crate::report::{self, CheckKind, RunClass};

#[derive(Clone, Debug)]
pub struct BatchRunEvidence {
    pub role: BatchRole,
    pub class: RunClass,
    pub stdout_complete: bool,
}

#[derive(Debug)]
pub struct BatchEvidence<'a> {
    pub runs: &'a [BatchRunEvidence],
    pub check: Option<&'a report::CheckOutcome>,
    pub validation: Option<report::ValidationOutcome>,
    pub invalid_answer: bool,
    pub min_score: Option<report::Score>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchDecision {
    pub class: IterationClass,
    pub role: Option<BatchRole>,
    pub reason: &'static str,
    pub invariant: bool,
}

fn decision(class: IterationClass, role: Option<BatchRole>, reason: &'static str) -> BatchDecision {
    BatchDecision {
        class,
        role,
        reason,
        invariant: false,
    }
}

fn orphan() -> BatchDecision {
    BatchDecision {
        invariant: true,
        ..decision(IterationClass::Infrastructure, None, "invariant")
    }
}

impl<T: Clone> LiveIteration<'_, T> {
    /// R2 runs under the same verdict lock as admission. A late intent alone
    /// does not cancel finished evidence: the skipped run/check or a cancelled
    /// record must witness it. R3/R4 use `begin_retirement(decision.class)`.
    pub fn classify_batch(&self, evidence: &BatchEvidence<'_>) -> BatchDecision {
        let mut state = self.owner.cancel.lock();
        let result = classify_batch(evidence, self.cancelled);
        if result.invariant && state.internal.is_none() {
            state.internal = Some(report::InternalFailure {
                invariant: report::Invariant::JobEvidence,
                locus: report::InternalLocus::Job,
            });
        }
        result
    }
}

pub fn classify_batch(evidence: &BatchEvidence<'_>, cancelled: Option<BatchRole>) -> BatchDecision {
    use BatchRole as R;
    use IterationClass as C;
    if evidence.runs.len() > 5
        || evidence
            .runs
            .windows(2)
            .any(|p| p[0].role.index() >= p[1].role.index())
    {
        return orphan();
    }
    let find = |role: BatchRole| evidence.runs.iter().find(|run| run.role == role);
    let check_role = if find(R::Checker).is_some() {
        R::Checker
    } else if find(R::Candidate).is_some() {
        R::Candidate
    } else {
        R::Brute
    };
    // S1 precedes cancellation, even when a later run was never admitted.
    for run in evidence.runs {
        if let RunClass::Infrastructure { causes } = &run.class {
            let Some(cause) = causes.primary() else {
                return orphan();
            };
            let reason = match cause {
                report::InfrastructureCause::NotStarted {
                    reason: report::NotStartedReason::RetentionBudget,
                } => "retention_budget",
                _ => cause.wire_token(),
            };
            return decision(C::Infrastructure, Some(run.role), reason);
        }
    }
    if let Some(check) = evidence.check {
        match &check.kind {
            CheckKind::CheckError { .. } => {
                return decision(C::Infrastructure, Some(check_role), "check_error");
            }
            CheckKind::InternalError { .. } => return orphan(),
            _ => {}
        }
    }
    // S2: explicit witnesses only. Transferred creation reduces to Cancelled.
    if let Some(run) = evidence
        .runs
        .iter()
        .find(|r| r.class == RunClass::Cancelled)
    {
        return decision(C::Cancelled, Some(run.role), "cancelled");
    }
    if let Some(role) = cancelled {
        return decision(
            C::Cancelled,
            (!evidence.runs.is_empty()).then_some(role),
            "cancelled",
        );
    }
    if evidence.check.is_some_and(|check| match &check.kind {
        CheckKind::NotAdmitted {
            reason: report::CheckNotAdmitted::Cancelled,
        } => true,
        CheckKind::Judged { outcome } => matches!(
            outcome.as_ref(),
            report::JudgedCheck::Unchecked {
                reason: report::CheckingUnchecked::CheckingInterrupted,
            }
        ),
        _ => false,
    }) {
        return decision(C::Cancelled, Some(check_role), "cancelled");
    }
    let Some(generator) = find(R::Generator) else {
        return orphan();
    };
    // S3-S5: only a complete generator/valid input/valid oracle answer permits
    // downstream evidence. An impossible combination is an internal orphan.
    if generator.class != RunClass::ExitedZero {
        return decision(
            C::GeneratorFailure,
            Some(R::Generator),
            generator.class.wire_token(),
        );
    }
    if !generator.stdout_complete {
        return orphan();
    }
    if let Some(validator) = find(R::Validator) {
        let reason = match evidence.validation {
            Some(report::ValidationOutcome::Valid) if validator.class == RunClass::ExitedZero => {
                None
            }
            Some(report::ValidationOutcome::Invalid) => Some("invalid_input"),
            Some(report::ValidationOutcome::ValidationFailed) => Some("validation_failed"),
            // B7's `<class reason>`: the class token, `exit_code` for a
            // nonzero exit (report contract §2.11).
            Some(report::ValidationOutcome::ValidatorFailed) if validator.class.diagnostic() => {
                Some(match validator.class {
                    RunClass::NonzeroExit { .. } => "exit_code",
                    _ => validator.class.wire_token(),
                })
            }
            _ => return orphan(),
        };
        if let Some(reason) = reason {
            return decision(C::ValidatorFailure, Some(R::Validator), reason);
        }
    } else if evidence.validation.is_some() {
        return orphan();
    }
    if let Some(brute) = find(R::Brute) {
        if brute.class != RunClass::ExitedZero {
            return decision(C::OracleFailure, Some(R::Brute), brute.class.wire_token());
        }
        if !brute.stdout_complete {
            return orphan();
        }
        if evidence.invalid_answer {
            return decision(C::OracleFailure, Some(R::Brute), "invalid_answer");
        }
    } else if evidence.invalid_answer {
        return orphan();
    }
    let Some(candidate) = find(R::Candidate) else {
        return orphan();
    };
    // S6 is checked before the candidate's failure. Check evidence comes from
    // the existing simple/testlib reducer, never from the checker's exit alone.
    if let Some(report::CheckOutcome {
        kind: CheckKind::Judged { outcome },
        ..
    }) = evidence.check
    {
        match outcome.as_ref() {
            report::JudgedCheck::JudgeFailed { reason, .. } => {
                return decision(C::JudgeFailure, Some(R::Checker), judge_reason(*reason));
            }
            report::JudgedCheck::Unchecked {
                reason: report::CheckingUnchecked::CheckingUnverified,
            } => return decision(C::JudgeFailure, Some(R::Checker), "checking_unverified"),
            _ => {}
        }
    }
    // S7.
    if candidate.class.diagnostic() {
        return decision(
            C::CandidateFailure,
            Some(R::Candidate),
            candidate.class.wire_token(),
        );
    }
    if candidate.class != RunClass::ExitedZero {
        return orphan();
    }
    let Some(check) = evidence.check else {
        return orphan();
    };
    match &check.kind {
        CheckKind::Failed { diag } => decision(
            C::CandidateFailure,
            Some(R::Candidate),
            report::ComparisonReason::from(diag.as_ref()).wire_token(),
        ),
        CheckKind::Judged { outcome } => match outcome.as_ref() {
            report::JudgedCheck::Rejected { fail, .. } => decision(
                C::CandidateFailure,
                Some(R::Checker),
                match fail {
                    report::JudgeFail::WrongAnswer => "wrong_answer",
                    report::JudgeFail::PresentationError => "presentation_error",
                },
            ),
            report::JudgedCheck::Scored { raw, scale, .. } => {
                if *raw < evidence.min_score.unwrap_or(*scale) {
                    decision(
                        C::CandidateFailure,
                        Some(R::Checker),
                        "score_below_threshold",
                    )
                } else {
                    decision(C::Passed, None, "passed")
                }
            }
            _ => orphan(),
        },
        // S8. A successful comparator on an incomplete capture is expressly
        // an orphan, even when its producer supplied no run-class row.
        CheckKind::Passed if candidate.stdout_complete || find(R::Checker).is_some() => {
            decision(C::Passed, None, "passed")
        }
        _ => orphan(),
    }
}

fn judge_reason(reason: report::JudgeReason) -> &'static str {
    use report::JudgeReason as R;
    match reason {
        R::DeclaredFail => "declared_fail",
        R::ExitUnsupported => "exit_unsupported",
        R::OutcomeUnsupported => "outcome_unsupported",
        R::PointsNotEnabled => "points_not_enabled",
        R::OutsideAuthority => "outside_authority",
        R::ReportMissing => "report_missing",
        R::ReportNotRegular => "report_not_regular",
        R::ReportUnreadable => "report_unreadable",
        R::ReportTooLarge => "report_too_large",
        R::ReportUndecodable => "report_undecodable",
        R::ReportMalformed => "report_malformed",
        R::ReportSchema => "report_schema",
        R::PointsInvalid => "points_invalid",
        R::Disagreement => "disagreement",
        R::ToutMissing => "tout_missing",
        R::ToutNotRegular => "tout_not_regular",
        R::ToutUnreadable => "tout_unreadable",
        R::ToutTooLarge => "tout_too_large",
        R::Signal => "signal",
        R::ExitCode => "exit_code",
        R::WallDeadline => "wall_deadline",
        R::OutputBudget => "output_budget",
        R::DialogueOutput => "dialogue_output",
        R::PeerAllowanceExpired => "peer_allowance_expired",
        R::InvalidInput => "invalid_input",
        R::ValidationFailed => "validation_failed",
    }
}
