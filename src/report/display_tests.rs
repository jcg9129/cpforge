use super::*;
use std::time::Duration;

fn judge(profile: JudgeProfile, role: JudgeRole) -> Judge {
    Judge {
        role,
        execution: 1,
        profile,
        result: Some(JudgeResult::Passed),
        message: (profile == JudgeProfile::Testlib).then(|| JudgeText::message("judge message")),
        stderr: Some(JudgeText::new(b"simple stderr", 1024)),
        report: None,
    }
}
fn test_label(verdict: &TestVerdict, context: &DisplayContext) -> String {
    render_label(&label(
        LabelSubject::Test {
            verdict,
            class: None,
            check: None,
        },
        context,
    ))
}
fn points(value: &str) -> Score {
    Score::parse(value).unwrap()
}

#[test]
fn deciding_role_and_message_use_only_deciding_profile_evidence() {
    for (profile, role, name, message) in [
        (
            JudgeProfile::Testlib,
            JudgeRole::Checker,
            "checker",
            "judge message",
        ),
        (
            JudgeProfile::Testlib,
            JudgeRole::Interactor,
            "interactor",
            "judge message",
        ),
        (
            JudgeProfile::Simple,
            JudgeRole::Checker,
            "checker",
            "simple stderr",
        ),
    ] {
        let judge = judge(profile, role);
        let context = DisplayContext {
            judged: JudgedDisplayEvidence::from_judge(&judge, None).map(Box::new),
            comparator: Some("tokens"),
            ..Default::default()
        };
        for (verdict, prefix) in [
            (TestVerdict::Passed, "OK"),
            (TestVerdict::WrongAnswer, "WA"),
            (TestVerdict::PresentationError, "PE  presentation error"),
        ] {
            assert_eq!(
                test_label(&verdict, &context),
                format!("{prefix}  {name}: \"{message}\"")
            );
            if verdict == TestVerdict::PresentationError {
                assert_eq!(
                    render_label(&label_for_profile(
                        LabelSubject::Test {
                            verdict: &verdict,
                            class: None,
                            check: None
                        },
                        &context,
                        DisplayProfile::JudgeCf
                    )),
                    format!("WA  presentation error  {name}: \"{message}\"")
                );
            }
        }
    }
    let mut judge = judge(JudgeProfile::Testlib, JudgeRole::Interactor);
    judge.message = Some(JudgeText::message("accepted\n\u{1b}[31m\"quoted\""));
    let context = DisplayContext {
        judged: JudgedDisplayEvidence::from_judge(&judge, None).map(Box::new),
        ..Default::default()
    };
    assert_eq!(
        test_label(&TestVerdict::Passed, &context),
        "OK  interactor: \"accepted  [31m\"quoted\"\""
    );
    judge.message = None;
    let context = DisplayContext {
        judged: JudgedDisplayEvidence::from_judge(&judge, None).map(Box::new),
        ..Default::default()
    };
    assert_eq!(test_label(&TestVerdict::Passed, &context), "OK  interactor");
    judge.result = None;
    assert_eq!(JudgedDisplayEvidence::from_judge(&judge, None), None);
}

