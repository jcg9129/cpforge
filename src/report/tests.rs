use super::*;
use crate::compare::{self, Diag};
use crate::supervisor::{self, CaptureLoss, Cleanup, StopTrigger, Termination};
use std::ffi::OsString;
use std::time::Duration;

fn infra() -> RunClass {
    RunClass::Infrastructure {
        causes: InfrastructureCauses::new([InfrastructureCause::StatusUnknown]),
    }
}
fn started(class: RunClass) -> ExecutionResult {
    ExecutionResult {
        coverage: Coverage::Started,
        class: Some(class),
        invariant: None,
    }
}
fn skipped(reason: SkipReason) -> ExecutionResult {
    ExecutionResult {
        coverage: Coverage::Skipped { reason },
        class: None,
        invariant: None,
    }
}
fn not_started(reason: NotStartedReason) -> ExecutionResult {
    ExecutionResult {
        coverage: Coverage::NotStarted { reason },
        class: Some(classify_run(&RunInput::not_started(reason))),
        invariant: None,
    }
}
fn input(change: impl FnOnce(&mut RunInput)) -> RunInput {
    let mut record = RunInput::not_started(NotStartedReason::Worker);
    record.execution = ExecutionEvidence::Started(Termination::Exited { code: 0 });
    record.consumes_stdout = true;
    change(&mut record);
    record
}
fn causes(values: impl IntoIterator<Item = InfrastructureCause>) -> RunClass {
    RunClass::Infrastructure {
        causes: InfrastructureCauses::new(values),
    }
}
fn output_budget(stdout: bool, stderr: bool) -> RunClass {
    RunClass::OutputBudget {
        streams: BudgetStreams {
            stdout,
            stderr,
            output_file: false,
        },
    }
}
fn check(kind: CheckKind, diagnostic: bool) -> CheckOutcome {
    CheckOutcome { kind, diagnostic }
}
fn not_admitted(reason: CheckNotAdmitted) -> CheckOutcome {
    check(CheckKind::NotAdmitted { reason }, false)
}
fn mismatch() -> Box<Diag> {
    Box::new(Diag::ByteMismatch {
        offset: 0,
        o: Some(b'x'),
        a: Some(b'y'),
    })
}
fn invalid_answer() -> Box<Diag> {
    let compare::CheckOutcome::CheckerFailed(diag) = compare::check(
        &crate::config::Comparator::Float(crate::config::FloatComparator {
            abs: Some(1e-9),
            rel: None,
            ulps: None,
            rel_base: crate::config::RelativeBase::Answer,
            combine: crate::config::Combine::Either,
            int_exact: true,
        }),
        b"0",
        Some(b"nan"),
        true,
    ) else {
        panic!("invalid float answer fixture");
    };
    diag
}

#[test]
fn classify_run_fixtures() {
    use InfrastructureCause as C;
    use SupervisorErrorKind as E;
    let mut rows = vec![
        ("exited_zero", input(|_| {}), RunClass::ExitedZero),
        (
            "nonzero_exit",
            input(|r| r.execution = ExecutionEvidence::Started(Termination::Exited { code: 7 })),
            RunClass::NonzeroExit { code: 7 },
        ),
        (
            "signal",
            input(|r| {
                r.execution = ExecutionEvidence::Started(Termination::Signalled {
                    signal: 11,
                    core_dumped: true,
                })
            }),
            RunClass::Signal {
                signal: 11,
                core_dumped: true,
            },
        ),
        (
            "status_unknown",
            input(|r| r.execution = ExecutionEvidence::Started(Termination::Unknown)),
            causes([C::StatusUnknown]),
        ),
        (
            "cancelled_during_creation",
            RunInput::transferred(),
            RunClass::Cancelled,
        ),
        (
            "cancelled_before_start",
            RunInput::not_started(NotStartedReason::CancelledBeforeStart),
            RunClass::Cancelled,
        ),
        (
            "cancelled_stop",
            input(|r| r.stop_trigger = Some(StopTrigger::Cancelled)),
            RunClass::Cancelled,
        ),
        (
            "wall_deadline",
            input(|r| r.wall_violation = true),
            RunClass::WallDeadline,
        ),
        (
            "wall_before_budget_signal_exit",
            input(|r| {
                r.wall_violation = true;
                r.stdout.exceeded = true;
                r.execution = ExecutionEvidence::Started(Termination::Signalled {
                    signal: 9,
                    core_dumped: false,
                });
            }),
            RunClass::WallDeadline,
        ),
        (
            "cancel_before_wall",
            input(|r| {
                r.stop_trigger = Some(StopTrigger::Cancelled);
                r.wall_violation = true;
            }),
            RunClass::Cancelled,
        ),
        (
            "stdout_write_error_exited_zero",
            input(|r| r.stdout.losses.push(CaptureLoss::WriteError(5))),
            causes([C::CaptureLoss {
                stream: Stream::Stdout,
                loss: LossKind::WriteError,
            }]),
        ),
        (
            "kill_breach_independent_write_error",
            input(|r| {
                r.stop_trigger = Some(StopTrigger::OutputBudget(supervisor::Stream::Stdout));
                r.stdout.exceeded = true;
                r.budget_violations.stdout = true;
                r.stdout
                    .losses
                    .extend([CaptureLoss::BudgetDiscard, CaptureLoss::WriteError(5)]);
            }),
            causes([C::CaptureLoss {
                stream: Stream::Stdout,
                loss: LossKind::WriteError,
            }]),
        ),
        (
            "stderr_discard_test",
            input(|r| {
                r.stderr.exceeded = true;
                r.stderr.losses.push(CaptureLoss::BudgetDiscard);
            }),
            RunClass::ExitedZero,
        ),
        (
            "stdout_discard_raw_not_consumed",
            input(|r| {
                r.consumes_stdout = false;
                r.stdout.exceeded = true;
                r.stdout.losses.push(CaptureLoss::BudgetDiscard);
            }),
            RunClass::ExitedZero,
        ),
        (
            "stdout_discard_test_consumed",
            input(|r| r.stdout.exceeded = true),
            output_budget(true, false),
        ),
        (
            "stdout_budget_discard_without_offset",
            input(|r| r.stdout.losses.push(CaptureLoss::BudgetDiscard)),
            output_budget(true, false),
        ),
        (
            "kill_breach_both_streams",
            input(|r| {
                r.stop_trigger = Some(StopTrigger::OutputBudget(supervisor::Stream::Stderr));
                r.stdout.exceeded = true;
                r.stdout.kill_on_breach = true;
                r.stderr.exceeded = true;
                r.stderr.kill_on_breach = true;
            }),
            output_budget(true, true),
        ),
        (
            "kill_stderr",
            input(|r| r.stop_trigger = Some(StopTrigger::OutputBudget(supervisor::Stream::Stderr))),
            output_budget(false, true),
        ),
        (
            "kill_stdout_raw",
            input(|r| {
                r.consumes_stdout = false;
                r.stdout.exceeded = true;
                r.stdout.kill_on_breach = true;
            }),
            output_budget(true, false),
        ),
        (
            "budget_before_signal",
            input(|r| {
                r.stdout.exceeded = true;
                r.execution = ExecutionEvidence::Started(Termination::Signalled {
                    signal: 9,
                    core_dumped: false,
                });
            }),
            output_budget(true, false),
        ),
        (
            "cleanup_incomplete",
            input(|r| r.cleanup = Cleanup::Incomplete { survivors: 2 }),
            causes([C::CleanupIncomplete { survivors: 2 }]),
        ),
        (
            "cleanup_unverified_pass",
            input(|r| {
                r.cleanup = Cleanup::Unverified {
                    reason: supervisor::UnverifiedReason::ObserverFailed,
                }
            }),
            RunClass::ExitedZero,
        ),
        (
            "infra_before_cancel_wall_budget",
            input(|r| {
                r.supervisor_errors.push(E::Worker);
                r.stop_trigger = Some(StopTrigger::Cancelled);
                r.wall_violation = true;
                r.stdout.exceeded = true;
            }),
            causes([C::SupervisorError { error: E::Worker }]),
        ),
        (
            "duplicate_errors_and_os_codes",
            input(|r| {
                r.supervisor_errors.extend([E::Wait, E::Register, E::Wait]);
                r.stdout
                    .losses
                    .extend([CaptureLoss::WriteError(1), CaptureLoss::WriteError(2)]);
            }),
            causes([
                C::SupervisorError { error: E::Register },
                C::SupervisorError { error: E::Wait },
                C::CaptureLoss {
                    stream: Stream::Stdout,
                    loss: LossKind::WriteError,
                },
            ]),
        ),
        (
            "nine_distinct_causes",
            input(|r| {
                r.execution = ExecutionEvidence::Started(Termination::Unknown);
                r.cleanup = Cleanup::Incomplete { survivors: 1 };
                r.supervisor_errors.extend([
                    E::Accounting,
                    E::ReapTimeout,
                    E::Reap,
                    E::Kill,
                    E::PipeRead,
                    E::Wait,
                    E::Register,
                ]);
            }),
            causes([
                C::StatusUnknown,
                C::CleanupIncomplete { survivors: 1 },
                C::SupervisorError { error: E::Register },
                C::SupervisorError { error: E::Wait },
                C::SupervisorError { error: E::PipeRead },
                C::SupervisorError { error: E::Kill },
                C::SupervisorError { error: E::Reap },
                C::SupervisorError {
                    error: E::ReapTimeout,
                },
                C::SupervisorError {
                    error: E::Accounting,
                },
            ]),
        ),
    ];
    for reason in start_reasons() {
        if reason != NotStartedReason::CancelledBeforeStart {
            rows.push((
                reason.wire_token(),
                RunInput::not_started(reason),
                causes([C::NotStarted { reason }]),
            ));
        }
    }
    for (name, loss, kind) in [
        (
            "write_stalled",
            CaptureLoss::WriteStalled,
            LossKind::WriteStalled,
        ),
        ("pipe_read", CaptureLoss::PipeRead(5), LossKind::PipeRead),
        (
            "no_eof",
            CaptureLoss::NoEofBeforeDrainDeadline,
            LossKind::NoEofBeforeDrainDeadline,
        ),
        (
            "forward_incomplete",
            CaptureLoss::ForwardIncomplete,
            LossKind::ForwardIncomplete,
        ),
    ] {
        rows.push((
            name,
            input(|r| r.stdout.losses.push(loss)),
            causes([C::CaptureLoss {
                stream: Stream::Stdout,
                loss: kind,
            }]),
        ));
        rows.push((
            name,
            input(|r| r.stderr.losses.push(loss)),
            RunClass::ExitedZero,
        ));
    }
    for (name, record, expected) in rows {
        let actual = classify_run(&record);
        assert_eq!(actual, expected, "{name}");
        if name == "nine_distinct_causes" {
            let RunClass::Infrastructure { causes } = actual else {
                panic!("{name}");
            };
            assert_eq!(causes.as_slice().len(), 8);
            assert_eq!(causes.causes_dropped, 1);
            assert_eq!(causes.primary(), Some(&C::StatusUnknown));
            assert_eq!(
                causes.as_slice().last(),
                Some(&C::SupervisorError {
                    error: E::ReapTimeout
                })
            );
        }
        if name == "kill_breach_independent_write_error" {
            assert!(record.stdout.exceeded && record.budget_violations.stdout);
        }
    }
}

