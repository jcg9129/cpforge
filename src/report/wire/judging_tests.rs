use super::*;
use crate::report as r;

fn judge(result: Option<r::JudgeResult>) -> r::Judge {
    let read = !matches!(
        result,
        None | Some(
            r::JudgeResult::Unverified
                | r::JudgeResult::NotRead { .. }
                | r::JudgeResult::JudgeFailed {
                    class: r::JudgeFailClass::Crashed,
                    ..
                }
        )
    );
    r::Judge {
        role: r::JudgeRole::Checker,
        execution: 1,
        profile: r::JudgeProfile::Testlib,
        message: read.then(|| r::JudgeText::new(b"message", 4096)),
        stderr: result
            .as_ref()
            .map(|_| r::JudgeText::new(b"diagnostic", 1024)),
        report: read.then_some(r::JudgeReport {
            path: Some("r00001/judge/report".into()),
            bytes: 64,
            encoding: Some(r::JudgeEncoding::Utf8),
        }),
        result,
    }
}
#[test]
fn all_batch_evidence_encoder_vectors_round_trip() {
    use r::{JudgeFail as F, JudgeFailClass as C, JudgeReason as R, JudgeResult as J};
    let score = r::Score::parse("12.5").unwrap();
    let scale = r::Score::parse("100").unwrap();
    let results = [
        Some(J::Passed),
        Some(J::Rejected {
            fail: F::WrongAnswer,
        }),
        Some(J::Rejected {
            fail: F::PresentationError,
        }),
        Some(J::JudgeFailed {
            class: C::Crashed,
            reason: R::Signal,
        }),
        Some(J::Scored { raw: score, scale }),
        Some(J::Unverified),
        Some(J::NotRead {
            reason: r::JudgeNotRead::Stopped,
        }),
        Some(J::NotRead {
            reason: r::JudgeNotRead::CleanupIncomplete,
        }),
        Some(J::NotRead {
            reason: r::JudgeNotRead::ExtractionFailed,
        }),
        None,
    ];
    let judges: Vec<_> = results.into_iter().map(judge).collect();
    let bytes = encode(&array(&judges, |v| judging::judge(v).unwrap()));
    assert_eq!(bytes, include_bytes!("fixtures/batch_judges.json"));
    for (original, value) in judges
        .iter()
        .zip(read::read_json(&bytes).unwrap().as_array().unwrap())
    {
        assert_eq!(*original, read::read_judge(value).unwrap());
        assert_eq!(
            encode(&judging::judge(original).unwrap()),
            encode(&judging::judge(&read::read_judge(value).unwrap()).unwrap())
        );
    }
    let validations: Vec<_> = [
        r::ValidationOutcome::Valid,
        r::ValidationOutcome::Invalid,
        r::ValidationOutcome::ValidationFailed,
        r::ValidationOutcome::ValidatorFailed,
        r::ValidationOutcome::NotRun,
    ]
    .into_iter()
    .map(|outcome| r::Validation {
        execution: 0,
        profile: r::JudgeProfile::Simple,
        outcome,
        stderr: None,
    })
    .collect();
    let bytes = encode(&array(&validations, |v| judging::validation(v).unwrap()));
    assert_eq!(bytes, include_bytes!("fixtures/batch_validation.json"));
    for (original, value) in validations
        .iter()
        .zip(read::read_json(&bytes).unwrap().as_array().unwrap())
    {
        assert_eq!(*original, read::read_validation(value).unwrap());
    }
    let scores = [
        r::AggregateScore::Points {
            aggregation: r::Aggregation::Sum,
            value: score,
            max: scale,
            full: false,
        },
        r::AggregateScore::Incomplete {
            aggregation: r::Aggregation::Min,
            missing: 1,
        },
        r::AggregateScore::NoTests,
    ];
    let bytes = encode(&judging::serialized(&scores).unwrap());
    assert_eq!(bytes, include_bytes!("fixtures/batch_scores.json"));
    for (original, value) in scores
        .iter()
        .zip(read::read_json(&bytes).unwrap().as_array().unwrap())
    {
        assert_eq!(*original, read::read_aggregate_score(value).unwrap());
    }
}
#[test]
fn judged_report_encoder_vector_and_reader() {
    let mut report = tests::passing();
    report.details.shape = Some(ReportShape {
        judges: 1,
        ..ReportShape::default()
    });
    report.tests[0].judges = vec![judge(Some(r::JudgeResult::Scored {
        raw: r::Score::parse("12.5").unwrap(),
        scale: r::Score::parse("100").unwrap(),
    }))];
    report.tests[0].check = Some(r::CheckOutcome {
        kind: r::CheckKind::Judged {
            outcome: Box::new(r::JudgedCheck::Scored {
                raw: r::Score::parse("12.5").unwrap(),
                scale: r::Score::parse("100").unwrap(),
                role: r::JudgeRole::Checker,
            }),
        },
        diagnostic: false,
    });
    report.tests[0].score = r::Score::parse("12.5");
    report.tests[0].verdict = Some(r::TestVerdict::Scored);
    report.result.verdict = r::JobVerdict::Scored;
    report.result.primary_verdict = r::JobVerdict::Scored;
    report.result.counts.by_verdict.passed = 0;
    report.result.counts.by_verdict.scored = 1;
    report.details.pre_delivery_exit.code = 1;
    report.details.score = Some(r::AggregateScore::Points {
        aggregation: r::Aggregation::Sum,
        value: r::Score::parse("12.5").unwrap(),
        max: r::Score::parse("100").unwrap(),
        full: false,
    });
    let bytes = encode_report_data(&report).unwrap();
    assert_eq!(bytes, include_bytes!("fixtures/batch_scored_report.json"));
    assert_eq!(
        read::read_document_kind(&bytes).unwrap(),
        r::DocumentKind::JobReport
    );
    let parsed = read::read_json(&bytes).unwrap();
    assert_eq!(
        read::read_judged_check(&parsed["tests"][0]["check"]).unwrap(),
        match &report.tests[0].check.as_ref().unwrap().kind {
            r::CheckKind::Judged { outcome } => *outcome.clone(),
            _ => unreachable!(),
        }
    );
}
/// A capped head of `byte` whose full value is maximally long (`len` at
/// `u64::MAX`), so every member of the summary is at its serialized maximum.
fn maximal_text(byte: u8, cap: usize) -> r::JudgeText {
    r::JudgeText {
        head: vec![byte; cap],
        len: u64::MAX,
        sha256: [0xff; 32],
    }
}
#[test]
fn maximum_judge_message_and_diagnostic_fit_shape_and_reservation() {
    let mut vectors = Vec::new();
    for byte in [b'"', b'\\', 0] {
        let mut evidence = judge(Some(r::JudgeResult::JudgeFailed {
            class: r::JudgeFailClass::ProtocolUnsupported,
            reason: r::JudgeReason::PeerAllowanceExpired,
        }));
        evidence.role = r::JudgeRole::Interactor;
        evidence.execution = u32::MAX;
        evidence.report = Some(r::JudgeReport {
            path: Some("r99999/judge/report".into()),
            bytes: 65_536,
            encoding: Some(r::JudgeEncoding::Windows1251),
        });
        evidence.message = Some(maximal_text(byte, 4096));
        evidence.stderr = Some(maximal_text(byte, 1024));
        let value = judging::judge(&evidence).unwrap();
        let encoded = encode(&value);
        // Table sums of report §3.7.1, below the 11 KiB and 2.5 KiB bounds.
        assert!(encoded.len() <= 10_928, "{}", encoded.len());
        assert_eq!(
            read::read_judge(&read::read_json(&encoded).unwrap()).unwrap(),
            evidence
        );
        let validation = r::Validation {
            execution: u32::MAX,
            profile: r::JudgeProfile::Testlib,
            outcome: r::ValidationOutcome::ValidationFailed,
            stderr: Some(maximal_text(byte, 1024)),
        };
        let validation_value = judging::validation(&validation).unwrap();
        assert!(encode(&validation_value).len() <= 2_321);
        assert_eq!(
            read::read_validation(&read::read_json(&encode(&validation_value)).unwrap()).unwrap(),
            validation
        );
        vectors.push(value);
        vectors.push(validation_value);
        let mut report = tests::passing();
        report.tests[0].judges = vec![evidence];
        report.tests[0].validation = Some(validation);
        let shape = ReportShape {
            validator: true,
            judges: 1,
            ..ReportShape::default()
        };
        assert!(
            encode(&schema::test(&report.tests[0]).unwrap()).len() <= shape.test_bound().unwrap()
        );
        let bound = shaped_report_bound(3003, 1000, 0, shape).unwrap();
        assert_eq!(
            report_reservation(3003, 1000, 0, Some(shape)).unwrap(),
            2 * bound as u64
        );
        assert!(shaped_report_bound(3006, 1001, 0, shape).is_err());
    }
    assert_eq!(
        encode(&Value::Array(vectors)),
        include_bytes!("fixtures/batch_size_vectors.json")
    );
}
#[test]
fn invalid_judging_members_are_rejected() {
    for value in [
        serde_json::json!({"kind":"scored","raw":"01","scale":"100"}),
        serde_json::json!({"kind":"scored","raw":"101","scale":"100"}),
        serde_json::json!({"kind":"judge_failed","class":"wrong_answer","reason":"signal"}),
    ] {
        assert!(read::read_judge_result(&value).is_err());
    }
    assert!(read::read_aggregate_score(&serde_json::json!({"kind":"points","aggregation":"sum","value":"1","max":"2","full":true})).is_err());
    let mut value =
        read::read_json(include_bytes!("fixtures/batch_judges.json")).unwrap()[0].clone();
    value["message"]["truncated"] = true.into();
    assert!(read::read_judge(&value).is_err());
    let judges = read::read_json(include_bytes!("fixtures/batch_judges.json")).unwrap();
    // An eligible head in base64, and a hash that does not cover a whole value.
    let mut value = judges[0].clone();
    value["message"]["head"] = serde_json::json!({"bytes_b64": "bWVzc2FnZQ=="});
    assert!(read::read_judge(&value).is_err());
    let mut value = judges[0].clone();
    value["stderr"]["sha256"] = "f".repeat(64).into();
    assert!(read::read_judge(&value).is_err());
    // A failed extraction never names an extracted copy.
    let mut value = judges[8].clone();
    value["report"] = serde_json::json!({"path":"r00001/judge/report","bytes":"0","encoding":null});
    assert!(read::read_judge(&value).is_err());
    value["report"]["path"] = serde_json::Value::Null;
    assert!(read::read_judge(&value).is_ok());
    let mut report = read::read_json(include_bytes!("fixtures/passing.json")).unwrap();
    report["tests"][0]["check"] = serde_json::json!({"kind":"unchecked","reason":"not_verified"});
    assert!(read::read_document_kind(&serde_json::to_vec(&report).unwrap()).is_err());
    report["tests"][0]["check"] =
        serde_json::json!({"kind":"unchecked","reason":"checking_unverified"});
    assert!(read::read_document_kind(&serde_json::to_vec(&report).unwrap()).is_ok());
    report["tests"][0].as_object_mut().unwrap().remove("judges");
    assert!(read::read_document_kind(&serde_json::to_vec(&report).unwrap()).is_err());
}