#[test]
fn judge_failure_class_reason_and_cleanup_replace_comparison_qualifiers() {
    let rows = [
        (
            JudgeRole::Checker,
            JudgeFailClass::Crashed,
            JudgeReason::Signal,
            Some(RunClass::Signal {
                signal: 11,
                core_dumped: false,
            }),
            "checker crashed: signal 11",
        ),
        (
            JudgeRole::Interactor,
            JudgeFailClass::ReportInvalid,
            JudgeReason::Disagreement,
            None,
            "interactor: report invalid (disagreement)",
        ),
        (
            JudgeRole::Validator,
            JudgeFailClass::TestDataError,
            JudgeReason::InvalidInput,
            None,
            "validator: invalid input",
        ),
        (
            JudgeRole::Validator,
            JudgeFailClass::TestDataError,
            JudgeReason::ValidationFailed,
            None,
            "validator: validation failed",
        ),
        (
            JudgeRole::Checker,
            JudgeFailClass::DeclaredFail,
            JudgeReason::DeclaredFail,
            None,
            "checker: declared fail (declared fail)",
        ),
        (
            JudgeRole::Checker,
            JudgeFailClass::ProtocolUnsupported,
            JudgeReason::ExitUnsupported,
            None,
            "checker: protocol unsupported (exit unsupported)",
        ),
        (
            JudgeRole::Checker,
            JudgeFailClass::ArtefactInvalid,
            JudgeReason::ToutMissing,
            None,
            "checker: artefact invalid (tout missing)",
        ),
    ];
    for (role, class, reason, run_class, expected) in rows {
        let mut judge = judge(JudgeProfile::Testlib, role);
        judge.result = Some(JudgeResult::JudgeFailed { class, reason });
        let context = DisplayContext {
            judged: JudgedDisplayEvidence::from_judge(&judge, run_class).map(Box::new),
            ..Default::default()
        };
        assert_eq!(
            test_label(&TestVerdict::CheckerFailed, &context),
            format!("CHECKER-FAILED  {expected}")
        );
    }
    let context = DisplayContext {
        judged: Some(Box::new(JudgedDisplayEvidence::CheckingUnverified)),
        ..Default::default()
    };
    assert_eq!(
        test_label(&TestVerdict::Unchecked, &context),
        "UNCHECKED  cleanup not verified: report not read"
    );
    for result in [
        JudgeResult::Unverified,
        JudgeResult::NotRead {
            reason: JudgeNotRead::CleanupIncomplete,
        },
    ] {
        let mut judge = judge(JudgeProfile::Testlib, JudgeRole::Checker);
        judge.result = Some(result);
        assert_eq!(
            JudgedDisplayEvidence::from_judge(&judge, None),
            Some(JudgedDisplayEvidence::CheckingUnverified)
        );
    }
}

#[test]
fn judged_check_alone_supplies_failure_score_and_cleanup_evidence() {
    let rows = [
        (
            JudgedCheck::JudgeFailed {
                role: JudgeRole::Interactor,
                class: JudgeFailClass::ReportInvalid,
                reason: JudgeReason::Disagreement,
            },
            TestVerdict::CheckerFailed,
            "CHECKER-FAILED  interactor: report invalid (disagreement)",
        ),
        (
            JudgedCheck::Unchecked {
                reason: CheckingUnchecked::CheckingUnverified,
            },
            TestVerdict::Unchecked,
            "UNCHECKED  cleanup not verified: report not read",
        ),
        (
            JudgedCheck::Unchecked {
                reason: CheckingUnchecked::CheckingInterrupted,
            },
            TestVerdict::Unchecked,
            "UNCHECKED  checking interrupted",
        ),
        (
            JudgedCheck::Scored {
                raw: points("12.5"),
                scale: points("100"),
                role: JudgeRole::Checker,
            },
            TestVerdict::Scored,
            "SCORE  12.5/100",
        ),
        (
            JudgedCheck::Rejected {
                fail: JudgeFail::WrongAnswer,
                role: JudgeRole::Checker,
            },
            TestVerdict::WrongAnswer,
            "WA  checker",
        ),
    ];
    for (outcome, verdict, expected) in rows {
        let check = CheckOutcome {
            kind: CheckKind::Judged {
                outcome: Box::new(outcome),
            },
            diagnostic: false,
        };
        assert_eq!(
            render_label(&label(
                LabelSubject::Test {
                    verdict: &verdict,
                    class: Some(&RunClass::ExitedZero),
                    check: Some(&check)
                },
                &DisplayContext::default()
            )),
            expected
        );
    }
}

#[test]
fn interactive_deadline_peer_and_idle_wording_preserves_labels() {
    let context = DisplayContext {
        peer_allowance_expired: true,
        wall_deadline: Some(Duration::from_secs(2)),
        session_deadline: Some(Duration::from_millis(3500)),
        idle_interval: Some(Duration::from_secs(4)),
        ..Default::default()
    };
    assert_eq!(
        test_label(&TestVerdict::WallDeadline, &context),
        "TLE  did not exit after the interactor finished"
    );
    assert_eq!(
        test_label(&TestVerdict::SessionLimit, &context),
        "TLE  session deadline 3.5s"
    );
    assert_eq!(
        test_label(&TestVerdict::Idle, &context),
        "IDLE  no successful forwarding observed for 4 s"
    );
    assert_eq!(
        test_label(
            &TestVerdict::WallDeadline,
            &DisplayContext {
                peer_allowance_expired: false,
                ..context
            }
        ),
        "TLE  wall deadline 2s"
    );
}

