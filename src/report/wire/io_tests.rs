use super::*;
use crate::names::PortableComponent;
use crate::{jobs::engine as e, report as r};
use r::{CollectionFailure as F, OutputFile as O, OutputFileType as T};

#[path = "../../../tests/ready_support/mod.rs"]
#[allow(dead_code)] // Shared setup includes helpers for later execution producers.
#[allow(clippy::duplicate_mod)] // The engine tests load the same shared fixture module.
mod ready_support;

fn config(input: Option<&str>, output: Option<&str>) -> r::FileIo {
    r::FileIo {
        input: input.map(|v| PortableComponent::new(v).unwrap()),
        output: output.map(|v| PortableComponent::new(v).unwrap()),
    }
}
fn variants() -> Vec<O> {
    vec![
        O::Collected {
            bytes: u64::MAX,
            sha256: [0xff; 32],
            path: "r99999/out/output".into(),
        },
        O::Missing,
        O::Misnamed,
        O::TooManyEntries,
        O::NotRegular { file_type: T::Link },
        O::NotRegular {
            file_type: T::Directory,
        },
        O::NotRegular {
            file_type: T::Other,
        },
        O::Unreadable { os_error: i32::MIN },
        O::TooLarge {
            bytes: u64::MAX,
            budget: u64::MAX - 1,
        },
        O::NotCollected {
            reason: F::Io,
            os_error: Some(i32::MIN),
        },
        O::NotCollected {
            reason: F::Timeout,
            os_error: None,
        },
        O::NotCollected {
            reason: F::Unstable,
            os_error: None,
        },
        O::NotAttempted,
    ]
}
fn run(class: r::RunClass) -> r::ExecutionResult {
    r::ExecutionResult {
        coverage: r::Coverage::Started,
        class: Some(class),
        invariant: None,
    }
}
fn outcome(
    class: r::RunClass,
    output: &O,
) -> (r::ExecutionResult, r::CheckOutcome, r::TestVerdict) {
    r::file_test_outcome(
        &run(class),
        r::CheckAdmission::NoIntent,
        false,
        &r::Comparison::NotRun,
        output,
    )
}

#[test]
fn every_collection_variant_has_an_encoder_vector_and_typed_round_trip() {
    let values: Vec<_> = variants()
        .iter()
        .map(|v| io::output_file(v).unwrap())
        .collect();
    let bytes = encode(&Value::Array(values));
    assert_eq!(bytes, include_bytes!("fixtures/file_io_variants.json"));
    let decoded = read::read_json(&bytes).unwrap();
    for (original, value) in variants().iter().zip(decoded.as_array().unwrap()) {
        assert_eq!(*original, read::read_output_file(value).unwrap());
        // The largest output_file, including its member name and comma, is 169.
        assert!(encode(&io::output_file(original).unwrap()).len() + 15 <= 169);
    }
}

#[test]
fn io_names_and_stdin_have_encoder_vectors_and_typed_round_trips() {
    let input = format!("{}a", "é".repeat(127));
    let output = format!("{}b", "é".repeat(127));
    let configs = [
        config(Some("in.txt"), None),
        config(None, Some("out.txt")),
        config(Some(&input), Some(&output)),
    ];
    let bytes = encode(&Value::Array(
        configs.iter().map(|v| io::config(v).unwrap()).collect(),
    ));
    assert_eq!(bytes, include_bytes!("fixtures/file_io_configs.json"));
    let decoded = read::read_json(&bytes).unwrap();
    for (original, value) in configs.iter().zip(decoded.as_array().unwrap()) {
        assert_eq!(*original, read::read_file_io(value).unwrap());
    }
    assert_eq!(encode(&io::config(&configs[2]).unwrap()).len() + 6, 562);
    for (input, output) in [
        (None, None),
        (Some("a"), Some("A")),
        (Some("é"), Some("e\u{301}")),
        (Some("ß"), Some("ss")),
    ] {
        assert!(io::config(&config(input, output)).is_err());
    }
}

