//! Process statuses and the single report-contract precedence table.
use crate::error::FailureClass;
use crate::report::JobVerdict;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    Failed = 1,
    Usage = 2,
    BuildFailed = 3,
    NotVerified = 4,
    CheckerFailed = 5,
    Cancelled = 6,
    Config = 7,
    Workspace = 8,
    Partial = 9,
    Infrastructure = 10,
    Delivery = 11,
    Site = 12,
    Internal = 70,
}
impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        Self::from(code as u8)
    }
}

/// Independent observations, including failures detected after report freeze.
/// Set `switch_hook` only for the hook attached to a successful switch, never
/// for the selection operation itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExitInputs {
    pub failure: Option<FailureClass>,
    pub verdict: Option<JobVerdict>,
    pub forced: bool,
    pub machine_output: bool,
    pub delivery_failed: bool,
    pub cancellation: bool,
    pub switch_hook: bool,
    pub score_full: bool,
}

/// First matching row of report §5.2. Signal termination is performed separately
/// after delivery; its fallback status is always `Cancelled`.
pub fn exit_status(input: &ExitInputs) -> ExitCode {
    use FailureClass as F;
    use JobVerdict as V;
    if input.failure == Some(F::Usage) {
        ExitCode::Usage
    } else if input.forced {
        ExitCode::Cancelled
    } else if input.failure == Some(F::Internal) || input.verdict == Some(V::InternalError) {
        ExitCode::Internal
    } else if (input.machine_output && input.delivery_failed) || input.failure == Some(F::Delivery)
    {
        ExitCode::Delivery
    } else if input.cancellation
        || input.failure == Some(F::Cancelled)
        || input.verdict == Some(V::Cancelled)
    {
        ExitCode::Cancelled
    } else if input.switch_hook {
        ExitCode::Success
    } else {
        match input.failure {
            Some(F::Config) => ExitCode::Config,
            Some(F::Workspace) => ExitCode::Workspace,
            Some(F::Partial) => ExitCode::Partial,
            Some(F::Site) => ExitCode::Site,
            Some(F::Infrastructure) => ExitCode::Infrastructure,
            _ => match input.verdict {
                Some(V::Infrastructure) => ExitCode::Infrastructure,
                Some(V::SubmitVersionMismatch) => ExitCode::Config,
                Some(V::BuildFailed | V::SubmitFailed) => ExitCode::BuildFailed,
                Some(V::CheckerFailed) => ExitCode::CheckerFailed,
                Some(
                    V::WallDeadline
                    | V::OutputBudget
                    | V::Signal
                    | V::NonzeroExit
                    | V::WrongAnswer
                    | V::PresentationError
                    | V::SessionLimit
                    | V::Idle,
                ) => ExitCode::Failed,
                Some(V::Scored) if !input.score_full => ExitCode::Failed,
                Some(V::Unchecked | V::NoTests) => ExitCode::NotVerified,
                _ => ExitCode::Success,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_precedence_row_and_outcome() {
        use FailureClass as F;
        use JobVerdict as V;
        for (failure, verdict, forced, machine_output, delivery_failed, cancellation, expected) in [
            (Some(F::Usage), None, true, true, true, true, 2),
            (Some(F::Internal), None, true, true, true, true, 6),
            (Some(F::Internal), None, false, true, true, true, 70),
            (None, Some(V::InternalError), false, true, true, true, 70),
            (None, None, false, true, true, true, 11),
            (Some(F::Cancelled), None, false, false, false, false, 6),
            (None, Some(V::Cancelled), false, false, false, false, 6),
            (None, Some(V::Passed), false, false, false, true, 6),
            (Some(F::Config), None, false, false, false, false, 7),
            (Some(F::Workspace), None, false, false, false, false, 8),
            (Some(F::Partial), None, false, false, false, false, 9),
            (Some(F::Site), None, false, false, false, false, 12),
            (
                None,
                Some(V::Infrastructure),
                false,
                false,
                false,
                false,
                10,
            ),
            (
                None,
                Some(V::SubmitVersionMismatch),
                false,
                false,
                false,
                false,
                7,
            ),
            (None, Some(V::SubmitFailed), false, false, false, false, 3),
            (None, Some(V::BuildFailed), false, false, false, false, 3),
            (None, Some(V::CheckerFailed), false, false, false, false, 5),
            (None, Some(V::WallDeadline), false, false, false, false, 1),
            (None, Some(V::OutputBudget), false, false, false, false, 1),
            (None, Some(V::Signal), false, false, false, false, 1),
            (None, Some(V::NonzeroExit), false, false, false, false, 1),
            (None, Some(V::WrongAnswer), false, false, false, false, 1),
            (None, Some(V::Unchecked), false, false, false, false, 4),
            (None, Some(V::NoTests), false, false, false, false, 4),
            (None, Some(V::Passed), false, false, false, false, 0),
            (None, Some(V::Succeeded), false, false, false, false, 0),
        ] {
            assert_eq!(
                exit_status(&ExitInputs {
                    failure,
                    verdict,
                    forced,
                    machine_output,
                    delivery_failed,
                    cancellation,
                    switch_hook: false,
                    score_full: false,
                }) as u8,
                expected
            );
        }
    }
    #[test]
    fn delivery_beats_refusals_and_all_ordinary_verdicts() {
        for failure in [
            FailureClass::Config,
            FailureClass::Workspace,
            FailureClass::Partial,
            FailureClass::Site,
            FailureClass::Cancelled,
        ] {
            assert_eq!(
                exit_status(&ExitInputs {
                    failure: Some(failure),
                    machine_output: true,
                    delivery_failed: true,
                    ..ExitInputs::default()
                }),
                ExitCode::Delivery
            );
        }
        for verdict in [
            JobVerdict::Infrastructure,
            JobVerdict::Cancelled,
            JobVerdict::BuildFailed,
            JobVerdict::CheckerFailed,
            JobVerdict::WrongAnswer,
            JobVerdict::WallDeadline,
            JobVerdict::OutputBudget,
            JobVerdict::Signal,
            JobVerdict::NonzeroExit,
            JobVerdict::Unchecked,
            JobVerdict::NoTests,
            JobVerdict::Passed,
            JobVerdict::Succeeded,
        ] {
            let input = ExitInputs {
                verdict: Some(verdict),
                machine_output: true,
                delivery_failed: true,
                ..ExitInputs::default()
            };
            assert_eq!(exit_status(&input), ExitCode::Delivery);
            assert_eq!(
                exit_status(&ExitInputs {
                    forced: true,
                    ..input
                }),
                ExitCode::Cancelled
            );
        }
    }
    #[test]
    fn failure_classes_without_a_verdict() {
        for (failure, expected) in [
            (FailureClass::Delivery, ExitCode::Delivery),
            (FailureClass::Infrastructure, ExitCode::Infrastructure),
            (FailureClass::Cancelled, ExitCode::Cancelled),
        ] {
            assert_eq!(
                exit_status(&ExitInputs {
                    failure: Some(failure),
                    ..ExitInputs::default()
                }),
                expected
            );
        }
        // A delivery failure counts only when machine output was requested.
        assert_eq!(
            exit_status(&ExitInputs {
                delivery_failed: true,
                verdict: Some(JobVerdict::Passed),
                ..ExitInputs::default()
            }),
            ExitCode::Success
        );
    }
    #[test]
    fn switch_ignores_only_ordinary_hook_failures() {
        let input = ExitInputs {
            switch_hook: true,
            verdict: Some(JobVerdict::Infrastructure),
            failure: Some(FailureClass::Config),
            ..ExitInputs::default()
        };
        assert_eq!(exit_status(&input), ExitCode::Success);
        assert_eq!(
            exit_status(&ExitInputs {
                cancellation: true,
                ..input
            }),
            ExitCode::Cancelled
        );
        assert_eq!(
            exit_status(&ExitInputs {
                verdict: Some(JobVerdict::InternalError),
                ..input
            }),
            ExitCode::Internal
        );
    }
    #[test]
    fn infrastructure_beats_wrong_answer_via_the_job_verdict() {
        let verdicts = [
            crate::report::TestVerdict::Infrastructure,
            crate::report::TestVerdict::WrongAnswer,
        ];
        let result = crate::report::job_verdict(&crate::report::JobInput {
            roles: &[],
            scoring: crate::report::Scoring::PassFail,
            kind: crate::report::JobKind::Test,
            build: None,
            primary: &[],
            tests: &verdicts,
            selected_tests: 2,
            internal_before_freeze: None,
            internal_after_freeze: None,
        });
        assert_eq!(
            exit_status(&ExitInputs {
                verdict: Some(result.verdict),
                ..ExitInputs::default()
            }),
            ExitCode::Infrastructure
        );
    }

    #[test]
    fn site_is_twelve_and_precedes_every_ordinary_verdict() {
        assert_eq!(ExitCode::Site as u8, 12);
        assert_eq!(FailureClass::Site.token(), "site");
        for verdict in [
            JobVerdict::Infrastructure,
            JobVerdict::BuildFailed,
            JobVerdict::CheckerFailed,
            JobVerdict::WrongAnswer,
            JobVerdict::Unchecked,
            JobVerdict::Passed,
        ] {
            assert_eq!(
                exit_status(&ExitInputs {
                    failure: Some(FailureClass::Site),
                    verdict: Some(verdict),
                    ..ExitInputs::default()
                }),
                ExitCode::Site
            );
        }
        assert_eq!(
            exit_status(&ExitInputs {
                failure: Some(FailureClass::Site),
                cancellation: true,
                ..ExitInputs::default()
            }),
            ExitCode::Cancelled
        );
        assert_eq!(
            exit_status(&ExitInputs {
                failure: Some(FailureClass::Site),
                verdict: Some(JobVerdict::InternalError),
                ..ExitInputs::default()
            }),
            ExitCode::Internal
        );
        // Earlier non-job refusal rows retain their codes.
        for (failure, code) in [
            (FailureClass::Config, ExitCode::Config),
            (FailureClass::Workspace, ExitCode::Workspace),
            (FailureClass::Partial, ExitCode::Partial),
        ] {
            assert_eq!(
                exit_status(&ExitInputs {
                    failure: Some(failure),
                    ..ExitInputs::default()
                }),
                code
            );
        }
    }
}