fn start_reasons() -> [NotStartedReason; 11] {
    use NotStartedReason::*;
    [
        CancelledBeforeStart,
        CreateFailed,
        Limit,
        UnsupportedLimit,
        Unsupported,
        ContainmentUnavailable,
        Setup,
        CleanupCapacity,
        Worker,
        RetentionBudget,
        SpecTooLarge,
    ]
}

#[test]
fn run_note_fixtures() {
    let rows = [
        (
            "stderr_discard_test",
            input(|r| {
                r.stderr.exceeded = true;
                r.stderr.losses.push(CaptureLoss::BudgetDiscard);
            }),
            vec![RunNote::TruncatedStderr],
        ),
        (
            "stdout_discard_raw",
            input(|r| {
                r.consumes_stdout = false;
                r.stdout.exceeded = true;
            }),
            vec![RunNote::TruncatedStdout],
        ),
        (
            "cleanup_unverified",
            input(|r| {
                r.cleanup = Cleanup::Unverified {
                    reason: supervisor::UnverifiedReason::ObserverFailed,
                }
            }),
            vec![RunNote::CleanupUnverified],
        ),
        (
            "cwd_overflow",
            input(|r| r.cwd_overflow_bytes = Some(101)),
            vec![RunNote::CwdOverflow],
        ),
        (
            "stdout_consumed_breach",
            input(|r| r.stdout.exceeded = true),
            vec![],
        ),
        (
            "stderr_kill_breach",
            input(|r| {
                r.stderr.exceeded = true;
                r.stderr.kill_on_breach = true;
            }),
            vec![],
        ),
    ];
    for (name, record, expected) in rows {
        assert_eq!(run_notes(&record), expected, "{name}");
        if matches!(
            name,
            "cleanup_unverified" | "cwd_overflow" | "stderr_discard_test" | "stdout_discard_raw"
        ) {
            assert_eq!(classify_run(&record), RunClass::ExitedZero, "{name}");
        }
    }
}

#[test]
fn infrastructure_causes_fixture() {
    use InfrastructureCause as C;
    let rows = [(
        "stable_order_and_dedup",
        vec![
            C::CaptureLoss {
                stream: Stream::Stderr,
                loss: LossKind::ForwardIncomplete,
            },
            C::CaptureLoss {
                stream: Stream::Stdout,
                loss: LossKind::PipeRead,
            },
            C::SupervisorError {
                error: SupervisorErrorKind::Wait,
            },
            C::StatusUnknown,
            C::NotStarted {
                reason: NotStartedReason::Setup,
            },
            C::CleanupIncomplete { survivors: 3 },
            C::CleanupIncomplete { survivors: 1 },
            C::CaptureLoss {
                stream: Stream::Stdout,
                loss: LossKind::WriteError,
            },
            C::StatusUnknown,
        ],
        vec![
            C::NotStarted {
                reason: NotStartedReason::Setup,
            },
            C::StatusUnknown,
            C::CleanupIncomplete { survivors: 1 },
            C::SupervisorError {
                error: SupervisorErrorKind::Wait,
            },
            C::CaptureLoss {
                stream: Stream::Stdout,
                loss: LossKind::WriteError,
            },
            C::CaptureLoss {
                stream: Stream::Stdout,
                loss: LossKind::PipeRead,
            },
            C::CaptureLoss {
                stream: Stream::Stderr,
                loss: LossKind::ForwardIncomplete,
            },
        ],
    )];
    for (name, input, expected) in rows {
        assert_eq!(
            InfrastructureCauses::new(input).as_slice(),
            expected,
            "{name}"
        );
    }
}

#[test]
fn build_result_fixtures() {
    let post = PostBuild::default();
    let missing = PostBuild {
        missing_outputs: vec![OsString::from("main")],
        ..post.clone()
    };
    let invalid = PostBuild {
        generated_program_invalid: Some(("main".into(), "not launchable".into())),
        ..post.clone()
    };
    let large = PostBuild {
        output_too_large: Some((101, 100)),
        ..post.clone()
    };
    let mut rows = vec![
        (
            "skipped_cancelled",
            skipped(SkipReason::Cancelled),
            post.clone(),
            BuildResult::Cancelled,
        ),
        (
            "infrastructure",
            started(infra()),
            missing.clone(),
            BuildResult::Infrastructure {
                causes: InfrastructureCauses::new([InfrastructureCause::StatusUnknown]),
            },
        ),
        (
            "cancelled",
            started(RunClass::Cancelled),
            post.clone(),
            BuildResult::Cancelled,
        ),
        (
            "succeeded",
            started(RunClass::ExitedZero),
            post.clone(),
            BuildResult::Succeeded,
        ),
        (
            "missing_output",
            started(RunClass::ExitedZero),
            missing.clone(),
            BuildResult::Failed {
                cause: BuildCause::MissingOutput {
                    names: vec!["main".into()],
                    names_dropped: 0,
                },
            },
        ),
        (
            "generated_program_invalid",
            started(RunClass::ExitedZero),
            invalid.clone(),
            BuildResult::Failed {
                cause: BuildCause::GeneratedProgramInvalid {
                    name: "main".into(),
                    reason: "not launchable".into(),
                },
            },
        ),
        (
            "output_too_large",
            started(RunClass::ExitedZero),
            large.clone(),
            BuildResult::Failed {
                cause: BuildCause::OutputTooLarge {
                    bytes: 101,
                    cap: 100,
                },
            },
        ),
        (
            "post_checks_in_order",
            started(RunClass::ExitedZero),
            PostBuild {
                missing_outputs: missing.missing_outputs.clone(),
                generated_program_invalid: invalid.generated_program_invalid.clone(),
                output_too_large: large.output_too_large,
                invariant: None,
            },
            BuildResult::Failed {
                cause: BuildCause::MissingOutput {
                    names: vec!["main".into()],
                    names_dropped: 0,
                },
            },
        ),
        (
            "invalid_before_cap",
            started(RunClass::ExitedZero),
            PostBuild {
                output_too_large: large.output_too_large,
                ..invalid
            },
            BuildResult::Failed {
                cause: BuildCause::GeneratedProgramInvalid {
                    name: "main".into(),
                    reason: "not launchable".into(),
                },
            },
        ),
        (
            "build_skipped_stop_policy",
            skipped(SkipReason::StopPolicy),
            post.clone(),
            BuildResult::InternalError {
                invariant: Invariant::BuildCoverage,
            },
        ),
        (
            "record_missing",
            ExecutionResult {
                coverage: Coverage::Admitted,
                class: None,
                invariant: None,
            },
            post.clone(),
            BuildResult::InternalError {
                invariant: Invariant::RecordMissing,
            },
        ),
        (
            "summary_underivable",
            ExecutionResult {
                coverage: Coverage::Started,
                class: None,
                invariant: None,
            },
            post.clone(),
            BuildResult::InternalError {
                invariant: Invariant::SummaryUnderivable,
            },
        ),
        (
            "post_build_underivable",
            started(RunClass::ExitedZero),
            PostBuild {
                invariant: Some(Invariant::BuildEvidence),
                ..post.clone()
            },
            BuildResult::InternalError {
                invariant: Invariant::BuildEvidence,
            },
        ),
        (
            "missing_outputs_bounded",
            started(RunClass::ExitedZero),
            PostBuild {
                missing_outputs: (0..18).map(|n| OsString::from(n.to_string())).collect(),
                ..post.clone()
            },
            BuildResult::Failed {
                cause: BuildCause::MissingOutput {
                    names: (0..16).map(|n| OsString::from(n.to_string())).collect(),
                    names_dropped: 2,
                },
            },
        ),
    ];
    for class in [
        RunClass::WallDeadline,
        output_budget(true, false),
        RunClass::Signal {
            signal: 9,
            core_dumped: false,
        },
        RunClass::NonzeroExit { code: 2 },
    ] {
        rows.push((
            class.wire_token(),
            started(class.clone()),
            missing.clone(),
            BuildResult::Failed {
                cause: BuildCause::Run(class),
            },
        ));
    }
    for reason in [
        SkipReason::BuildNotSucceeded,
        SkipReason::AdmissionClosed,
        SkipReason::JobNotEligible,
    ] {
        rows.push((
            reason.wire_token(),
            skipped(reason),
            post.clone(),
            BuildResult::InternalError {
                invariant: Invariant::BuildCoverage,
            },
        ));
    }
    for (name, execution, post, expected) in rows {
        assert_eq!(build_result(&execution, &post), expected, "{name}");
    }
}