#[test]
fn output_rejections_and_scratch_details_have_encoder_vectors_and_round_trips() {
    let rejections: Vec<_> = variants()
        .iter()
        .filter_map(O::rejection_reason)
        .map(|reason| r::CheckKind::OutputRejected { reason })
        .collect();
    // NotRegular has three underlying types, but one check reason.
    let mut unique = Vec::new();
    for check in rejections {
        if !unique.contains(&check) {
            unique.push(check);
        }
    }
    let bytes = encode(&Value::Array(
        unique.iter().map(|v| schema::check(v).unwrap()).collect(),
    ));
    assert_eq!(bytes, include_bytes!("fixtures/file_io_checks.json"));
    for (original, value) in unique
        .iter()
        .zip(read::read_json(&bytes).unwrap().as_array().unwrap())
    {
        assert_eq!(*original, read::read_output_rejected(value).unwrap());
    }
    let details = [
        r::IoScratchDetail::StagedInputKept,
        r::IoScratchDetail::OutputKept,
        r::IoScratchDetail::PartialCopyKept,
        r::IoScratchDetail::NotMeasured,
    ];
    let warnings: Vec<_> = details
        .into_iter()
        .map(|detail| Warning {
            kind: WarningKind::IoScratch {
                run: "r99999".into(),
                detail,
            },
            message: "\"".repeat(129),
        })
        .collect();
    let bytes = encode(&Value::Array(
        warnings
            .iter()
            .map(|v| schema::warning(v).unwrap())
            .collect(),
    ));
    assert_eq!(bytes, include_bytes!("fixtures/file_io_warnings.json"));
    for (original, value) in warnings
        .iter()
        .zip(read::read_json(&bytes).unwrap().as_array().unwrap())
    {
        let decoded = read::read_io_scratch(value).unwrap();
        assert_eq!(
            encode(&schema::warning(original).unwrap()),
            encode(&schema::warning(&decoded).unwrap())
        );
        assert!(encode(&schema::warning(original).unwrap()).len() <= 351);
    }
}

macro_rules! rejection_row {
    ($name:ident, $output:expr, $reason:ident) => {
        #[test]
        fn $name() {
            let (_, check, verdict) = outcome(r::RunClass::ExitedZero, &$output);
            assert_eq!(
                check,
                r::CheckOutcome {
                    kind: r::CheckKind::OutputRejected {
                        reason: r::OutputRejectReason::$reason
                    },
                    diagnostic: false
                }
            );
            assert_eq!(verdict, r::TestVerdict::WrongAnswer);
        }
    };
}
rejection_row!(f_06_missing_precedes_absent_answer, O::Missing, Missing);
rejection_row!(f_07_misnamed, O::Misnamed, Misnamed);
rejection_row!(f_10_link, O::NotRegular { file_type: T::Link }, NotRegular);
rejection_row!(
    f_11_directory,
    O::NotRegular {
        file_type: T::Directory
    },
    NotRegular
);
rejection_row!(
    f_11_other,
    O::NotRegular {
        file_type: T::Other
    },
    NotRegular
);
rejection_row!(f_12_unreadable, O::Unreadable { os_error: 13 }, Unreadable);
rejection_row!(f_30_directory_limit, O::TooManyEntries, DirTooLarge);

#[test]
fn f_08_too_large_raises_only_test_class_and_appends_output_stream_last() {
    let output = O::TooLarge {
        bytes: 4097,
        budget: 4096,
    };
    for original in [
        r::RunClass::ExitedZero,
        r::RunClass::NonzeroExit { code: 1 },
        r::RunClass::Signal {
            signal: 9,
            core_dumped: false,
        },
        r::RunClass::OutputBudget {
            streams: r::BudgetStreams {
                stdout: true,
                stderr: true,
                output_file: false,
            },
        },
    ] {
        let execution = run(original.clone());
        let (test, check, verdict) = r::file_test_outcome(
            &execution,
            r::CheckAdmission::NoIntent,
            true,
            &r::Comparison::NotRun,
            &output,
        );
        assert_eq!(execution.class.as_ref(), Some(&original));
        let r::RunClass::OutputBudget { streams } = test.class.unwrap() else {
            panic!("budget class")
        };
        assert!(streams.output_file);
        assert_eq!(streams.streams().last(), Some(r::Stream::OutputFile));
        assert!(check.diagnostic);
        assert_eq!(
            check.kind,
            r::CheckKind::OutputRejected {
                reason: r::OutputRejectReason::TooLarge
            }
        );
        assert_eq!(verdict, r::TestVerdict::OutputBudget);
        let value = run::class(&r::RunClass::OutputBudget { streams }).unwrap();
        assert_eq!(
            read::read_budget_streams(&read::read_json(&encode(&value)).unwrap()).unwrap(),
            streams
        );
    }
    let (_, _, verdict) = outcome(r::RunClass::WallDeadline, &output);
    assert_eq!(verdict, r::TestVerdict::WallDeadline);
}