#[test]
fn batch_check_configs_round_trip() {
    let mut configs = vec![r::CheckConfig::default()];
    for aggregation in [r::Aggregation::Sum, r::Aggregation::Min] {
        configs.push(r::CheckConfig {
            kind: r::CheckMode::Checker,
            answer: r::AnswerPolicy::None,
            scoring: r::Scoring::Points {
                scale: r::Score::parse("100").unwrap(),
                aggregation,
            },
            display: r::DisplayProfile::JudgeCf,
            interactive: None,
        });
    }
    let bytes = encode(&array(&configs, |v| judging::config(v).unwrap()));
    assert_eq!(bytes, include_bytes!("fixtures/batch_check_configs.json"));
    for (original, value) in configs
        .iter()
        .zip(read::read_json(&bytes).unwrap().as_array().unwrap())
    {
        assert_eq!(*original, read::read_check_config(value).unwrap());
    }
}

#[test]
fn judged_check_variants_encoder_vectors_and_round_trips() {
    use r::{CheckingUnchecked as U, JudgeFail as F, JudgeRole as R, JudgedCheck as C};
    let checks = [
        C::Rejected {
            fail: F::WrongAnswer,
            role: R::Checker,
        },
        C::Rejected {
            fail: F::PresentationError,
            role: R::Interactor,
        },
        C::Scored {
            raw: r::Score::parse("12.5").unwrap(),
            scale: r::Score::parse("100").unwrap(),
            role: R::Checker,
        },
        C::JudgeFailed {
            class: r::JudgeFailClass::TestDataError,
            role: R::Validator,
            reason: r::JudgeReason::InvalidInput,
        },
        C::Unchecked {
            reason: U::CheckingUnverified,
        },
        C::Unchecked {
            reason: U::CheckingInterrupted,
        },
    ];
    let bytes = encode(&array(&checks, |v| {
        schema::check(&r::CheckKind::Judged {
            outcome: Box::new(v.clone()),
        })
        .unwrap()
    }));
    assert_eq!(bytes, include_bytes!("fixtures/batch_checks.json"));
    for (original, value) in checks
        .iter()
        .zip(read::read_json(&bytes).unwrap().as_array().unwrap())
    {
        assert_eq!(*original, read::read_judged_check(value).unwrap());
    }
}

#[test]
fn supplied_judge_fingerprint_validates_wire_form_without_recomputing() {
    let mut value: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/passing.json")).unwrap();
    value["fingerprints"] =
        serde_json::json!({"source": "0".repeat(64), "tests": "1".repeat(64), "judge": null});
    for valid in [
        serde_json::Value::Null,
        serde_json::Value::String("a".repeat(64)),
        serde_json::Value::String("0".repeat(64)),
    ] {
        value["fingerprints"]["judge"] = valid;
        judging::validate_members(&value).unwrap();
    }
    for invalid in [
        serde_json::json!("A".repeat(64)),
        serde_json::json!("g".repeat(64)),
        serde_json::json!("a".repeat(63)),
        serde_json::json!("a".repeat(65)),
        serde_json::json!(1),
    ] {
        value["fingerprints"]["judge"] = invalid;
        assert!(judging::validate_members(&value).is_err());
    }
}