#[test]
fn check_outcome_fixtures() {
    use CheckAdmission as A;
    let matched = Comparison::Outcome(compare::CheckOutcome::Passed);
    let absent = Comparison::NotRun;
    let io = Comparison::IoError { os_error: Some(5) };
    let invalid = Comparison::Outcome(compare::CheckOutcome::CheckerFailed(invalid_answer()));
    let rows = vec![
        (
            "skipped_cancelled",
            skipped(SkipReason::Cancelled),
            A::NoIntent,
            false,
            absent.clone(),
            not_admitted(CheckNotAdmitted::Cancelled),
        ),
        (
            "answer_absent_build_not_succeeded",
            skipped(SkipReason::BuildNotSucceeded),
            A::NoIntent,
            false,
            absent.clone(),
            not_admitted(CheckNotAdmitted::RunNotEligible),
        ),
        (
            "cancelled_before_start",
            not_started(NotStartedReason::CancelledBeforeStart),
            A::IntentBeforeAdmission,
            true,
            absent.clone(),
            not_admitted(CheckNotAdmitted::RunNotEligible),
        ),
        (
            "not_started_retention",
            not_started(NotStartedReason::RetentionBudget),
            A::NoIntent,
            true,
            absent.clone(),
            not_admitted(CheckNotAdmitted::RunNotEligible),
        ),
        (
            "infrastructure_before_intent",
            started(infra()),
            A::IntentBeforeAdmission,
            false,
            absent.clone(),
            not_admitted(CheckNotAdmitted::RunNotEligible),
        ),
        (
            "cancelled_class",
            started(RunClass::Cancelled),
            A::NoIntent,
            true,
            absent.clone(),
            not_admitted(CheckNotAdmitted::RunNotEligible),
        ),
        (
            "answer_absent_exited_zero",
            started(RunClass::ExitedZero),
            A::NoIntent,
            false,
            absent.clone(),
            check(CheckKind::Unchecked, false),
        ),
        (
            "answer_absent_intent_at_check_admit",
            started(RunClass::ExitedZero),
            A::IntentBeforeAdmission,
            false,
            absent.clone(),
            not_admitted(CheckNotAdmitted::Cancelled),
        ),
        (
            "intent_blocks_diagnostic",
            started(RunClass::WallDeadline),
            A::IntentBeforeAdmission,
            true,
            absent.clone(),
            not_admitted(CheckNotAdmitted::Cancelled),
        ),
        (
            "intent_after_admission_finishes",
            started(RunClass::ExitedZero),
            A::IntentAfterAdmission,
            true,
            matched.clone(),
            check(CheckKind::Passed, false),
        ),
        (
            "answer_empty_present_match",
            started(RunClass::ExitedZero),
            A::NoIntent,
            true,
            matched.clone(),
            check(CheckKind::Passed, false),
        ),
        (
            "tle_matching_output",
            started(RunClass::WallDeadline),
            A::NoIntent,
            true,
            matched,
            check(CheckKind::Passed, true),
        ),
        (
            "tle_capture_read_error",
            started(RunClass::WallDeadline),
            A::NoIntent,
            true,
            io.clone(),
            check(CheckKind::CheckError { os_error: Some(5) }, true),
        ),
        (
            "exited_zero_read_error",
            started(RunClass::ExitedZero),
            A::NoIntent,
            true,
            io,
            check(CheckKind::CheckError { os_error: Some(5) }, false),
        ),
        (
            "invalid_float_answer",
            started(RunClass::ExitedZero),
            A::NoIntent,
            true,
            invalid,
            check(
                CheckKind::CheckerFailed {
                    diag: invalid_answer(),
                },
                false,
            ),
        ),
        (
            "mismatch",
            started(RunClass::ExitedZero),
            A::NoIntent,
            true,
            Comparison::Outcome(compare::CheckOutcome::Failed(mismatch())),
            check(CheckKind::Failed { diag: mismatch() }, false),
        ),
        (
            "match_incomplete_capture",
            started(output_budget(true, false)),
            A::NoIntent,
            true,
            Comparison::Outcome(compare::CheckOutcome::Failed(Box::new(
                Diag::IncompleteOutput,
            ))),
            check(
                CheckKind::Failed {
                    diag: Box::new(Diag::IncompleteOutput),
                },
                true,
            ),
        ),
        (
            "diagnostic_panic",
            started(RunClass::WallDeadline),
            A::NoIntent,
            true,
            Comparison::InternalError {
                invariant: Invariant::CheckEvidence,
            },
            check(
                CheckKind::InternalError {
                    invariant: Invariant::CheckEvidence,
                },
                true,
            ),
        ),
        (
            "record_missing",
            ExecutionResult {
                coverage: Coverage::Admitted,
                class: None,
                invariant: None,
            },
            A::NoIntent,
            true,
            absent.clone(),
            check(
                CheckKind::InternalError {
                    invariant: Invariant::RecordMissing,
                },
                false,
            ),
        ),
        (
            "summary_underivable",
            ExecutionResult {
                coverage: Coverage::Started,
                class: None,
                invariant: None,
            },
            A::NoIntent,
            true,
            absent.clone(),
            check(
                CheckKind::InternalError {
                    invariant: Invariant::SummaryUnderivable,
                },
                false,
            ),
        ),
        (
            "missing_comparison_evidence",
            started(RunClass::ExitedZero),
            A::NoIntent,
            true,
            absent,
            check(
                CheckKind::InternalError {
                    invariant: Invariant::CheckEvidence,
                },
                false,
            ),
        ),
    ];
    for (name, execution, admission, answer, comparison, expected) in rows {
        assert_eq!(
            check_outcome(&execution, admission, answer, &comparison),
            expected,
            "{name}"
        );
    }
}