macro_rules! collection_error_row {
    ($name:ident, $reason:ident, $error:expr) => {
        #[test]
        fn $name() {
            let (_, check, verdict) = outcome(
                r::RunClass::ExitedZero,
                &O::NotCollected {
                    reason: F::$reason,
                    os_error: $error,
                },
            );
            assert_eq!(check.kind, r::CheckKind::CheckError { os_error: $error });
            assert_eq!(verdict, r::TestVerdict::Infrastructure);
        }
    };
}
collection_error_row!(f_13_collection_io, Io, Some(5));
collection_error_row!(f_31_collection_timeout, Timeout, None);
collection_error_row!(f_29_collection_unstable, Unstable, None);

#[test]
fn f_22_admission_cancellation_precedes_collection_failure() {
    for output in variants() {
        let (_, check, verdict) = r::file_test_outcome(
            &run(r::RunClass::ExitedZero),
            r::CheckAdmission::IntentBeforeAdmission,
            false,
            &r::Comparison::NotRun,
            &output,
        );
        assert_eq!(
            check.kind,
            r::CheckKind::NotAdmitted {
                reason: r::CheckNotAdmitted::Cancelled
            }
        );
        assert_eq!(verdict, r::TestVerdict::Cancelled);
    }
}

#[test]
fn file_collection_checks_remain_diagnostic_after_solution_failure() {
    for class in [
        r::RunClass::WallDeadline,
        r::RunClass::NonzeroExit { code: 1 },
        r::RunClass::Signal {
            signal: 9,
            core_dumped: false,
        },
    ] {
        for output in variants()
            .into_iter()
            .filter(|v| !matches!(v, O::Collected { .. } | O::NotAttempted))
        {
            let (test, check, verdict) = outcome(class.clone(), &output);
            assert!(check.diagnostic);
            assert_eq!(verdict.wire_token(), test.class.unwrap().wire_token());
        }
    }
}

#[test]
fn f_14_collected_uses_comparison_and_missing_evidence_stays_internal() {
    let output = O::Collected {
        bytes: 0,
        sha256: [0; 32],
        path: "r00000/out/output".into(),
    };
    let (_, check, verdict) = r::file_test_outcome(
        &run(r::RunClass::ExitedZero),
        r::CheckAdmission::IntentAfterAdmission,
        true,
        &r::Comparison::Outcome(crate::compare::CheckOutcome::Passed),
        &output,
    );
    assert_eq!(check.kind, r::CheckKind::Passed);
    assert_eq!(verdict, r::TestVerdict::Passed);
    let (_, _, verdict) = outcome(r::RunClass::ExitedZero, &O::NotAttempted);
    assert!(matches!(verdict, r::TestVerdict::InternalError { .. }));
    let execution = r::ExecutionResult {
        coverage: r::Coverage::Admitted,
        class: None,
        invariant: Some(r::Invariant::RecordMissing),
    };
    let (_, check, verdict) = r::file_test_outcome(
        &execution,
        r::CheckAdmission::NoIntent,
        true,
        &r::Comparison::NotRun,
        &O::NotAttempted,
    );
    assert!(matches!(check.kind, r::CheckKind::InternalError { .. }));
    assert!(matches!(verdict, r::TestVerdict::InternalError { .. }));
}

#[test]
fn f_06_collected_output_follows_the_existing_answer_and_diagnostic_rows() {
    let output = O::Collected {
        bytes: 2,
        sha256: [0; 32],
        path: "r00000/out/output".into(),
    };
    let (_, check, verdict) = outcome(r::RunClass::ExitedZero, &output);
    assert_eq!(check.kind, r::CheckKind::Unchecked);
    assert!(!check.diagnostic);
    assert_eq!(verdict, r::TestVerdict::Unchecked);
    let (test, check, verdict) = r::file_test_outcome(
        &run(r::RunClass::NonzeroExit { code: 3 }),
        r::CheckAdmission::NoIntent,
        true,
        &r::Comparison::Outcome(crate::compare::CheckOutcome::Passed),
        &output,
    );
    assert_eq!(test.class, Some(r::RunClass::NonzeroExit { code: 3 }));
    assert_eq!(check.kind, r::CheckKind::Passed);
    assert!(check.diagnostic);
    assert_eq!(verdict, r::TestVerdict::NonzeroExit);
}

