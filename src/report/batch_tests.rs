use super::*;

fn score(v: &str) -> Score {
    Score::parse(v).unwrap()
}
fn points(aggregation: Aggregation) -> Scoring {
    Scoring::Points {
        scale: score("100"),
        aggregation,
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
fn infra() -> RunClass {
    RunClass::Infrastructure {
        causes: InfrastructureCauses::new([InfrastructureCause::StatusUnknown]),
    }
}
fn judge() -> JudgeInput {
    JudgeInput {
        execution: started(RunClass::ExitedZero),
        profile: JudgeProfile::Testlib,
        cleanup_unverified: false,
        read: JudgeRead::Parsed {
            outcome: TestlibOutcome::Accepted,
            raw: None,
        },
    }
}
fn judge_failed(class: JudgeFailClass, role: JudgeRole, reason: JudgeReason) -> CheckKind {
    CheckKind::Judged {
        outcome: Box::new(JudgedCheck::JudgeFailed {
            class,
            role,
            reason,
        }),
    }
}
fn rejected(fail: JudgeFail) -> CheckKind {
    CheckKind::Judged {
        outcome: Box::new(JudgedCheck::Rejected {
            fail,
            role: JudgeRole::Checker,
        }),
    }
}
fn unchecked(reason: CheckingUnchecked) -> CheckKind {
    CheckKind::Judged {
        outcome: Box::new(JudgedCheck::Unchecked { reason }),
    }
}
fn row(id: &str) {
    use JudgeFailClass as C;
    use JudgeReason as R;
    let mut validator = judge();
    let mut solution = started(RunClass::ExitedZero);
    let mut checker = judge();
    let comparison = Comparison::Outcome(crate::compare::CheckOutcome::Passed);
    let mut compare = false;
    let mut output = None;
    let mut expected = TestVerdict::CheckerFailed;
    let mut kind = CheckKind::NotAdmitted {
        reason: CheckNotAdmitted::RunNotEligible,
    };
    let mut diagnostic = false;
    match id {
        "J-B01" | "J-B09" | "J-B13" => {
            let execution = skipped(SkipReason::Cancelled);
            match id {
                "J-B01" => validator.execution = execution,
                "J-B09" => solution = execution,
                _ => checker.execution = execution,
            }
            kind = CheckKind::NotAdmitted {
                reason: CheckNotAdmitted::Cancelled,
            };
            expected = TestVerdict::Cancelled;
        }
        "J-B02" | "J-B10" | "J-B14" => {
            let reason = if id == "J-B14" {
                SkipReason::AdmissionClosed
            } else {
                SkipReason::BuildNotSucceeded
            };
            let execution = skipped(reason);
            match id {
                "J-B02" => validator.execution = execution,
                "J-B10" => solution = execution,
                _ => checker.execution = execution,
            }
            expected = TestVerdict::NotRun { reason };
        }
        "J-B03" | "J-B11" | "J-B17" => {
            let execution = ExecutionResult {
                coverage: Coverage::NotStarted {
                    reason: NotStartedReason::CreateFailed,
                },
                class: Some(infra()),
                invariant: None,
            };
            match id {
                "J-B03" => validator.execution = execution,
                "J-B11" => solution = execution,
                _ => checker.execution = execution,
            }
            expected = TestVerdict::Infrastructure;
        }
        "J-B04" | "J-B12" => {
            let execution = ExecutionResult {
                coverage: Coverage::Transferred,
                class: Some(RunClass::Cancelled),
                invariant: None,
            };
            if id == "J-B04" {
                validator.execution = started(RunClass::Cancelled);
            } else {
                solution = execution;
            }
            expected = TestVerdict::Cancelled;
        }
        "J-B05" | "J-B06" | "J-B07" => {
            validator.execution = started(RunClass::NonzeroExit {
                code: if id == "J-B06" { 1 } else { 3 },
            });
            validator.profile = if id == "J-B05" {
                JudgeProfile::Testlib
            } else {
                JudgeProfile::Simple
            };
            let (class, reason) = match id {
                "J-B05" => (C::TestDataError, R::ValidationFailed),
                "J-B06" => (C::TestDataError, R::InvalidInput),
                _ => (C::Crashed, R::ExitCode),
            };
            kind = judge_failed(class, JudgeRole::Validator, reason);
        }
        "J-B08" => {
            compare = true;
            kind = CheckKind::Passed;
            expected = TestVerdict::Passed;
        }
        "J-B14a" => {
            output = Some(OutputFile::Missing);
            checker.execution = skipped(SkipReason::NotEligible);
            kind = CheckKind::OutputRejected {
                reason: OutputRejectReason::Missing,
            };
            expected = TestVerdict::WrongAnswer;
        }
        "J-B15" => {
            checker.execution = started(RunClass::Cancelled);
            kind = unchecked(CheckingUnchecked::CheckingInterrupted);
            expected = TestVerdict::Cancelled;
        }
        "J-B16" => {
            solution = started(RunClass::WallDeadline);
            kind = CheckKind::Passed;
            expected = TestVerdict::WallDeadline;
            diagnostic = true;
        }
        "J-B18" | "J-B19" | "J-B20" => {
            checker.profile = JudgeProfile::Simple;
            checker.execution = started(match id {
                "J-B18" => RunClass::ExitedZero,
                "J-B19" => RunClass::NonzeroExit { code: 1 },
                _ => RunClass::NonzeroExit { code: 2 },
            });
            match id {
                "J-B18" => {
                    kind = CheckKind::Passed;
                    expected = TestVerdict::Passed;
                }
                "J-B19" => {
                    kind = rejected(JudgeFail::WrongAnswer);
                    expected = TestVerdict::WrongAnswer;
                }
                _ => kind = judge_failed(C::Crashed, JudgeRole::Checker, R::ExitCode),
            }
        }
        "J-B21" => {
            checker.execution = started(RunClass::WallDeadline);
            kind = judge_failed(C::Crashed, JudgeRole::Checker, R::WallDeadline);
        }
        "J-B22" => {
            checker.cleanup_unverified = true;
            kind = unchecked(CheckingUnchecked::CheckingUnverified);
            expected = TestVerdict::Unchecked;
        }
        "J-B23" => {
            checker.read = JudgeRead::Io { os_error: Some(5) };
            kind = CheckKind::CheckError { os_error: Some(5) };
            expected = TestVerdict::Infrastructure;
        }
        "J-B24" | "J-B25" | "J-B26" | "J-B27" | "J-B28" | "J-B29" => {
            let reason = match id {
                "J-B24" => R::ReportMissing,
                "J-B25" => R::ReportTooLarge,
                "J-B26" => R::ReportUndecodable,
                "J-B27" => R::ReportMalformed,
                "J-B28" => R::ReportSchema,
                _ => R::PointsInvalid,
            };
            checker.read = JudgeRead::Invalid(reason);
            kind = judge_failed(C::ReportInvalid, JudgeRole::Checker, reason);
        }
        "J-B30" => {
            checker.execution = started(RunClass::NonzeroExit { code: 42 });
            kind = judge_failed(
                C::ProtocolUnsupported,
                JudgeRole::Checker,
                R::ExitUnsupported,
            );
        }
        "J-B31" => {
            checker.read = JudgeRead::Parsed {
                outcome: TestlibOutcome::Unsupported,
                raw: None,
            };
            kind = judge_failed(
                C::ProtocolUnsupported,
                JudgeRole::Checker,
                R::OutcomeUnsupported,
            );
        }
        "J-B32" => {
            checker.read = JudgeRead::Parsed {
                outcome: TestlibOutcome::WrongAnswer,
                raw: None,
            };
            kind = judge_failed(C::ReportInvalid, JudgeRole::Checker, R::Disagreement);
        }
        "J-B33" | "J-B34" | "J-B35" | "J-B36" | "J-B37" => {
            let (exit, outcome) = match id {
                "J-B33" => (0, TestlibOutcome::Accepted),
                "J-B34" => (1, TestlibOutcome::WrongAnswer),
                "J-B35" => (2, TestlibOutcome::PresentationError),
                "J-B36" => (3, TestlibOutcome::Fail),
                _ => (7, TestlibOutcome::Points),
            };
            checker.execution = started(if exit == 0 {
                RunClass::ExitedZero
            } else {
                RunClass::NonzeroExit { code: exit }
            });
            checker.read = JudgeRead::Parsed {
                outcome,
                raw: (exit == 7).then_some(score("12.5")),
            };
            match exit {
                0 => {
                    kind = CheckKind::Passed;
                    expected = TestVerdict::Passed;
                }
                1 => {
                    kind = rejected(JudgeFail::WrongAnswer);
                    expected = TestVerdict::WrongAnswer;
                }
                2 => {
                    kind = rejected(JudgeFail::PresentationError);
                    expected = TestVerdict::PresentationError;
                }
                3 => kind = judge_failed(C::DeclaredFail, JudgeRole::Checker, R::DeclaredFail),
                _ => {
                    kind = CheckKind::Judged {
                        outcome: Box::new(JudgedCheck::Scored {
                            raw: score("12.5"),
                            scale: score("100"),
                            role: JudgeRole::Checker,
                        }),
                    };
                    expected = TestVerdict::Scored;
                }
            }
        }
        _ => panic!("unknown row"),
    }
    let actual = batch_outcome(&BatchInput {
        validator: Some(&validator),
        solution: &solution,
        checker: (!compare).then_some(&checker),
        scoring: points(Aggregation::Sum),
        admission: CheckAdmission::NoIntent,
        answer_present: true,
        comparison: &comparison,
        output_file: output.as_ref(),
    });
    assert_eq!(
        actual,
        (CheckOutcome { kind, diagnostic }, expected),
        "{id}"
    );
}
macro_rules! rows { ($($name:ident => $id:literal),+ $(,)?) => { $(#[test] fn $name() { row($id); })+ }; }
rows! {
    j_b01 => "J-B01", j_b02 => "J-B02", j_b03 => "J-B03", j_b04 => "J-B04", j_b05 => "J-B05",
    j_b06 => "J-B06", j_b07 => "J-B07", j_b08 => "J-B08", j_b09 => "J-B09", j_b10 => "J-B10",
    j_b11 => "J-B11", j_b12 => "J-B12", j_b13 => "J-B13", j_b14 => "J-B14", j_b14a => "J-B14a",
    j_b15 => "J-B15", j_b16 => "J-B16", j_b17 => "J-B17", j_b18 => "J-B18", j_b19 => "J-B19",
    j_b20 => "J-B20", j_b21 => "J-B21", j_b22 => "J-B22", j_b23 => "J-B23", j_b24 => "J-B24",
    j_b25 => "J-B25", j_b26 => "J-B26", j_b27 => "J-B27", j_b28 => "J-B28", j_b29 => "J-B29",
    j_b30 => "J-B30", j_b31 => "J-B31", j_b32 => "J-B32", j_b33 => "J-B33", j_b34 => "J-B34",
    j_b35 => "J-B35", j_b36 => "J-B36", j_b37 => "J-B37",
}
fn checked(
    validator: ExecutionResult,
    solution: ExecutionResult,
    checker: ExecutionResult,
    output: Option<OutputFile>,
) -> (CheckOutcome, TestVerdict) {
    let validator = JudgeInput {
        execution: validator,
        ..judge()
    };
    let checker = JudgeInput {
        execution: checker,
        ..judge()
    };
    let comparison = Comparison::NotRun;
    batch_outcome(&BatchInput {
        validator: Some(&validator),
        solution: &solution,
        checker: Some(&checker),
        scoring: points(Aggregation::Sum),
        admission: CheckAdmission::NoIntent,
        answer_present: true,
        comparison: &comparison,
        output_file: output.as_ref(),
    })
}
fn pair(kind: CheckKind, diagnostic: bool, verdict: TestVerdict) -> (CheckOutcome, TestVerdict) {
    (CheckOutcome { kind, diagnostic }, verdict)
}
fn not_eligible_check() -> CheckKind {
    CheckKind::NotAdmitted {
        reason: CheckNotAdmitted::RunNotEligible,
    }
}
#[test]
fn j_b04_transferred_and_cancelled_before_start_validators() {
    for validator in [
        ExecutionResult {
            coverage: Coverage::Transferred,
            class: Some(RunClass::Cancelled),
            invariant: None,
        },
        ExecutionResult {
            coverage: Coverage::NotStarted {
                reason: NotStartedReason::CancelledBeforeStart,
            },
            class: Some(RunClass::Cancelled),
            invariant: None,
        },
    ] {
        assert_eq!(
            checked(
                validator,
                skipped(SkipReason::Cancelled),
                skipped(SkipReason::Cancelled),
                None
            ),
            pair(not_eligible_check(), false, TestVerdict::Cancelled)
        );
    }
}
#[test]
fn j_b12_solution_stopped_by_intent() {
    assert_eq!(
        checked(
            started(RunClass::ExitedZero),
            started(RunClass::Cancelled),
            skipped(SkipReason::NotEligible),
            None
        ),
        pair(not_eligible_check(), false, TestVerdict::Cancelled)
    );
}
#[test]
fn j_b14a_uncollected_output_is_infrastructure() {
    let output = OutputFile::NotCollected {
        reason: CollectionFailure::Io,
        os_error: Some(5),
    };
    assert_eq!(
        checked(
            started(RunClass::ExitedZero),
            started(RunClass::ExitedZero),
            skipped(SkipReason::NotEligible),
            Some(output)
        ),
        pair(
            CheckKind::CheckError { os_error: Some(5) },
            false,
            TestVerdict::Infrastructure
        )
    );
}
#[test]
fn j_b16_ineligible_checker_and_diagnostic_collection() {
    let not_started = ExecutionResult {
        coverage: Coverage::NotStarted {
            reason: NotStartedReason::CreateFailed,
        },
        class: Some(infra()),
        invariant: None,
    };
    for (class, verdict) in [
        (RunClass::WallDeadline, TestVerdict::WallDeadline),
        (
            RunClass::Signal {
                signal: 11,
                core_dumped: false,
            },
            TestVerdict::Signal,
        ),
        (RunClass::NonzeroExit { code: 1 }, TestVerdict::NonzeroExit),
    ] {
        for checker in [
            skipped(SkipReason::AdmissionClosed),
            not_started.clone(),
            started(infra()),
        ] {
            assert_eq!(
                checked(
                    started(RunClass::ExitedZero),
                    started(class.clone()),
                    checker,
                    None
                ),
                pair(not_eligible_check(), false, verdict.clone())
            );
        }
        let mut rejecting = judge();
        rejecting.execution = started(RunClass::NonzeroExit { code: 1 });
        rejecting.read = JudgeRead::Parsed {
            outcome: TestlibOutcome::WrongAnswer,
            raw: None,
        };
        let solution = started(class.clone());
        let comparison = Comparison::NotRun;
        assert_eq!(
            batch_outcome(&BatchInput {
                validator: None,
                solution: &solution,
                checker: Some(&rejecting),
                scoring: Scoring::PassFail,
                admission: CheckAdmission::NoIntent,
                answer_present: true,
                comparison: &comparison,
                output_file: None,
            }),
            pair(rejected(JudgeFail::WrongAnswer), true, verdict.clone())
        );
        assert_eq!(
            checked(
                started(RunClass::ExitedZero),
                started(class),
                skipped(SkipReason::NotEligible),
                Some(OutputFile::Missing)
            ),
            pair(
                CheckKind::OutputRejected {
                    reason: OutputRejectReason::Missing,
                },
                true,
                verdict
            )
        );
    }
}
#[test]
fn j_b38_orphans_are_internal_errors() {
    let orphan = TestVerdict::InternalError {
        invariant: Invariant::CheckEvidence,
    };
    // A collected (or no) output file leaves no reason to skip the checker.
    for output in [
        None,
        Some(OutputFile::Collected {
            bytes: 1,
            sha256: [0; 32],
            path: "r00001/out/output".into(),
        }),
    ] {
        let actual = checked(
            started(RunClass::ExitedZero),
            started(RunClass::ExitedZero),
            skipped(SkipReason::NotEligible),
            output,
        );
        assert_eq!(actual.1, orphan);
    }
    let actual = checked(
        started(RunClass::ExitedZero),
        skipped(SkipReason::InputNotValidated),
        skipped(SkipReason::InputNotValidated),
        None,
    );
    assert_eq!(actual.1, orphan);
    // Points evidence outside a `points` outcome never yields success.
    let mut checker = judge();
    checker.read = JudgeRead::Parsed {
        outcome: TestlibOutcome::Accepted,
        raw: Some(score("1")),
    };
    let solution = started(RunClass::ExitedZero);
    let comparison = Comparison::NotRun;
    let actual = batch_outcome(&BatchInput {
        validator: None,
        solution: &solution,
        checker: Some(&checker),
        scoring: points(Aggregation::Sum),
        admission: CheckAdmission::NoIntent,
        answer_present: true,
        comparison: &comparison,
        output_file: None,
    });
    assert_eq!(actual.1, orphan);
}
#[test]
fn judge_text_heads_follow_the_summary_rules() {
    // An eligible value is cut back to a character boundary.
    let text = JudgeText::new("aé".as_bytes(), 2);
    assert_eq!((text.head.as_slice(), text.len), (&b"a"[..], 3));
    // An ineligible value (a newline) keeps exactly P bytes, as `nsum(P)`.
    let text = JudgeText::new("\né".as_bytes(), 2);
    assert_eq!(text.head, b"\n\xc3");
    // A decoded message stays UTF-8 and keeps the whole length and hash.
    let message = "é\n".repeat(2048);
    let text = JudgeText::message(&message);
    assert_eq!(text.head.len(), 4095);
    assert_eq!(text.len, 6144);
    assert!(std::str::from_utf8(&text.head).is_ok());
}
#[test]
fn j_b51_scores_and_aggregation() {
    for (aggregation, value, max) in [
        (Aggregation::Sum, "112.5", "300"),
        (Aggregation::Min, "0", "100"),
    ] {
        let scoring = points(aggregation);
        let scores = [
            test_score(scoring, &TestVerdict::Scored, Some(score("12.5"))).unwrap(),
            test_score(scoring, &TestVerdict::Passed, None).unwrap(),
            test_score(scoring, &TestVerdict::WrongAnswer, None).unwrap(),
        ];
        assert_eq!(
            aggregate_score(scoring, &scores).unwrap(),
            Some(AggregateScore::Points {
                aggregation,
                value: score(value),
                max: score(max),
                full: false
            })
        );
        assert_eq!(
            aggregate_score(scoring, &[Some(score("100")); 3]).unwrap(),
            Some(AggregateScore::Points {
                aggregation,
                value: score(max),
                max: score(max),
                full: true
            })
        );
        assert_eq!(
            aggregate_score(scoring, &[scores[0], None]).unwrap(),
            Some(AggregateScore::Incomplete {
                aggregation,
                missing: 1
            })
        );
        assert_eq!(
            aggregate_score(scoring, &[]).unwrap(),
            Some(AggregateScore::NoTests)
        );
    }
    for verdict in [
        TestVerdict::PresentationError,
        TestVerdict::WallDeadline,
        TestVerdict::SessionLimit,
        TestVerdict::Idle,
        TestVerdict::OutputBudget,
        TestVerdict::Signal,
        TestVerdict::NonzeroExit,
    ] {
        assert_eq!(
            test_score(points(Aggregation::Sum), &verdict, None).unwrap(),
            Some(score("0"))
        );
    }
    for verdict in [
        TestVerdict::CheckerFailed,
        TestVerdict::Unchecked,
        TestVerdict::Cancelled,
        TestVerdict::Infrastructure,
        TestVerdict::NotRun {
            reason: SkipReason::AdmissionClosed,
        },
        TestVerdict::InternalError {
            invariant: Invariant::TestCheck,
        },
    ] {
        assert_eq!(
            test_score(points(Aggregation::Sum), &verdict, None).unwrap(),
            None
        );
    }
    assert_eq!(aggregate_score(Scoring::PassFail, &[None]).unwrap(), None);
    assert!(
        test_score(
            points(Aggregation::Sum),
            &TestVerdict::Scored,
            Some(score("101"))
        )
        .is_err()
    );
    assert!(aggregate_score(points(Aggregation::Sum), &[Some(score("101"))]).is_err());
    for invalid in [
        "-0",
        "01",
        "1.0",
        "1.",
        "1e3",
        "1.0000000001",
        "100000000000000",
        "+1",
    ] {
        assert!(Score::parse(invalid).is_none(), "{invalid}");
    }
}

#[test]
fn judge_cf_display_and_judged_exits() {
    let verdict = TestVerdict::PresentationError;
    let subject = LabelSubject::Test {
        verdict: &verdict,
        class: None,
        check: None,
    };
    let profiles = [DisplayProfile::Cpf, DisplayProfile::JudgeCf];
    let labels: Vec<_> = profiles
        .into_iter()
        .map(|profile| {
            render_label(&label_for_profile(
                subject,
                &DisplayContext::default(),
                profile,
            ))
        })
        .collect();
    assert_eq!(
        serde_json::to_vec(&labels).unwrap(),
        include_bytes!("wire/fixtures/judge_cf_display.json")
    );
    for (verdict, job, token) in [
        (TestVerdict::Scored, JobVerdict::Scored, "SCORE"),
        (TestVerdict::SessionLimit, JobVerdict::SessionLimit, "TLE"),
        (TestVerdict::Idle, JobVerdict::Idle, "IDLE"),
        (
            TestVerdict::PresentationError,
            JobVerdict::PresentationError,
            "PE",
        ),
    ] {
        let subject = LabelSubject::Test {
            verdict: &verdict,
            class: None,
            check: None,
        };
        for subject in [subject, LabelSubject::Job(job)] {
            assert_eq!(
                label(subject, &DisplayContext::default())
                    .token
                    .map(LabelToken::wire_token),
                Some(token)
            );
        }
    }
    for (verdict, code) in [
        (JobVerdict::PresentationError, 1),
        (JobVerdict::Scored, 1),
        (JobVerdict::CheckerFailed, 5),
        (JobVerdict::SessionLimit, 1),
        (JobVerdict::Idle, 1),
    ] {
        assert_eq!(
            crate::jobs::engine::pre_delivery_exit(JobKind::Test, verdict, None, None).code,
            code
        );
    }
    let full = AggregateScore::Points {
        aggregation: Aggregation::Sum,
        value: score("100"),
        max: score("100"),
        full: true,
    };
    assert_eq!(
        crate::jobs::engine::pre_delivery_exit(
            JobKind::Test,
            JobVerdict::Scored,
            None,
            Some(&full)
        )
        .code,
        0
    );
}

#[test]
fn j_b51_all_passed_points_job_is_scored() {
    let tests = [TestVerdict::Passed];
    let input = JobInput {
        roles: &[],
        kind: JobKind::Test,
        scoring: points(Aggregation::Sum),
        build: None,
        primary: &[],
        tests: &tests,
        selected_tests: 1,
        internal_before_freeze: None,
        internal_after_freeze: None,
    };
    assert_eq!(job_verdict(&input).verdict, JobVerdict::Scored);
    assert_eq!(
        job_verdict(&JobInput {
            scoring: Scoring::PassFail,
            ..input
        })
        .verdict,
        JobVerdict::Passed
    );
}
#[test]
fn j_b51_job_verdicts_and_statuses() {
    let status = |tests: &[TestVerdict], scores: &[Option<Score>], aggregation| {
        let scoring = points(aggregation);
        let verdict = job_verdict(&JobInput {
            roles: &[],
            kind: JobKind::Test,
            scoring,
            build: None,
            primary: &[],
            tests,
            selected_tests: tests.len() as u32,
            internal_before_freeze: None,
            internal_after_freeze: None,
        })
        .verdict;
        let score = aggregate_score(scoring, scores).unwrap();
        crate::jobs::engine::pre_delivery_exit(JobKind::Test, verdict, None, score.as_ref()).code
    };
    let partial = [Some(score("12.5")), Some(score("100")), Some(score("0"))];
    let partial_tests = [
        TestVerdict::Scored,
        TestVerdict::Passed,
        TestVerdict::WrongAnswer,
    ];
    for aggregation in [Aggregation::Sum, Aggregation::Min] {
        assert_eq!(status(&partial_tests, &partial, aggregation), 1);
        assert_eq!(
            status(
                &[
                    TestVerdict::Passed,
                    TestVerdict::Passed,
                    TestVerdict::Passed
                ],
                &[Some(score("100")); 3],
                aggregation
            ),
            0
        );
        assert_eq!(
            status(&[TestVerdict::Scored], &[Some(score("12.5"))], aggregation),
            1
        );
        assert_eq!(
            status(
                &[
                    TestVerdict::Scored,
                    TestVerdict::Passed,
                    TestVerdict::CheckerFailed
                ],
                &[Some(score("12.5")), Some(score("100")), None],
                aggregation
            ),
            5
        );
    }
}