fn classes() -> Vec<RunClass> {
    vec![
        infra(),
        RunClass::Cancelled,
        RunClass::WallDeadline,
        output_budget(true, false),
        RunClass::Signal {
            signal: 11,
            core_dumped: true,
        },
        RunClass::NonzeroExit { code: 7 },
        RunClass::ExitedZero,
    ]
}
fn check_kinds() -> Vec<CheckKind> {
    vec![
        CheckKind::NotAdmitted {
            reason: CheckNotAdmitted::Cancelled,
        },
        CheckKind::NotAdmitted {
            reason: CheckNotAdmitted::RunNotEligible,
        },
        CheckKind::Unchecked,
        CheckKind::Passed,
        CheckKind::Failed { diag: mismatch() },
        CheckKind::Failed {
            diag: Box::new(Diag::IncompleteOutput),
        },
        CheckKind::CheckerFailed {
            diag: invalid_answer(),
        },
        CheckKind::CheckError { os_error: None },
        CheckKind::InternalError {
            invariant: Invariant::CheckEvidence,
        },
        CheckKind::Failed {
            diag: invalid_answer(),
        },
        CheckKind::CheckerFailed { diag: mismatch() },
    ]
}
/// The legal rows of §2.5, expanded independently of the implementation. The
/// table also supplies the whitelist for the exhaustive orphan check below.
fn verdict_rows() -> Vec<(&'static str, ExecutionResult, CheckOutcome, TestVerdict)> {
    use CheckNotAdmitted as N;
    let mut rows = vec![
        (
            "skipped_cancelled",
            skipped(SkipReason::Cancelled),
            not_admitted(N::Cancelled),
            TestVerdict::Cancelled,
        ),
        (
            "answer_absent_build_not_succeeded",
            skipped(SkipReason::BuildNotSucceeded),
            not_admitted(N::RunNotEligible),
            TestVerdict::NotRun {
                reason: SkipReason::BuildNotSucceeded,
            },
        ),
        (
            "admission_closed",
            skipped(SkipReason::AdmissionClosed),
            not_admitted(N::RunNotEligible),
            TestVerdict::NotRun {
                reason: SkipReason::AdmissionClosed,
            },
        ),
        (
            "cancelled_during_creation",
            ExecutionResult {
                coverage: Coverage::Transferred,
                class: Some(RunClass::Cancelled),
                invariant: None,
            },
            not_admitted(N::RunNotEligible),
            TestVerdict::Cancelled,
        ),
        (
            "cancelled_before_start",
            not_started(NotStartedReason::CancelledBeforeStart),
            not_admitted(N::RunNotEligible),
            TestVerdict::Cancelled,
        ),
        (
            "started_infrastructure",
            started(infra()),
            not_admitted(N::RunNotEligible),
            TestVerdict::Infrastructure,
        ),
        (
            "started_cancelled",
            started(RunClass::Cancelled),
            not_admitted(N::RunNotEligible),
            TestVerdict::Cancelled,
        ),
        (
            "exited_zero_read_error",
            started(RunClass::ExitedZero),
            check(CheckKind::CheckError { os_error: None }, false),
            TestVerdict::Infrastructure,
        ),
        (
            "invalid_float_answer",
            started(RunClass::ExitedZero),
            check(
                CheckKind::CheckerFailed {
                    diag: invalid_answer(),
                },
                false,
            ),
            TestVerdict::CheckerFailed,
        ),
        (
            "wrong_answer",
            started(RunClass::ExitedZero),
            check(CheckKind::Failed { diag: mismatch() }, false),
            TestVerdict::WrongAnswer,
        ),
        (
            "answer_absent_exited_zero",
            started(RunClass::ExitedZero),
            check(CheckKind::Unchecked, false),
            TestVerdict::Unchecked,
        ),
        (
            "passed",
            started(RunClass::ExitedZero),
            check(CheckKind::Passed, false),
            TestVerdict::Passed,
        ),
        (
            "record_missing",
            ExecutionResult {
                coverage: Coverage::Admitted,
                class: None,
                invariant: Some(Invariant::RecordMissing),
            },
            check(
                CheckKind::InternalError {
                    invariant: Invariant::RecordMissing,
                },
                false,
            ),
            TestVerdict::InternalError {
                invariant: Invariant::RecordMissing,
            },
        ),
        (
            "diagnostic_internal_error",
            started(RunClass::WallDeadline),
            check(
                CheckKind::InternalError {
                    invariant: Invariant::CheckEvidence,
                },
                true,
            ),
            TestVerdict::InternalError {
                invariant: Invariant::CheckEvidence,
            },
        ),
    ];
    for reason in start_reasons() {
        if reason != NotStartedReason::CancelledBeforeStart {
            rows.push((
                reason.wire_token(),
                not_started(reason),
                not_admitted(N::RunNotEligible),
                TestVerdict::Infrastructure,
            ));
        }
    }
    for class in classes()
        .into_iter()
        .filter(|class| !matches!(class, RunClass::Infrastructure { .. } | RunClass::Cancelled))
    {
        rows.push((
            "intent_at_check_admit",
            started(class.clone()),
            not_admitted(N::Cancelled),
            TestVerdict::Cancelled,
        ));
        let verdict = match class {
            RunClass::WallDeadline => TestVerdict::WallDeadline,
            RunClass::OutputBudget { .. } => TestVerdict::OutputBudget,
            RunClass::Signal { .. } => TestVerdict::Signal,
            RunClass::NonzeroExit { .. } => TestVerdict::NonzeroExit,
            _ => continue,
        };
        for kind in check_kinds().into_iter().take(8).skip(2) {
            rows.push((
                class.wire_token(),
                started(class.clone()),
                check(kind, true),
                verdict.clone(),
            ));
        }
    }
    rows
}
#[test]
fn test_verdict_fixtures() {
    for (name, execution, check, expected) in verdict_rows() {
        assert_eq!(test_verdict(&execution, &check), expected, "{name}");
    }
}
#[test]
fn test_verdict_is_total_and_matches_every_legal_table_combination() {
    let rows = verdict_rows();
    let mut coverage = vec![Coverage::Started, Coverage::Admitted, Coverage::Transferred];
    coverage.extend(start_reasons().map(|reason| Coverage::NotStarted { reason }));
    coverage.extend(
        [
            SkipReason::Cancelled,
            SkipReason::BuildNotSucceeded,
            SkipReason::AdmissionClosed,
            SkipReason::StopPolicy,
            SkipReason::JobNotEligible,
        ]
        .map(|reason| Coverage::Skipped { reason }),
    );
    let valid_classes = classes();
    let mut classes: Vec<_> = valid_classes.iter().cloned().map(Some).collect();
    classes.extend([
        None,
        Some(output_budget(false, false)),
        Some(causes([])),
        Some(RunClass::NonzeroExit { code: 0 }),
    ]);
    let mut checked = 0;
    for coverage in coverage {
        for class in &classes {
            let execution = ExecutionResult {
                coverage,
                class: class.clone(),
                invariant: None,
            };
            for kind in check_kinds() {
                for diagnostic in [false, true] {
                    let check = check(kind.clone(), diagnostic);
                    let actual = test_verdict(&execution, &check);
                    if let CheckKind::InternalError { invariant } = kind {
                        assert_eq!(actual, TestVerdict::InternalError { invariant });
                    } else if let Some((name, _, _, expected)) = rows.iter().find(|(_, e, c, _)| {
                        (class.is_none()
                            || class
                                .as_ref()
                                .is_some_and(|class| valid_classes.contains(class)))
                            && e.coverage == execution.coverage
                            && e.class.as_ref().map(RunClass::wire_token)
                                == class.as_ref().map(RunClass::wire_token)
                            && *c == check
                    }) {
                        assert_eq!(&actual, expected, "legal row {name}");
                    } else {
                        assert!(
                            matches!(actual, TestVerdict::InternalError { .. }),
                            "orphan: {execution:?}, {check:?} -> {actual:?}"
                        );
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 3000);
}

fn conditions() -> SkipConditions {
    SkipConditions {
        job: JobKind::Test,
        depends_on_unsucceeded_build: false,
        earlier_retention_refusal: false,
        submit_not_succeeded: false,
        input_not_validated: false,
        not_eligible: false,
        keep_going: false,
        earlier_primary_failed: false,
        intent_before_admission: false,
        after_test_hook: false,
        hooks_eligible: false,
    }
}
#[test]
fn coverage_legality_fixtures() {
    let rows = [
        ("started", started(RunClass::ExitedZero), conditions(), None),
        (
            "not_started",
            not_started(NotStartedReason::Setup),
            conditions(),
            None,
        ),
        (
            "admitted_record_missing",
            ExecutionResult {
                coverage: Coverage::Admitted,
                class: None,
                invariant: None,
            },
            conditions(),
            Some(Invariant::RecordMissing),
        ),
        (
            "summary_underivable",
            ExecutionResult {
                coverage: Coverage::Started,
                class: None,
                invariant: None,
            },
            conditions(),
            Some(Invariant::SummaryUnderivable),
        ),
        (
            "build_not_succeeded",
            skipped(SkipReason::BuildNotSucceeded),
            SkipConditions {
                depends_on_unsucceeded_build: true,
                ..conditions()
            },
            None,
        ),
        (
            "admission_closed_after_retention_refusal",
            skipped(SkipReason::AdmissionClosed),
            SkipConditions {
                earlier_retention_refusal: true,
                ..conditions()
            },
            None,
        ),
        (
            "stop_policy_cmd",
            skipped(SkipReason::StopPolicy),
            SkipConditions {
                job: JobKind::Cmd,
                earlier_primary_failed: true,
                ..conditions()
            },
            None,
        ),
        (
            "cancelled_before_admission",
            skipped(SkipReason::Cancelled),
            SkipConditions {
                intent_before_admission: true,
                ..conditions()
            },
            None,
        ),
        (
            "after_test_job_not_eligible",
            skipped(SkipReason::JobNotEligible),
            SkipConditions {
                after_test_hook: true,
                ..conditions()
            },
            None,
        ),
        (
            "admission_closed_without_retention_refusal",
            skipped(SkipReason::AdmissionClosed),
            conditions(),
            Some(Invariant::SkipCondition),
        ),
        (
            "stop_policy_test",
            skipped(SkipReason::StopPolicy),
            SkipConditions {
                earlier_primary_failed: true,
                ..conditions()
            },
            Some(Invariant::SkipCondition),
        ),
        (
            "stop_policy_keep_going",
            skipped(SkipReason::StopPolicy),
            SkipConditions {
                job: JobKind::Cmd,
                earlier_primary_failed: true,
                keep_going: true,
                ..conditions()
            },
            Some(Invariant::SkipCondition),
        ),
        (
            "build_skip_without_dependency",
            skipped(SkipReason::BuildNotSucceeded),
            conditions(),
            Some(Invariant::SkipCondition),
        ),
        (
            "cancelled_without_intent",
            skipped(SkipReason::Cancelled),
            conditions(),
            Some(Invariant::SkipCondition),
        ),
        (
            "job_not_eligible_on_primary",
            skipped(SkipReason::JobNotEligible),
            conditions(),
            Some(Invariant::SkipCondition),
        ),
        (
            "skipped_with_record_class",
            ExecutionResult {
                coverage: Coverage::Skipped {
                    reason: SkipReason::Cancelled,
                },
                class: Some(RunClass::ExitedZero),
                invariant: None,
            },
            conditions(),
            Some(Invariant::CoverageClass),
        ),
    ];
    for (name, execution, facts, expected) in rows {
        let actual = validate_execution(execution.clone(), &facts);
        assert_eq!(actual.invariant, expected, "{name}");
        assert_eq!(actual.coverage, execution.coverage, "{name}");
        assert_eq!(actual.class, execution.class, "{name}");
    }
}
#[test]
fn orphan_fixtures_propagate_to_internal_job_result() {
    let rows = [
        (
            "admission_closed_without_retention_refusal",
            validate_execution(skipped(SkipReason::AdmissionClosed), &conditions()),
            not_admitted(CheckNotAdmitted::RunNotEligible),
            Invariant::SkipCondition,
        ),
        (
            "stop_policy_test",
            skipped(SkipReason::StopPolicy),
            not_admitted(CheckNotAdmitted::RunNotEligible),
            Invariant::TestCoverage,
        ),
        (
            "exited_zero_incomplete_output",
            started(RunClass::ExitedZero),
            check(
                CheckKind::Failed {
                    diag: Box::new(Diag::IncompleteOutput),
                },
                false,
            ),
            Invariant::IncompleteZeroExit,
        ),
        (
            "diagnostic_flag",
            started(RunClass::WallDeadline),
            check(CheckKind::Passed, false),
            Invariant::DiagnosticFlag,
        ),
        (
            "illegal_check_admission",
            started(RunClass::ExitedZero),
            not_admitted(CheckNotAdmitted::RunNotEligible),
            Invariant::TestCheck,
        ),
    ];
    for (name, execution, check, invariant) in rows {
        let verdict = test_verdict(&execution, &check);
        assert_eq!(verdict, TestVerdict::InternalError { invariant }, "{name}");
        let result = job_verdict(&JobInput {
            roles: &[],
            scoring: crate::report::Scoring::PassFail,
            kind: JobKind::Test,
            build: None,
            primary: &[execution],
            tests: &[verdict],
            selected_tests: 1,
            internal_before_freeze: None,
            internal_after_freeze: None,
        });
        assert_eq!(result.primary_verdict, JobVerdict::InternalError, "{name}");
        assert_eq!(result.verdict, JobVerdict::InternalError, "{name}");
        assert!(result.internal.is_some(), "{name}");
    }
    let build = build_result(&skipped(SkipReason::StopPolicy), &PostBuild::default());
    let result = job_verdict(&JobInput {
        roles: &[],
        scoring: crate::report::Scoring::PassFail,
        kind: JobKind::Build,
        build: Some(&build),
        primary: &[],
        tests: &[],
        selected_tests: 0,
        internal_before_freeze: None,
        internal_after_freeze: None,
    });
    assert_eq!(result.verdict, JobVerdict::InternalError);
    assert_eq!(
        result.internal,
        Some(InternalFailure {
            invariant: Invariant::BuildCoverage,
            locus: InternalLocus::Build,
        })
    );
}

#[test]
fn command_and_hook_outcome_fixtures() {
    let mut rows = vec![
        (
            "succeeded",
            started(RunClass::ExitedZero),
            Ok(false),
            HookOutcome::Succeeded,
        ),
        (
            "cancelled",
            started(RunClass::Cancelled),
            Ok(true),
            HookOutcome::Cancelled,
        ),
        (
            "stop_policy_skipped",
            skipped(SkipReason::StopPolicy),
            Ok(false),
            HookOutcome::Skipped {
                reason: SkipReason::StopPolicy,
            },
        ),
        (
            "cancelled_skipped",
            skipped(SkipReason::Cancelled),
            Ok(false),
            HookOutcome::Skipped {
                reason: SkipReason::Cancelled,
            },
        ),
        (
            "job_not_eligible",
            skipped(SkipReason::JobNotEligible),
            Ok(false),
            HookOutcome::Skipped {
                reason: SkipReason::JobNotEligible,
            },
        ),
        (
            "record_missing",
            ExecutionResult {
                coverage: Coverage::Admitted,
                class: None,
                invariant: None,
            },
            Err(Invariant::RecordMissing),
            HookOutcome::InternalError {
                invariant: Invariant::RecordMissing,
            },
        ),
        (
            "summary_underivable",
            ExecutionResult {
                coverage: Coverage::Started,
                class: Some(RunClass::ExitedZero),
                invariant: Some(Invariant::SummaryUnderivable),
            },
            Err(Invariant::SummaryUnderivable),
            HookOutcome::InternalError {
                invariant: Invariant::SummaryUnderivable,
            },
        ),
    ];
    for class in classes()
        .into_iter()
        .filter(|class| !matches!(class, RunClass::ExitedZero | RunClass::Cancelled))
    {
        rows.push((
            class.wire_token(),
            started(class.clone()),
            Ok(true),
            HookOutcome::Failed { class },
        ));
    }
    let refusal = not_started(NotStartedReason::RetentionBudget);
    rows.push((
        "not_started_is_command_failure",
        refusal.clone(),
        Ok(true),
        HookOutcome::Failed {
            class: refusal.class.unwrap(),
        },
    ));
    for (name, execution, failed, hook) in rows {
        assert_eq!(command_failed(&execution), failed, "{name}");
        assert_eq!(hook_outcome(&execution), hook, "{name}");
    }
}

#[test]
fn job_verdict_fixtures() {
    struct Row {
        name: &'static str,
        kind: JobKind,
        build: Option<BuildResult>,
        primary: Vec<ExecutionResult>,
        tests: Vec<TestVerdict>,
        selected: u32,
        before: Option<InternalFailure>,
        after: Option<InternalFailure>,
        expected: JobVerdict,
        final_expected: JobVerdict,
    }
    fn row(name: &'static str, tests: Vec<TestVerdict>, expected: JobVerdict) -> Row {
        Row {
            name,
            kind: JobKind::Test,
            build: None,
            primary: vec![],
            selected: 1,
            tests,
            before: None,
            after: None,
            expected,
            final_expected: expected,
        }
    }
    let failure = InternalFailure {
        invariant: Invariant::CheckEvidence,
        locus: InternalLocus::Test {
            test: "sample".into(),
        },
    };
    let failed_build = BuildResult::Failed {
        cause: BuildCause::Run(RunClass::NonzeroExit { code: 1 }),
    };
    let mut rows = vec![
        row("passed", vec![TestVerdict::Passed], JobVerdict::Passed),
        row(
            "infrastructure_before_cancelled",
            vec![TestVerdict::Cancelled, TestVerdict::Infrastructure],
            JobVerdict::Infrastructure,
        ),
        Row {
            build: Some(failed_build.clone()),
            ..row(
                "cancelled_before_build_failure",
                vec![TestVerdict::Cancelled],
                JobVerdict::Cancelled,
            )
        },
        Row {
            build: Some(failed_build.clone()),
            ..row(
                "build_failed",
                vec![TestVerdict::NotRun {
                    reason: SkipReason::BuildNotSucceeded,
                }],
                JobVerdict::BuildFailed,
            )
        },
        Row {
            before: Some(failure.clone()),
            ..row(
                "internal_before_everything",
                vec![TestVerdict::Infrastructure],
                JobVerdict::InternalError,
            )
        },
        Row {
            after: Some(failure.clone()),
            final_expected: JobVerdict::InternalError,
            ..row(
                "internal_after_primary_freeze",
                vec![TestVerdict::Passed],
                JobVerdict::Passed,
            )
        },
        Row {
            selected: 0,
            build: Some(BuildResult::Succeeded),
            ..row("no_tests_still_built", vec![], JobVerdict::NoTests)
        },
        Row {
            selected: 0,
            build: Some(failed_build.clone()),
            ..row(
                "zero_selected_build_failed",
                vec![],
                JobVerdict::BuildFailed,
            )
        },
        Row {
            build: Some(BuildResult::Infrastructure {
                causes: InfrastructureCauses::new([InfrastructureCause::StatusUnknown]),
            }),
            ..row(
                "build_infrastructure",
                vec![TestVerdict::Cancelled],
                JobVerdict::Infrastructure,
            )
        },
        Row {
            build: Some(BuildResult::Cancelled),
            ..row(
                "build_cancelled",
                vec![TestVerdict::NotRun {
                    reason: SkipReason::BuildNotSucceeded,
                }],
                JobVerdict::Cancelled,
            )
        },
        Row {
            primary: vec![skipped(SkipReason::Cancelled)],
            ..row(
                "primary_coverage_cancelled",
                vec![TestVerdict::Passed],
                JobVerdict::Cancelled,
            )
        },
        Row {
            primary: vec![not_started(NotStartedReason::CancelledBeforeStart)],
            ..row(
                "primary_not_started_cancelled",
                vec![TestVerdict::Passed],
                JobVerdict::Cancelled,
            )
        },
        row(
            "derived_internal_test",
            vec![TestVerdict::InternalError {
                invariant: Invariant::CheckEvidence,
            }],
            JobVerdict::InternalError,
        ),
        Row {
            build: Some(BuildResult::InternalError {
                invariant: Invariant::BuildCoverage,
            }),
            ..row(
                "derived_internal_build",
                vec![TestVerdict::Passed],
                JobVerdict::InternalError,
            )
        },
        Row {
            primary: vec![ExecutionResult {
                coverage: Coverage::Admitted,
                class: None,
                invariant: None,
            }],
            ..row(
                "derived_internal_execution",
                vec![TestVerdict::Passed],
                JobVerdict::InternalError,
            )
        },
        Row {
            primary: vec![started(RunClass::ExitedZero)],
            ..row(
                "intent_after_all_runs_and_checks_admitted",
                vec![TestVerdict::Passed],
                JobVerdict::Passed,
            )
        },
        Row {
            build: Some(BuildResult::Succeeded),
            ..row(
                "ordinary_after_test_hook_warning_excluded",
                vec![TestVerdict::Passed],
                JobVerdict::Passed,
            )
        },
    ];
    // Every priority row is checked against every lower priority row, regardless
    // of input order. This catches accidental last-result aggregation.
    let priorities = [
        (TestVerdict::Infrastructure, JobVerdict::Infrastructure),
        (TestVerdict::Cancelled, JobVerdict::Cancelled),
        (TestVerdict::WallDeadline, JobVerdict::WallDeadline),
        (TestVerdict::OutputBudget, JobVerdict::OutputBudget),
        (TestVerdict::Signal, JobVerdict::Signal),
        (TestVerdict::NonzeroExit, JobVerdict::NonzeroExit),
        (TestVerdict::CheckerFailed, JobVerdict::CheckerFailed),
        (TestVerdict::WrongAnswer, JobVerdict::WrongAnswer),
        (TestVerdict::Unchecked, JobVerdict::Unchecked),
        (TestVerdict::Passed, JobVerdict::Passed),
    ];
    for (index, (higher, expected)) in priorities.iter().enumerate() {
        rows.push(row(higher.wire_token(), vec![higher.clone()], *expected));
        for (lower, _) in &priorities[index + 1..] {
            rows.push(row(
                higher.wire_token(),
                vec![lower.clone(), higher.clone()],
                *expected,
            ));
            rows.push(row(
                higher.wire_token(),
                vec![higher.clone(), lower.clone()],
                *expected,
            ));
        }
    }
    for (test, expected) in &priorities[2..] {
        rows.push(Row {
            build: Some(failed_build.clone()),
            ..row(
                test.wire_token(),
                vec![test.clone()],
                JobVerdict::BuildFailed,
            )
        });
        assert_ne!(*expected, JobVerdict::BuildFailed);
    }
    for kind in [JobKind::Run, JobKind::Cmd, JobKind::Hook] {
        for class in classes() {
            let expected = match class {
                RunClass::Infrastructure { .. } => JobVerdict::Infrastructure,
                RunClass::Cancelled => JobVerdict::Cancelled,
                RunClass::WallDeadline => JobVerdict::WallDeadline,
                RunClass::OutputBudget { .. } => JobVerdict::OutputBudget,
                RunClass::Signal { .. } => JobVerdict::Signal,
                RunClass::NonzeroExit { .. } => JobVerdict::NonzeroExit,
                RunClass::ExitedZero => JobVerdict::Succeeded,
            };
            rows.push(Row {
                kind,
                selected: 0,
                primary: vec![started(class)],
                ..row(kind.wire_token(), vec![], expected)
            });
        }
    }
    rows.push(Row {
        kind: JobKind::Build,
        selected: 0,
        build: Some(BuildResult::Succeeded),
        ..row("build_job_succeeded", vec![], JobVerdict::Succeeded)
    });
    rows.push(Row {
        kind: JobKind::Build,
        selected: 0,
        build: Some(failed_build),
        ..row("build_job_failed", vec![], JobVerdict::BuildFailed)
    });
    rows.push(Row {
        kind: JobKind::Build,
        selected: 0,
        ..row("build_free_job", vec![], JobVerdict::Succeeded)
    });
    rows.push(Row {
        kind: JobKind::Build,
        selected: 0,
        primary: vec![started(RunClass::ExitedZero)],
        ..row(
            "build_job_missing_result",
            vec![],
            JobVerdict::InternalError,
        )
    });
    let class_priorities = classes();
    for (index, higher) in class_priorities.iter().enumerate() {
        let expected = match higher {
            RunClass::Infrastructure { .. } => JobVerdict::Infrastructure,
            RunClass::Cancelled => JobVerdict::Cancelled,
            RunClass::WallDeadline => JobVerdict::WallDeadline,
            RunClass::OutputBudget { .. } => JobVerdict::OutputBudget,
            RunClass::Signal { .. } => JobVerdict::Signal,
            RunClass::NonzeroExit { .. } => JobVerdict::NonzeroExit,
            RunClass::ExitedZero => JobVerdict::Succeeded,
        };
        for lower in &class_priorities[index + 1..] {
            for kind in [JobKind::Run, JobKind::Cmd, JobKind::Hook] {
                rows.push(Row {
                    kind,
                    selected: 0,
                    primary: vec![started(lower.clone()), started(higher.clone())],
                    ..row(higher.wire_token(), vec![], expected)
                });
            }
        }
    }
    for row in rows {
        let actual = job_verdict(&JobInput {
            roles: &[],
            scoring: crate::report::Scoring::PassFail,
            kind: row.kind,
            build: row.build.as_ref(),
            primary: &row.primary,
            tests: &row.tests,
            selected_tests: row.selected,
            internal_before_freeze: row.before.as_ref(),
            internal_after_freeze: row.after.as_ref(),
        });
        assert_eq!(actual.primary_verdict, row.expected, "{}", row.name);
        assert_eq!(actual.verdict, row.final_expected, "{}", row.name);
        assert_eq!(
            actual.internal.is_some(),
            actual.verdict == JobVerdict::InternalError,
            "{}",
            row.name
        );
        if let Some(first) = row.before.or(row.after) {
            assert_eq!(actual.internal, Some(first), "{}", row.name);
        }
    }
    let first = InternalFailure {
        invariant: Invariant::SummaryUnderivable,
        locus: InternalLocus::Execution { index: 2 },
    };
    let actual = job_verdict(&JobInput {
        roles: &[],
        scoring: crate::report::Scoring::PassFail,
        kind: JobKind::Test,
        build: None,
        primary: &[],
        tests: &[TestVerdict::Passed],
        selected_tests: 1,
        internal_before_freeze: Some(&first),
        internal_after_freeze: Some(&failure),
    });
    assert_eq!(actual.internal, Some(first));
}

#[test]
fn coverage_counts_fixtures() {
    struct Row {
        name: &'static str,
        primary: Vec<ExecutionResult>,
        hooks: Vec<ExecutionResult>,
        tests: Vec<TestVerdict>,
        primary_cov: Cov,
        hook_cov: Cov,
        by_class: ByClass,
        by_verdict: ByVerdict,
        partial: bool,
    }
    let underivable = |class| ExecutionResult {
        coverage: Coverage::Started,
        class,
        invariant: Some(Invariant::SummaryUnderivable),
    };
    let rows = vec![
        Row {
            name: "zero_planned",
            primary: vec![],
            hooks: vec![],
            tests: vec![],
            primary_cov: Cov::default(),
            hook_cov: Cov::default(),
            by_class: ByClass::default(),
            by_verdict: ByVerdict::default(),
            partial: false,
        },
        Row {
            name: "cancellation_after_all_admitted_and_checked",
            primary: vec![started(RunClass::ExitedZero)],
            hooks: vec![skipped(SkipReason::JobNotEligible)],
            tests: vec![TestVerdict::Passed],
            primary_cov: Cov {
                planned: 1,
                started: 1,
                ..Cov::default()
            },
            hook_cov: Cov {
                planned: 1,
                skipped: 1,
                ..Cov::default()
            },
            by_class: ByClass {
                exited_zero: 1,
                ..ByClass::default()
            },
            by_verdict: ByVerdict {
                passed: 1,
                ..ByVerdict::default()
            },
            partial: false,
        },
        Row {
            name: "retention_refusal_then_admission_closed",
            primary: vec![
                not_started(NotStartedReason::RetentionBudget),
                skipped(SkipReason::AdmissionClosed),
            ],
            hooks: vec![],
            tests: vec![
                TestVerdict::Infrastructure,
                TestVerdict::NotRun {
                    reason: SkipReason::AdmissionClosed,
                },
            ],
            primary_cov: Cov {
                planned: 2,
                not_started: 1,
                skipped: 1,
                ..Cov::default()
            },
            hook_cov: Cov::default(),
            by_class: ByClass {
                infrastructure: 1,
                skipped: 1,
                ..ByClass::default()
            },
            by_verdict: ByVerdict {
                infrastructure: 1,
                not_run: 1,
                ..ByVerdict::default()
            },
            partial: true,
        },
        Row {
            name: "run_dir_create_existing_setup",
            primary: vec![not_started(NotStartedReason::Setup)],
            hooks: vec![],
            tests: vec![TestVerdict::Infrastructure],
            primary_cov: Cov {
                planned: 1,
                not_started: 1,
                ..Cov::default()
            },
            hook_cov: Cov::default(),
            by_class: ByClass {
                infrastructure: 1,
                ..ByClass::default()
            },
            by_verdict: ByVerdict {
                infrastructure: 1,
                ..ByVerdict::default()
            },
            partial: true,
        },
        Row {
            name: "record_missing_admitted",
            primary: vec![ExecutionResult {
                coverage: Coverage::Admitted,
                class: None,
                invariant: Some(Invariant::RecordMissing),
            }],
            hooks: vec![],
            tests: vec![TestVerdict::InternalError {
                invariant: Invariant::RecordMissing,
            }],
            primary_cov: Cov {
                planned: 1,
                admitted: 1,
                invalid: 1,
                ..Cov::default()
            },
            hook_cov: Cov::default(),
            by_class: ByClass {
                unknown: 1,
                ..ByClass::default()
            },
            by_verdict: ByVerdict {
                internal_error: 1,
                ..ByVerdict::default()
            },
            partial: true,
        },
        Row {
            name: "summary_underivable_before_class",
            primary: vec![underivable(None)],
            hooks: vec![],
            tests: vec![TestVerdict::InternalError {
                invariant: Invariant::SummaryUnderivable,
            }],
            primary_cov: Cov {
                planned: 1,
                started: 1,
                invalid: 1,
                ..Cov::default()
            },
            hook_cov: Cov::default(),
            by_class: ByClass {
                unknown: 1,
                ..ByClass::default()
            },
            by_verdict: ByVerdict {
                internal_error: 1,
                ..ByVerdict::default()
            },
            partial: false,
        },
        Row {
            name: "summary_underivable_after_class",
            primary: vec![underivable(Some(RunClass::ExitedZero))],
            hooks: vec![],
            tests: vec![],
            primary_cov: Cov {
                planned: 1,
                started: 1,
                invalid: 1,
                ..Cov::default()
            },
            hook_cov: Cov::default(),
            by_class: ByClass {
                exited_zero: 1,
                ..ByClass::default()
            },
            by_verdict: ByVerdict::default(),
            partial: false,
        },
        Row {
            name: "orphan_retains_observed_class",
            primary: vec![ExecutionResult {
                coverage: Coverage::NotStarted {
                    reason: NotStartedReason::Worker,
                },
                class: Some(RunClass::ExitedZero),
                invariant: Some(Invariant::CoverageClass),
            }],
            hooks: vec![],
            tests: vec![],
            primary_cov: Cov {
                planned: 1,
                not_started: 1,
                invalid: 1,
                ..Cov::default()
            },
            hook_cov: Cov::default(),
            by_class: ByClass {
                exited_zero: 1,
                ..ByClass::default()
            },
            by_verdict: ByVerdict::default(),
            partial: true,
        },
        Row {
            name: "hook_missing_record_does_not_make_test_partial",
            primary: vec![started(RunClass::ExitedZero)],
            hooks: vec![ExecutionResult {
                coverage: Coverage::Admitted,
                class: None,
                invariant: None,
            }],
            tests: vec![TestVerdict::Passed],
            primary_cov: Cov {
                planned: 1,
                started: 1,
                ..Cov::default()
            },
            hook_cov: Cov {
                planned: 1,
                admitted: 1,
                invalid: 1,
                ..Cov::default()
            },
            by_class: ByClass {
                exited_zero: 1,
                ..ByClass::default()
            },
            by_verdict: ByVerdict {
                passed: 1,
                ..ByVerdict::default()
            },
            partial: false,
        },
    ];
    for row in rows {
        let actual = coverage_counts(&row.primary, &row.hooks, &row.tests);
        assert_eq!(actual.primary, row.primary_cov, "{}", row.name);
        assert_eq!(actual.hooks, row.hook_cov, "{}", row.name);
        assert_eq!(actual.by_class, row.by_class, "{}", row.name);
        assert_eq!(actual.by_verdict, row.by_verdict, "{}", row.name);
        assert_eq!(actual.partial, row.partial, "{}", row.name);
        assert_eq!(actual.tests as usize, row.tests.len());
        for population in [actual.primary, actual.hooks] {
            assert_eq!(
                population.planned,
                population.started
                    + population.not_started
                    + population.skipped
                    + population.admitted,
                "{}",
                row.name
            );
        }
    }
    let primary: Vec<_> = classes().into_iter().map(started).collect();
    let tests: Vec<_> = [
        TestVerdict::Passed,
        TestVerdict::WrongAnswer,
        TestVerdict::Unchecked,
        TestVerdict::CheckerFailed,
        TestVerdict::WallDeadline,
        TestVerdict::OutputBudget,
        TestVerdict::Signal,
        TestVerdict::NonzeroExit,
        TestVerdict::Cancelled,
        TestVerdict::Infrastructure,
        TestVerdict::NotRun {
            reason: SkipReason::BuildNotSucceeded,
        },
        TestVerdict::InternalError {
            invariant: Invariant::CheckEvidence,
        },
    ]
    .into();
    let actual = coverage_counts(&primary, &[], &tests);
    assert_eq!(
        actual.by_class,
        ByClass {
            exited_zero: 1,
            nonzero_exit: 1,
            signal: 1,
            wall_deadline: 1,
            output_budget: 1,
            cancelled: 1,
            infrastructure: 1,
            skipped: 0,
            unknown: 0
        }
    );
    assert_eq!(
        actual.by_verdict,
        ByVerdict {
            passed: 1,
            wrong_answer: 1,
            unchecked: 1,
            checker_failed: 1,
            wall_deadline: 1,
            output_budget: 1,
            signal: 1,
            nonzero_exit: 1,
            cancelled: 1,
            infrastructure: 1,
            not_run: 1,
            internal_error: 1,
            ..ByVerdict::default()
        }
    );
}

#[test]
fn label_fixtures() {
    let verdicts = [
        ("passed", TestVerdict::Passed, "OK"),
        ("wrong_answer", TestVerdict::WrongAnswer, "WA"),
        ("unchecked", TestVerdict::Unchecked, "UNCHECKED"),
        (
            "checker_failed",
            TestVerdict::CheckerFailed,
            "CHECKER-FAILED",
        ),
        ("wall_deadline", TestVerdict::WallDeadline, "TLE"),
        ("output_budget", TestVerdict::OutputBudget, "OLE"),
        ("signal", TestVerdict::Signal, "RE"),
        ("nonzero_exit", TestVerdict::NonzeroExit, "RE"),
        ("cancelled", TestVerdict::Cancelled, "CANCELLED"),
        ("infrastructure", TestVerdict::Infrastructure, "INFRA"),
        (
            "internal_error",
            TestVerdict::InternalError {
                invariant: Invariant::CheckEvidence,
            },
            "INTERNAL",
        ),
        (
            "not_run",
            TestVerdict::NotRun {
                reason: SkipReason::BuildNotSucceeded,
            },
            "NOT RUN",
        ),
    ];
    for (name, verdict, expected) in verdicts {
        let actual = label(
            LabelSubject::Test {
                verdict: &verdict,
                class: None,
                check: None,
            },
            &DisplayContext::default(),
        );
        assert_eq!(
            actual.token.map(LabelToken::wire_token),
            Some(expected),
            "{name}"
        );
    }
    let builds = [
        ("build_succeeded", BuildResult::Succeeded, "OK"),
        ("build_cancelled", BuildResult::Cancelled, "CANCELLED"),
        (
            "build_infrastructure",
            BuildResult::Infrastructure {
                causes: InfrastructureCauses::new([InfrastructureCause::StatusUnknown]),
            },
            "INFRA",
        ),
        (
            "build_failed",
            BuildResult::Failed {
                cause: BuildCause::MissingOutput {
                    names: vec!["main".into()],
                    names_dropped: 0,
                },
            },
            "CE",
        ),
        (
            "build_internal",
            BuildResult::InternalError {
                invariant: Invariant::BuildCoverage,
            },
            "INTERNAL",
        ),
    ];
    for (name, result, expected) in builds {
        let actual = label(LabelSubject::Build(&result), &DisplayContext::default());
        assert_eq!(
            actual.token.map(LabelToken::wire_token),
            Some(expected),
            "{name}"
        );
    }
    let jobs = [
        (JobVerdict::InternalError, Some("INTERNAL")),
        (JobVerdict::Infrastructure, Some("INFRA")),
        (JobVerdict::Cancelled, Some("CANCELLED")),
        (JobVerdict::BuildFailed, Some("CE")),
        (JobVerdict::WallDeadline, Some("TLE")),
        (JobVerdict::OutputBudget, Some("OLE")),
        (JobVerdict::Signal, Some("RE")),
        (JobVerdict::NonzeroExit, Some("RE")),
        (JobVerdict::CheckerFailed, Some("CHECKER-FAILED")),
        (JobVerdict::WrongAnswer, Some("WA")),
        (JobVerdict::Unchecked, Some("UNCHECKED")),
        (JobVerdict::NoTests, None),
        (JobVerdict::Passed, Some("OK")),
        (JobVerdict::Succeeded, Some("OK")),
    ];
    for (verdict, expected) in jobs {
        assert_eq!(
            label(LabelSubject::Job(verdict), &DisplayContext::default())
                .token
                .map(LabelToken::wire_token),
            expected,
            "{}",
            verdict.wire_token()
        );
    }
    for class in classes() {
        let expected = match class {
            RunClass::ExitedZero => "OK",
            RunClass::NonzeroExit { .. } | RunClass::Signal { .. } => "RE",
            RunClass::WallDeadline => "TLE",
            RunClass::OutputBudget { .. } => "OLE",
            RunClass::Cancelled => "CANCELLED",
            RunClass::Infrastructure { .. } => "INFRA",
        };
        let execution = started(class);
        assert_eq!(
            label(LabelSubject::Run(&execution), &DisplayContext::default())
                .token
                .map(LabelToken::wire_token),
            Some(expected)
        );
        assert_eq!(
            label(
                LabelSubject::Hook(&hook_outcome(&execution)),
                &DisplayContext::default()
            )
            .token
            .map(LabelToken::wire_token),
            Some(expected)
        );
    }
    for (reason, token, qualifiers) in [
        (SkipReason::Cancelled, LabelToken::Cancelled, vec![]),
        (
            SkipReason::StopPolicy,
            LabelToken::NotRun,
            vec![Qualifier::NotRun(SkipReason::StopPolicy)],
        ),
    ] {
        let expected = DisplayLabel {
            token: Some(token),
            qualifiers,
        };
        assert_eq!(
            label(
                LabelSubject::Run(&skipped(reason)),
                &DisplayContext::default()
            ),
            expected,
            "{}",
            reason.wire_token()
        );
        assert_eq!(
            label(
                LabelSubject::Hook(&HookOutcome::Skipped { reason }),
                &DisplayContext::default()
            ),
            expected,
            "{}",
            reason.wire_token()
        );
    }
}
#[test]
fn label_qualifier_fixtures() {
    struct Row {
        name: &'static str,
        verdict: TestVerdict,
        class: RunClass,
        check: Option<CheckOutcome>,
        context: DisplayContext,
        expected: DisplayLabel,
    }
    let context = DisplayContext::default();
    let rows = vec![
        Row {
            name: "passed_cleanup_unverified",
            verdict: TestVerdict::Passed,
            class: RunClass::ExitedZero,
            check: None,
            context: DisplayContext {
                cleanup_unverified: true,
                ..context.clone()
            },
            expected: DisplayLabel {
                token: Some(LabelToken::Ok),
                qualifiers: vec![Qualifier::CleanupNotVerified],
            },
        },
        Row {
            name: "passed_comparator",
            verdict: TestVerdict::Passed,
            class: RunClass::ExitedZero,
            check: None,
            context: DisplayContext {
                comparator: Some("tokens"),
                ..context.clone()
            },
            expected: DisplayLabel {
                token: Some(LabelToken::Ok),
                qualifiers: vec![Qualifier::Comparator("tokens")],
            },
        },
        Row {
            name: "wrong_answer_first_difference",
            verdict: TestVerdict::WrongAnswer,
            class: RunClass::ExitedZero,
            check: Some(check(CheckKind::Failed { diag: mismatch() }, false)),
            context: context.clone(),
            expected: DisplayLabel {
                token: Some(LabelToken::WrongAnswer),
                qualifiers: vec![Qualifier::FirstDifference(mismatch())],
            },
        },
        Row {
            name: "unchecked_no_answer",
            verdict: TestVerdict::Unchecked,
            class: RunClass::ExitedZero,
            check: None,
            context: context.clone(),
            expected: DisplayLabel {
                token: Some(LabelToken::Unchecked),
                qualifiers: vec![Qualifier::NoAnswer],
            },
        },
        Row {
            name: "checker_failed_invalid_answer",
            verdict: TestVerdict::CheckerFailed,
            class: RunClass::ExitedZero,
            check: None,
            context: context.clone(),
            expected: DisplayLabel {
                token: Some(LabelToken::CheckerFailed),
                qualifiers: vec![Qualifier::InvalidAnswer],
            },
        },
        Row {
            name: "wall_deadline_and_measured_wall",
            verdict: TestVerdict::WallDeadline,
            class: RunClass::WallDeadline,
            check: None,
            context: DisplayContext {
                wall_deadline: Some(Duration::from_secs(1)),
                measured_wall: Some(Duration::from_millis(1050)),
                ..context.clone()
            },
            expected: DisplayLabel {
                token: Some(LabelToken::TimeLimit),
                qualifiers: vec![Qualifier::WallDeadline {
                    deadline: Some(Duration::from_secs(1)),
                    measured_wall: Some(Duration::from_millis(1050)),
                }],
            },
        },
        Row {
            name: "output_streams_and_budgets",
            verdict: TestVerdict::OutputBudget,
            class: output_budget(true, true),
            check: None,
            context: DisplayContext {
                budgets: vec![(Stream::Stdout, 100), (Stream::Stderr, 200)],
                ..context.clone()
            },
            expected: DisplayLabel {
                token: Some(LabelToken::OutputLimit),
                qualifiers: vec![Qualifier::OutputBudget {
                    streams: BudgetStreams {
                        stdout: true,
                        stderr: true,
                        output_file: false,
                    },
                    budgets: vec![(Stream::Stdout, 100), (Stream::Stderr, 200)],
                }],
            },
        },
        Row {
            name: "signal_under_address_space_limit",
            verdict: TestVerdict::Signal,
            class: RunClass::Signal {
                signal: 11,
                core_dumped: true,
            },
            check: None,
            context: DisplayContext {
                signal_name: Some("SIGSEGV".into()),
                installed_limits: vec![supervisor::LocalLimit::AddressSpace { bytes: 1024 }],
                ..context.clone()
            },
            expected: DisplayLabel {
                token: Some(LabelToken::RuntimeError),
                qualifiers: vec![
                    Qualifier::Signal {
                        signal: 11,
                        name: Some("SIGSEGV".into()),
                        core_dumped: true,
                    },
                    Qualifier::InstalledLimit(supervisor::LocalLimit::AddressSpace { bytes: 1024 }),
                ],
            },
        },
        Row {
            name: "nonzero_windows_ntstatus",
            verdict: TestVerdict::NonzeroExit,
            class: RunClass::NonzeroExit { code: 0xc0000005 },
            check: None,
            context: DisplayContext {
                ntstatus: Some("STATUS_ACCESS_VIOLATION".into()),
                ..context.clone()
            },
            expected: DisplayLabel {
                token: Some(LabelToken::RuntimeError),
                qualifiers: vec![Qualifier::Exit {
                    code: 0xc0000005,
                    ntstatus: Some("STATUS_ACCESS_VIOLATION".into()),
                }],
            },
        },
        Row {
            name: "infrastructure_primary_cause",
            verdict: TestVerdict::Infrastructure,
            class: infra(),
            check: None,
            context: context.clone(),
            expected: DisplayLabel {
                token: Some(LabelToken::Infrastructure),
                qualifiers: vec![Qualifier::PrimaryCause(InfrastructureCause::StatusUnknown)],
            },
        },
        Row {
            name: "internal_invariant",
            verdict: TestVerdict::InternalError {
                invariant: Invariant::SummaryUnderivable,
            },
            class: RunClass::ExitedZero,
            check: None,
            context: context.clone(),
            expected: DisplayLabel {
                token: Some(LabelToken::Internal),
                qualifiers: vec![Qualifier::Invariant(Invariant::SummaryUnderivable)],
            },
        },
        Row {
            name: "not_run_reason",
            verdict: TestVerdict::NotRun {
                reason: SkipReason::AdmissionClosed,
            },
            class: RunClass::ExitedZero,
            check: None,
            context: context.clone(),
            expected: DisplayLabel {
                token: Some(LabelToken::NotRun),
                qualifiers: vec![Qualifier::NotRun(SkipReason::AdmissionClosed)],
            },
        },
        Row {
            name: "diagnostic_match_keeps_run_label",
            verdict: TestVerdict::WallDeadline,
            class: RunClass::WallDeadline,
            check: Some(check(CheckKind::Passed, true)),
            context: context.clone(),
            expected: DisplayLabel {
                token: Some(LabelToken::TimeLimit),
                qualifiers: vec![
                    Qualifier::WallDeadline {
                        deadline: None,
                        measured_wall: None,
                    },
                    Qualifier::OutputMatched,
                ],
            },
        },
        Row {
            name: "diagnostic_difference_keeps_run_label",
            verdict: TestVerdict::Signal,
            class: RunClass::Signal {
                signal: 11,
                core_dumped: false,
            },
            check: Some(check(CheckKind::Failed { diag: mismatch() }, true)),
            context: context.clone(),
            expected: DisplayLabel {
                token: Some(LabelToken::RuntimeError),
                qualifiers: vec![
                    Qualifier::Signal {
                        signal: 11,
                        name: None,
                        core_dumped: false,
                    },
                    Qualifier::OutputDiffers(mismatch()),
                ],
            },
        },
        Row {
            name: "cancelled_check_does_not_gain_signal_qualifier",
            verdict: TestVerdict::Cancelled,
            class: RunClass::Signal {
                signal: 11,
                core_dumped: false,
            },
            check: Some(not_admitted(CheckNotAdmitted::Cancelled)),
            context,
            expected: DisplayLabel {
                token: Some(LabelToken::Cancelled),
                qualifiers: vec![],
            },
        },
    ];
    for row in rows {
        let actual = label(
            LabelSubject::Test {
                verdict: &row.verdict,
                class: Some(&row.class),
                check: row.check.as_ref(),
            },
            &row.context,
        );
        assert_eq!(actual, row.expected, "{}", row.name);
        if row.name == "passed_cleanup_unverified" {
            assert_eq!(
                format!(
                    "{} ({})",
                    actual.token.unwrap().wire_token(),
                    actual.qualifiers[0].fixed_text().unwrap()
                ),
                "OK (cleanup not verified)"
            );
        }
    }
    let metric = supervisor::Measurement {
        kind: supervisor::MetricKind::PeakRss,
        value: None,
        unit: supervisor::Unit::Bytes,
        scope: supervisor::Scope::Root,
        aggregation: supervisor::Aggregation::MaxOfProcesses,
        completeness: supervisor::Completeness::Partial,
        method: "wait4_ru_maxrss",
        reason: Some("accounting_failed".into()),
    };
    let context = DisplayContext {
        measurements: vec![metric.clone()],
        ..DisplayContext::default()
    };
    let actual = label(LabelSubject::Job(JobVerdict::Passed), &context);
    assert_eq!(actual.qualifiers, vec![Qualifier::Measurement(metric)]);
    let cause = BuildCause::OutputTooLarge {
        bytes: 101,
        cap: 100,
    };
    let actual = label(
        LabelSubject::Build(&BuildResult::Failed {
            cause: cause.clone(),
        }),
        &DisplayContext::default(),
    );
    assert_eq!(
        actual,
        DisplayLabel {
            token: Some(LabelToken::CompileError),
            qualifiers: vec![Qualifier::BuildCause(cause)]
        }
    );
}
#[test]
fn summary_fixtures() {
    let rows = [
        (
            "job_summary_tokens",
            SummaryTemplate::Passed {
                passed: 2,
                selected: 3,
                comparator: "tokens",
            },
            true,
            false,
            "2/3 passed (tokens comparison)",
        ),
        (
            "job_summary_exact",
            SummaryTemplate::Passed {
                passed: 3,
                selected: 3,
                comparator: "exact",
            },
            true,
            false,
            "3/3 passed (exact comparison)",
        ),
        (
            "job_summary_lines",
            SummaryTemplate::Passed {
                passed: 1,
                selected: 1,
                comparator: "lines",
            },
            true,
            false,
            "1/1 passed (lines comparison)",
        ),
        (
            "job_summary_float",
            SummaryTemplate::Passed {
                passed: 1,
                selected: 1,
                comparator: "float",
            },
            true,
            false,
            "1/1 passed (float comparison)",
        ),
        (
            "unchecked",
            SummaryTemplate::Unchecked { count: 2 },
            true,
            false,
            "2 unchecked (no answer): not verified",
        ),
        (
            "no_tests",
            SummaryTemplate::NoTests,
            true,
            false,
            "no tests: nothing verified",
        ),
        (
            "interrupted_after_completion",
            SummaryTemplate::Passed {
                passed: 3,
                selected: 3,
                comparator: "tokens",
            },
            true,
            true,
            "3/3 passed (tokens comparison) (interrupted after completion)",
        ),
        (
            "unchecked_interrupted_after_completion",
            SummaryTemplate::Unchecked { count: 2 },
            true,
            true,
            "2 unchecked (no answer): not verified (interrupted after completion)",
        ),
        (
            "incomplete_does_not_claim_completed",
            SummaryTemplate::Passed {
                passed: 1,
                selected: 3,
                comparator: "tokens",
            },
            false,
            true,
            "1/3 passed (tokens comparison)",
        ),
    ];
    for (name, template, complete, intent, expected) in rows {
        let actual = summary(template, complete, intent).render();
        assert_eq!(actual, expected, "{name}");
        assert!(
            !actual.contains("accepted") && !actual.contains("correct"),
            "{name}"
        );
    }
}

fn root_outcome() -> supervisor::RootOutcome {
    let capture = supervisor::StreamCapture {
        observed: 0,
        retained: vec![],
        stored: 0,
        forwarded: 0,
        eof: true,
        exceeded_at: None,
        losses: vec![],
        sink: None,
    };
    supervisor::RootOutcome {
        output_file: None,
        #[cfg(all(
            feature = "storage-test-hooks",
            any(target_os = "linux", target_os = "macos")
        ))]
        milestones: None,
        kill_attempts: 0,
        stdout: Some(capture.clone()),
        stderr: Some(capture),
        status: Termination::Exited { code: 0 },
        stop_trigger: None,
        enforced_at: None,
        termination: supervisor::TerminationEvidence {
            requested_at: None,
            method: None,
            cleanup: Cleanup::Confirmed,
        },
        violations: vec![],
        measurements: vec![],
        limits: vec![],
        timing: supervisor::RootTiming {
            setup: Duration::ZERO,
            create_end: std::time::Instant::now(),
            exit_observed: None,
            observer: supervisor::ObserverKind::Polling,
            wall: Some(Duration::ZERO),
            method: supervisor::WallMethod::Observed,
            grace: supervisor::DEADLINE_GRACE,
        },
        notes: vec![],
        errors: vec![],
    }
}
#[test]
fn supervisor_adapter_fixtures() {
    use supervisor::session::{BoundedText, RunRecord, StartFailure, StartFailureKind};
    struct Row {
        name: &'static str,
        record: RunRecord,
        consumes_stdout: bool,
        policies: [supervisor::Exceed; 2],
        expected: Result<RunClass, Invariant>,
    }
    let record = |kind| {
        RunRecord::NotStarted(StartFailure {
            kind,
            code: 5,
            text: BoundedText::new("observed failure"),
        })
    };
    let discard = [supervisor::Exceed::Discard; 2];
    let mut both = root_outcome();
    both.stdout.as_mut().unwrap().exceeded_at = Some(101);
    both.stderr.as_mut().unwrap().exceeded_at = Some(101);
    both.violations.push(supervisor::Violation::OutputBudget {
        stream: supervisor::Stream::Stderr,
        offset: 101,
    });
    let mut independent = both.clone();
    independent
        .stdout
        .as_mut()
        .unwrap()
        .losses
        .push(CaptureLoss::WriteError(5));
    let mut deadline = root_outcome();
    deadline
        .violations
        .push(supervisor::Violation::WallDeadline {
            observed_by: std::time::Instant::now(),
        });
    let mut notes = root_outcome();
    notes
        .notes
        .push(supervisor::Note::LateTrigger(StopTrigger::WallDeadline));
    notes.termination.cleanup = Cleanup::Unverified {
        reason: supervisor::UnverifiedReason::ObserverFailed,
    };
    let rows = [
        Row {
            name: "cancelled_during_creation",
            record: RunRecord::Transferred,
            consumes_stdout: true,
            policies: discard,
            expected: Ok(RunClass::Cancelled),
        },
        Row {
            name: "cancelled_before_start",
            record: record(StartFailureKind::CancelledBeforeStart),
            consumes_stdout: true,
            policies: discard,
            expected: Ok(RunClass::Cancelled),
        },
        Row {
            name: "run_dir_create_existing_setup",
            record: record(StartFailureKind::Setup),
            consumes_stdout: true,
            policies: discard,
            expected: Ok(causes([InfrastructureCause::NotStarted {
                reason: NotStartedReason::Setup,
            }])),
        },
        Row {
            name: "invalid_limit_has_no_contract_token",
            record: record(StartFailureKind::InvalidLimit),
            consumes_stdout: true,
            policies: discard,
            expected: Err(Invariant::SummaryUnderivable),
        },
        Row {
            name: "both_kill_policies",
            record: RunRecord::Started(Box::new(both.clone())),
            consumes_stdout: true,
            policies: [supervisor::Exceed::Kill; 2],
            expected: Ok(output_budget(true, true)),
        },
        Row {
            name: "discard_raw",
            record: RunRecord::Started(Box::new(both)),
            consumes_stdout: false,
            policies: discard,
            expected: Ok(RunClass::ExitedZero),
        },
        Row {
            name: "independent_write_error",
            record: RunRecord::Started(Box::new(independent)),
            consumes_stdout: true,
            policies: [supervisor::Exceed::Kill; 2],
            expected: Ok(causes([InfrastructureCause::CaptureLoss {
                stream: Stream::Stdout,
                loss: LossKind::WriteError,
            }])),
        },
        Row {
            name: "wall_violation",
            record: RunRecord::Started(Box::new(deadline)),
            consumes_stdout: true,
            policies: discard,
            expected: Ok(RunClass::WallDeadline),
        },
        Row {
            name: "late_trigger_and_unverified_are_notes",
            record: RunRecord::Started(Box::new(notes)),
            consumes_stdout: true,
            policies: discard,
            expected: Ok(RunClass::ExitedZero),
        },
    ];
    for row in rows {
        if let RunRecord::Started(root) = &row.record {
            let policies = row.policies.map(|policy| match policy {
                supervisor::Exceed::Kill => crate::config::CapPolicy::Kill,
                supervisor::Exceed::Discard => crate::config::CapPolicy::Discard,
            });
            let build = crate::build::BuildExecutionOutcome::from(&**root);
            let build_input = build.run_input(policies);
            let report_input = RunInput::from_root(root, false, row.policies);
            assert_eq!(
                classify_run(&build_input),
                classify_run(&report_input),
                "{} build",
                row.name
            );
            assert_eq!(
                run_notes(&build_input),
                run_notes(&report_input),
                "{} build notes",
                row.name
            );
        }
        let actual = RunInput::from_record(&row.record, row.consumes_stdout, row.policies)
            .map(|input| classify_run(&input));
        assert_eq!(actual, row.expected, "{}", row.name);
    }
}
#[test]
fn build_validation_adapter_fixtures() {
    use crate::build::{BuildFailure, BuildFailureKind as F, BuildId};
    let root = std::path::PathBuf::from("build-root");
    let rows = [
        (
            "missing_output",
            F::OutputMissing(root.join("main")),
            PostBuild {
                missing_outputs: vec!["main".into()],
                ..PostBuild::default()
            },
        ),
        (
            "missing_root_is_not_a_declared_output",
            F::OutputMissing(root.clone()),
            PostBuild {
                invariant: Some(Invariant::BuildEvidence),
                ..PostBuild::default()
            },
        ),
        (
            "nonregular_output_is_missing",
            F::OutputNotRegular(root.join("main")),
            PostBuild {
                missing_outputs: vec!["main".into()],
                ..PostBuild::default()
            },
        ),
        (
            "link_output_is_missing",
            F::OutputThroughLink(root.join("main")),
            PostBuild {
                missing_outputs: vec!["main".into()],
                ..PostBuild::default()
            },
        ),
        (
            "nonregular_root_is_not_a_declared_output",
            F::OutputNotRegular(root.clone()),
            PostBuild {
                invariant: Some(Invariant::BuildEvidence),
                ..PostBuild::default()
            },
        ),
        (
            "measurement_incomplete_unmapped",
            F::IncompleteMeasurement(crate::storage::WalkReport::default()),
            PostBuild {
                invariant: Some(Invariant::BuildEvidence),
                ..PostBuild::default()
            },
        ),
        (
            "overflow_is_independent_of_primary_diagnostics",
            F::BuildOutputTooLarge,
            PostBuild::default(),
        ),
        (
            "empty_generated_program_diagnostics",
            F::GeneratedProgramNotLaunchable(crate::exec::ResolveErrors { errors: vec![] }),
            PostBuild {
                invariant: Some(Invariant::BuildEvidence),
                ..PostBuild::default()
            },
        ),
    ];
    for (name, kind, expected) in rows {
        let failure = BuildFailure {
            id: BuildId::new("build").unwrap(),
            output_root: root.clone(),
            post_build: Box::new(PostBuild::from_failure_kind(&kind, &root)),
            kind: Box::new(kind),
            over_cap: None,
        };
        assert_eq!(
            PostBuild::from_validation(Err(&failure)),
            expected,
            "{name}"
        );
    }
}

#[test]
fn wire_token_fixtures() {
    let coverage = [
        (Coverage::Started, "started"),
        (Coverage::Transferred, "transferred"),
        (
            Coverage::NotStarted {
                reason: NotStartedReason::Setup,
            },
            "not_started",
        ),
        (
            Coverage::Skipped {
                reason: SkipReason::Cancelled,
            },
            "skipped",
        ),
        (Coverage::Admitted, "admitted"),
    ];
    for (value, expected) in coverage {
        assert_eq!(value.wire_token(), expected);
    }
    let classes = classes();
    for (value, expected) in classes.iter().zip([
        "infrastructure",
        "cancelled",
        "wall_deadline",
        "output_budget",
        "signal",
        "nonzero_exit",
        "exited_zero",
    ]) {
        assert_eq!(value.wire_token(), expected);
    }
    for (value, expected) in check_kinds().iter().take(9).zip([
        "not_admitted",
        "not_admitted",
        "unchecked",
        "passed",
        "failed",
        "failed",
        "checker_failed",
        "check_error",
        "internal_error",
    ]) {
        assert_eq!(value.wire_token(), expected);
    }
    let rows = [
        (CheckKind::Unchecked, Some("no_answer"), None, None),
        (
            CheckKind::Failed { diag: mismatch() },
            Some("byte_mismatch"),
            Some("wrong_answer"),
            None,
        ),
        (
            CheckKind::Failed {
                diag: Box::new(Diag::IncompleteOutput),
            },
            Some("incomplete_output"),
            Some("wrong_answer"),
            None,
        ),
        (
            CheckKind::CheckerFailed {
                diag: invalid_answer(),
            },
            Some("invalid_answer"),
            None,
            Some("test_data_error"),
        ),
        (
            CheckKind::NotAdmitted {
                reason: CheckNotAdmitted::Cancelled,
            },
            Some("cancelled"),
            None,
            None,
        ),
        (
            CheckKind::NotAdmitted {
                reason: CheckNotAdmitted::RunNotEligible,
            },
            Some("run_not_eligible"),
            None,
            None,
        ),
        (CheckKind::Passed, None, None, None),
        (CheckKind::CheckError { os_error: None }, None, None, None),
        (
            CheckKind::InternalError {
                invariant: Invariant::CheckEvidence,
            },
            None,
            None,
            None,
        ),
    ];
    for (value, reason, fail, class) in rows {
        assert_eq!(value.reason_token(), reason);
        assert_eq!(value.fail_token(), fail);
        assert_eq!(value.class_token(), class);
    }
    let errors = [
        SupervisorErrorKind::Register,
        SupervisorErrorKind::Wait,
        SupervisorErrorKind::PipeRead,
        SupervisorErrorKind::Kill,
        SupervisorErrorKind::Reap,
        SupervisorErrorKind::ReapTimeout,
        SupervisorErrorKind::Accounting,
        SupervisorErrorKind::Worker,
    ];
    for (value, expected) in errors.into_iter().zip([
        "register",
        "wait",
        "pipe_read",
        "kill",
        "reap",
        "reap_timeout",
        "accounting",
        "worker",
    ]) {
        assert_eq!(value.wire_token(), expected);
    }
    for (value, expected) in [
        (
            InfrastructureCause::NotStarted {
                reason: NotStartedReason::Setup,
            },
            "not_started",
        ),
        (InfrastructureCause::StatusUnknown, "status_unknown"),
        (
            InfrastructureCause::CleanupIncomplete { survivors: 1 },
            "cleanup_incomplete",
        ),
        (
            InfrastructureCause::SupervisorError {
                error: SupervisorErrorKind::Wait,
            },
            "supervisor_error",
        ),
        (
            InfrastructureCause::CaptureLoss {
                stream: Stream::Stdout,
                loss: LossKind::WriteError,
            },
            "capture_loss",
        ),
    ] {
        assert_eq!(value.wire_token(), expected);
    }
    assert_eq!(Invariant::RecordMissing.wire_token(), "record_missing");
    assert_eq!(
        Invariant::SummaryUnderivable.wire_token(),
        "summary_underivable"
    );
    assert_eq!(output_budget(true, true).wire_token(), "output_budget");
    assert_eq!(
        BudgetStreams {
            stdout: true,
            stderr: true,
            output_file: false,
        }
        .streams()
        .collect::<Vec<_>>(),
        [Stream::Stdout, Stream::Stderr]
    );
    assert_eq!(
        BudgetStreams {
            stdout: true,
            stderr: true,
            output_file: false,
        }
        .primary(),
        Some(Stream::Stdout)
    );
}