#[test]
fn ineligible_solutions_never_admit_a_collection_check() {
    for execution in [
        r::ExecutionResult {
            coverage: r::Coverage::Skipped {
                reason: r::SkipReason::BuildNotSucceeded,
            },
            class: None,
            invariant: None,
        },
        r::ExecutionResult {
            coverage: r::Coverage::Transferred,
            class: Some(r::RunClass::Cancelled),
            invariant: None,
        },
        r::ExecutionResult {
            coverage: r::Coverage::NotStarted {
                reason: r::NotStartedReason::Worker,
            },
            class: Some(r::RunClass::Infrastructure {
                causes: r::InfrastructureCauses::new([r::InfrastructureCause::StatusUnknown]),
            }),
            invariant: None,
        },
    ] {
        let (_, check, verdict) = r::file_test_outcome(
            &execution,
            r::CheckAdmission::NoIntent,
            true,
            &r::Comparison::NotRun,
            &O::NotAttempted,
        );
        assert_eq!(
            check.kind,
            r::CheckKind::NotAdmitted {
                reason: r::CheckNotAdmitted::RunNotEligible
            }
        );
        assert!(!check.diagnostic);
        assert!(matches!(
            verdict,
            r::TestVerdict::NotRun { .. }
                | r::TestVerdict::Cancelled
                | r::TestVerdict::Infrastructure
        ));
    }
}

fn file_report() -> e::ReportData {
    let fixture = ready_support::Fixture::new();
    fixture.write("problem/main.py", "pass\n");
    let mut report = tests::passing();
    report.target = Some(fixture.target("python", ""));
    report.details.target = Some(Target {
        name: Some("solution".into()),
        language: Some("python".into()),
        profile: None,
        build_free: true,
        compare: Some(crate::config::Comparator::Tokens),
        check: Some(crate::report::CheckConfig::default()),
        io: Some(config(Some("in.txt"), Some("out.txt"))),
        run_deadline: None,
        published_limits: None,
        local_limits: vec![],
    });
    report.tests[0].output_file = Some(O::Collected {
        bytes: 0,
        sha256: [0; 32],
        path: "r00000/out/output".into(),
    });
    report.details.shape = Some(ReportShape {
        output_file: true,
        ..ReportShape::default()
    });
    report
}

#[test]
fn file_io_report_encoder_vector_and_reader_acceptance_on_both_platforms() {
    let mut report = file_report();
    let bytes = encode_report_data(&report).unwrap();
    assert_eq!(bytes, include_bytes!("fixtures/file_io_report.json"));
    assert_eq!(
        read::read_document_kind(&bytes).unwrap(),
        r::DocumentKind::JobReport
    );
    for platform in ["unix", "windows"] {
        report.platform = platform;
        assert!(read::read_document_kind(&encode_report_data(&report).unwrap()).is_ok());
    }
    report
        .details
        .target
        .as_mut()
        .unwrap()
        .io
        .as_mut()
        .unwrap()
        .output = None;
    report.tests[0].output_file = None;
    report.details.shape = Some(ReportShape::default());
    let bytes = encode_report_data(&report).unwrap();
    assert!(read::read_json(&bytes).unwrap()["tests"][0]["output_file"].is_null());
    assert!(read::read_document_kind(&bytes).is_ok());
    report.command = r::JobKind::Run;
    assert!(encode_report_data(&report).is_err());
}

#[test]
fn malformed_file_io_wire_values_are_rejected() {
    for bad in [
        r#"{"kind":"collected","bytes":"01","sha256":"ff","path":"r00000/out/output"}"#,
        r#"{"kind":"too_large","bytes":"2","budget":"2"}"#,
        r#"{"kind":"not_collected","reason":"timeout","os_error":5}"#,
        r#"{"kind":"not_regular","type":"regular"}"#,
        r#"{"kind":"unreadable","os_error":2147483648}"#,
        r#"{"kind":"not_attempted","kind":"missing"}"#,
    ] {
        assert!(
            read::read_json(bad.as_bytes())
                .and_then(|v| read::read_output_file(&v))
                .is_err(),
            "{bad}"
        );
    }
    for bad in [
        r#"{"input":"in.txt","output":null,"stdin":"test_input"}"#,
        r#"{"input":null,"output":"../out","stdin":"test_input"}"#,
        r#"{"input":"CON","output":null,"stdin":"null_device"}"#,
        r#"{"input":null,"output":null,"stdin":"test_input"}"#,
        r#"{"output":"out.txt","stdin":"test_input"}"#,
    ] {
        assert!(read::read_file_io(&read::read_json(bad.as_bytes()).unwrap()).is_err());
    }
    let mut report = read::read_json(include_bytes!("fixtures/file_io_report.json")).unwrap();
    report["tests"][0]["class"] = serde_json::json!({"kind":"output_budget","stream":"output_file","streams":["output_file"]});
    assert!(read::read_document_kind(&serde_json::to_vec(&report).unwrap()).is_err());
    let mut summary = tests::small_summary();
    summary.class = r::RunClass::OutputBudget {
        streams: r::BudgetStreams {
            stdout: false,
            stderr: false,
            output_file: true,
        },
    };
    assert!(encode_run_summary(&summary).is_err());
    for streams in [
        vec!["output_file", "stdout"],
        vec!["stdout", "stdout"],
        vec![],
    ] {
        assert!(
            read::read_budget_streams(
                &serde_json::json!({"kind":"output_budget","stream":"stdout","streams":streams})
            )
            .is_err()
        );
    }
}