#[test]
fn every_role_build_names_role_and_cause() {
    for role in [RoleName::Checker, RoleName::Validator, RoleName::Interactor] {
        for (cause, expected) in [
            (
                RoleBuildCause::Run(RunClass::NonzeroExit { code: 2 }),
                "exit code 2",
            ),
            (
                RoleBuildCause::Run(RunClass::Signal {
                    signal: 9,
                    core_dumped: false,
                }),
                "signal 9",
            ),
            (
                RoleBuildCause::MissingOutput {
                    names: vec![],
                    names_dropped: 0,
                },
                "missing output",
            ),
            (
                RoleBuildCause::GeneratedProgramInvalid {
                    name: JudgeText::new(b"program", 128),
                    reason: "not_regular".into(),
                },
                "generated program invalid",
            ),
            (
                RoleBuildCause::OutputTooLarge { bytes: 11, cap: 10 },
                "output too large: 11 bytes, cap 10",
            ),
        ] {
            let result = RoleBuildResult::Failed {
                cause: Box::new(cause),
            };
            assert_eq!(
                render_label(&label(
                    LabelSubject::RoleBuild {
                        role,
                        result: &result
                    },
                    &DisplayContext::default()
                )),
                format!("CE  {} build: {expected}", role.wire_token())
            );
        }
        for (result, expected) in [
            (RoleBuildResult::Succeeded, "OK"),
            (RoleBuildResult::Cancelled, "CANCELLED"),
            (
                RoleBuildResult::Skipped {
                    reason: SkipReason::BuildNotSucceeded,
                },
                "NOT RUN  build not succeeded",
            ),
        ] {
            assert_eq!(
                render_label(&label(
                    LabelSubject::RoleBuild {
                        role,
                        result: &result
                    },
                    &DisplayContext::default()
                )),
                expected
            );
        }
    }
}

#[test]
fn test_and_job_points_include_incomplete_scores() {
    let context = DisplayContext {
        test_points: Some((points("12.5"), points("100"))),
        ..Default::default()
    };
    assert_eq!(
        test_label(&TestVerdict::Scored, &context),
        "SCORE  12.5/100"
    );
    for aggregation in [Aggregation::Sum, Aggregation::Min] {
        let token = if aggregation == Aggregation::Sum {
            "sum"
        } else {
            "min"
        };
        let context = DisplayContext {
            job_score: Some(AggregateScore::Points {
                aggregation,
                value: points("12.5"),
                max: points("100"),
                full: false,
            }),
            ..Default::default()
        };
        assert_eq!(
            render_label(&label(LabelSubject::Job(JobVerdict::Scored), &context)),
            format!("SCORE  12.5/100 ({token})")
        );
        let context = DisplayContext {
            job_score: Some(AggregateScore::Incomplete {
                aggregation,
                missing: 2,
            }),
            ..Default::default()
        };
        assert_eq!(
            render_label(&label(LabelSubject::Job(JobVerdict::Scored), &context)),
            "SCORE  incomplete: 2 tests without a score"
        );
    }
}

#[test]
fn judged_summary_fixtures_and_validated_input_suffix() {
    let mut templates = vec![];
    for profile in [JudgeProfile::Simple, JudgeProfile::Testlib] {
        templates.push(SummaryTemplate::Checker {
            passed: 2,
            selected: 3,
            profile,
        });
    }
    for checking in [
        InteractiveChecking::Interactor,
        InteractiveChecking::Checker,
        InteractiveChecking::InteractorChecker,
    ] {
        templates.push(SummaryTemplate::Interactive {
            passed: 2,
            selected: 3,
            checking,
        });
    }
    for aggregation in [Aggregation::Sum, Aggregation::Min] {
        templates.push(SummaryTemplate::Points {
            value: points("12.5"),
            max: points("100"),
            aggregation,
            selected: 3,
        });
    }
    templates.push(SummaryTemplate::IncompleteScore { missing: 2 });
    let strings: Vec<_> = templates
        .into_iter()
        .map(|t| summary(t, true, true).with_validated_inputs().render())
        .collect();
    assert_eq!(
        serde_json::to_vec(&strings).unwrap(),
        include_bytes!("wire/fixtures/judged_summaries.json")
    );
    for text in strings {
        assert!(!text.contains("accepted") && !text.contains("correct"));
    }
    assert_eq!(
        summary(SummaryTemplate::NoTests, false, true)
            .with_file_io()
            .with_validated_inputs()
            .render(),
        "no tests: nothing verified (file I/O) (inputs validated)"
    );
}