#[test]
fn shape_formula_and_reservation_caps_match_contract_vectors() {
    let mut report = file_report();
    let planned = report.details.shape;
    report.details.shape = Some(ReportShape::default());
    assert!(encode_report_data(&report).is_err());
    // A shape inferred after planning could exceed the legacy reservation.
    report.details.shape = None;
    assert!(encode_report_data(&report).is_err());
    report.details.shape = planned;
    assert!(encode_report_data(&report).is_ok());
    report.details.problem.as_mut().unwrap().kind = "interactive".into();
    assert!(encode_report_data(&report).is_err());
    let shape = ReportShape {
        output_file: true,
        ..ReportShape::default()
    };
    let bound = shaped_report_bound(10_000, 10_000, 10_000, shape).unwrap();
    assert_eq!(bound, 127_491_072);
    assert_eq!(
        report_reservation(10_000, 10_000, 10_000, Some(shape)).unwrap(),
        254_982_144
    );
    let interactive = ReportShape {
        validator: true,
        judges: 2,
        interactive: true,
        output_file: false,
    };
    assert_eq!(interactive.test_bound().unwrap(), 47_616);
    assert_eq!(
        shaped_report_bound(4004, 1000, 0, interactive).unwrap(),
        84_647_936
    );
    assert_eq!(
        report_reservation(9382, 1000, 0, Some(interactive)).unwrap(),
        268_423_168
    );
    assert!(report_reservation(9383, 1000, 0, Some(interactive)).is_err());
    assert!(shaped_report_bound(4004, 1001, 0, interactive).is_err());
    assert!(shaped_report_bound(1, 2, 0, shape).is_err());
    assert!(shaped_report_bound(10_001, 0, 0, shape).is_err());
    assert!(ReportShape { judges: 3, ..shape }.test_bound().is_err());
    assert!(
        ReportShape {
            interactive: true,
            ..shape
        }
        .test_bound()
        .is_err()
    );
    for planned in [0, 1, 10_000] {
        assert_eq!(
            report_reservation(planned, 0, 0, None).unwrap(),
            2 * (131072 + 9216 * planned) as u64
        );
    }
}

#[test]
fn file_io_labels_keep_wa_and_diagnostic_class_labels() {
    let (_, check, verdict) = outcome(r::RunClass::ExitedZero, &O::Missing);
    let context = r::DisplayContext {
        output_file: Some(("out.txt".into(), O::Missing)),
        ..Default::default()
    };
    let label = r::label(
        r::LabelSubject::Test {
            verdict: &verdict,
            class: Some(&r::RunClass::ExitedZero),
            check: Some(&check),
        },
        &context,
    );
    assert_eq!(r::render_label(&label), "WA  no output file out.txt");
    let (execution, check, verdict) = outcome(r::RunClass::NonzeroExit { code: 1 }, &O::Missing);
    let label = r::label(
        r::LabelSubject::Test {
            verdict: &verdict,
            class: execution.class.as_ref(),
            check: Some(&check),
        },
        &context,
    );
    assert_eq!(r::render_label(&label), "RE  exit code 1  (no output file)");
    let output = O::TooLarge {
        bytes: 4097,
        budget: 4096,
    };
    let (execution, check, verdict) = outcome(r::RunClass::ExitedZero, &output);
    let context = r::DisplayContext {
        output_file: Some(("out.txt".into(), output)),
        ..Default::default()
    };
    let label = r::label(
        r::LabelSubject::Test {
            verdict: &verdict,
            class: execution.class.as_ref(),
            check: Some(&check),
        },
        &context,
    );
    assert_eq!(
        r::render_label(&label),
        "OLE  output file out.txt: more than 4096 bytes"
    );
    let summary = r::summary(
        r::SummaryTemplate::Passed {
            passed: 1,
            selected: 1,
            comparator: "tokens",
        },
        true,
        false,
    )
    .with_file_io();
    assert_eq!(
        summary.render(),
        "1/1 passed (tokens comparison) (file I/O)"
    );
    for (verdict, exit) in [
        (r::JobVerdict::WrongAnswer, 1),
        (r::JobVerdict::OutputBudget, 1),
        (r::JobVerdict::Infrastructure, 10),
    ] {
        assert_eq!(
            e::pre_delivery_exit(r::JobKind::Test, verdict, None, None).code,
            exit
        );
    }
}
