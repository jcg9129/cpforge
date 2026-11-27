use super::*;
use crate::{jobs::engine as e, report as r};

fn preview(bytes: &[u8]) -> r::JudgeText {
    r::JudgeText::new(bytes, 128)
}
fn built(header: r::Header) -> r::RoleReport {
    r::RoleReport {
        role: r::RoleName::Checker,
        profile: Some(r::JudgeProfile::Testlib),
        origin: r::RoleOrigin::Built {
            language: "cpp".into(),
            entry: Some(preview(b"checker.cpp")),
        },
        build: Some(r::RoleBuild {
            execution: 0,
            build_id: Some("r00000".into()),
            output_root: Some("builds/r00000".into()),
            result: r::RoleBuildResult::Succeeded,
            artifacts: vec![preview(b"program")],
            artifacts_dropped: 0,
        }),
        capture: r::RoleCapture {
            checked: true,
            sources: vec![preview(b"checker.cpp")],
            sources_dropped: 0,
            support_files: vec![preview(b"testlib.h")],
            support_files_dropped: 0,
            counts: [1, 1],
            bytes: 42,
            lists_sha256: [0; 32],
            content_sha256: [1; 32],
        },
        provenance: r::RoleProvenance::Built { header },
    }
}
fn candidate(pinned: bool) -> r::Header {
    r::Header::Candidate {
        path: preview(b"testlib.h"),
        sha256: [2; 32],
        pinned,
    }
}
fn round_trip(v: &r::RoleReport) -> Vec<u8> {
    let bytes = encode(&roles::role(v).unwrap());
    let decoded = read::read_role(&read::read_json(&bytes).unwrap()).unwrap();
    assert_eq!(*v, decoded);
    assert_eq!(bytes, encode(&roles::role(&decoded).unwrap()));
    bytes
}
#[test]
fn j_b47_header_provenance_evidence_round_trips() {
    let mut values = vec![built(candidate(true)), built(candidate(false))];
    for reason in [
        r::HeaderReason::Ambiguous,
        r::HeaderReason::NotBesideSources,
        r::HeaderReason::None,
        r::HeaderReason::AngleInclude,
    ] {
        values.push(built(r::Header::NotIdentified { reason }));
    }
    assert_eq!(values[0].header_warning(), None);
    assert_eq!(
        values[1].header_warning(),
        Some(r::TestlibHeaderDetail::NotPinned)
    );
    for v in &values[2..] {
        assert_eq!(
            v.header_warning(),
            Some(r::TestlibHeaderDetail::NotIdentified)
        );
    }
    for v in &values {
        round_trip(v);
    }
    assert_eq!(
        encode(&checked_array(&values, 6, roles::role).unwrap()),
        include_bytes!("fixtures/role_provenance.json")
    );
}
#[test]
fn j_b50_prebuilt_provenance_evidence_round_trips() {
    let mut v = built(candidate(true));
    v.origin = r::RoleOrigin::Prebuilt {
        program: preview(b"checker"),
    };
    v.provenance = r::RoleProvenance::Declared;
    v.build = None;
    assert_eq!(v.header_warning(), Some(r::TestlibHeaderDetail::Declared));
    round_trip(&v);
    v.profile = Some(r::JudgeProfile::Simple);
    assert_eq!(v.header_warning(), None);
    round_trip(&v);
}
#[test]
fn simple_and_unprofiled_build_free_roles_round_trip() {
    for role in [
        r::RoleName::Checker,
        r::RoleName::Validator,
        r::RoleName::Interactor,
        r::RoleName::Generator,
        r::RoleName::Brute,
    ] {
        let mut v = built(r::Header::NotApplicable);
        v.role = role;
        v.profile = if matches!(role, r::RoleName::Generator | r::RoleName::Brute) {
            None
        } else {
            Some(r::JudgeProfile::Simple)
        };
        v.build = None;
        round_trip(&v);
    }
}
#[test]
fn role_build_results_round_trip() {
    use r::{RoleBuildCause as C, RoleBuildResult as B};
    let mut v = built(candidate(true));
    let results = vec![
        B::Succeeded,
        B::Cancelled,
        B::Infrastructure {
            causes: r::InfrastructureCauses::new([r::InfrastructureCause::NotStarted {
                reason: r::NotStartedReason::CreateFailed,
            }]),
        },
        B::InternalError {
            invariant: r::Invariant::BuildEvidence,
        },
        B::Failed {
            cause: Box::new(C::MissingOutput {
                names: vec![preview(b"program")],
                names_dropped: 0,
            }),
        },
        B::Failed {
            cause: Box::new(C::GeneratedProgramInvalid {
                name: preview(b"program"),
                reason: "not regular".into(),
            }),
        },
        B::Failed {
            cause: Box::new(C::OutputTooLarge { bytes: 42, cap: 41 }),
        },
        B::Failed {
            cause: Box::new(C::Run(r::RunClass::NonzeroExit { code: 1 })),
        },
        B::Failed {
            cause: Box::new(C::Run(r::RunClass::WallDeadline)),
        },
        B::Failed {
            cause: Box::new(C::Run(r::RunClass::OutputBudget {
                streams: r::BudgetStreams {
                    stdout: true,
                    stderr: false,
                    output_file: false,
                },
            })),
        },
    ];
    for result in results {
        let build = v.build.as_mut().unwrap();
        build.result = result;
        if !matches!(build.result, B::Succeeded) {
            build.artifacts.clear();
        }
        round_trip(&v);
    }
    for reason in [
        r::SkipReason::BuildNotSucceeded,
        r::SkipReason::AdmissionClosed,
        r::SkipReason::SubmitNotSucceeded,
    ] {
        let build = v.build.as_mut().unwrap();
        build.result = B::Skipped { reason };
        build.build_id = None;
        build.output_root = None;
        round_trip(&v);
    }
    // A build skipped for cancellation has no run id and the result
    // `cancelled`, like a cancelled build that started.
    let build = v.build.as_mut().unwrap();
    build.result = B::Cancelled;
    round_trip(&v);
    let build = v.build.as_mut().unwrap();
    build.build_id = Some("r00000".into());
    build.output_root = Some("builds/r00000".into());
    round_trip(&v);
    v.build.as_mut().unwrap().output_root = None;
    assert!(roles::role(&v).is_err());
}
#[test]
fn invalid_role_evidence_is_rejected() {
    let original = built(candidate(true));
    let mut v = original.clone();
    v.profile = Some(r::JudgeProfile::Simple);
    assert!(roles::role(&v).is_err());
    let mut v = original.clone();
    v.capture.counts[0] = 2;
    assert!(roles::role(&v).is_err());
    let mut v = original.clone();
    v.build.as_mut().unwrap().output_root = Some("builds/r00001".into());
    assert!(roles::role(&v).is_err());
    assert!(roles::roles(&[original.clone(), original.clone()], false).is_err());
    let mut validator = original.clone();
    validator.role = r::RoleName::Validator;
    assert!(roles::roles(&[validator.clone(), original.clone()], false).is_err());
    assert!(roles::roles(&[original.clone(), validator], false).is_ok());
    let mut generator = original.clone();
    generator.role = r::RoleName::Generator;
    generator.profile = None;
    generator.provenance = r::RoleProvenance::Built {
        header: r::Header::NotApplicable,
    };
    assert!(roles::roles(std::slice::from_ref(&generator), false).is_err());
    assert!(roles::roles(&[original.clone(), generator], true).is_ok());
    let mut v = original.clone();
    v.build.as_mut().unwrap().result = r::RoleBuildResult::Failed {
        cause: Box::new(r::RoleBuildCause::Run(r::RunClass::ExitedZero)),
    };
    v.build.as_mut().unwrap().artifacts.clear();
    assert!(roles::role(&v).is_err());
    let mut v = original.clone();
    v.build.as_mut().unwrap().result = r::RoleBuildResult::Skipped {
        reason: r::SkipReason::BuildNotSucceeded,
    };
    v.build.as_mut().unwrap().artifacts.clear();
    assert!(roles::role(&v).is_err());
    let bytes = round_trip(&original);
    let json = read::read_json(&bytes).unwrap();
    for key in [
        "role",
        "profile",
        "origin",
        "build",
        "capture",
        "provenance",
    ] {
        let mut bad = json.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(read::read_role(&bad).is_err(), "{key}");
    }
    let mut bad = json.clone();
    bad["provenance"]["header"]["reason"] = "compiler_selected".into();
    bad["provenance"]["header"]["kind"] = "not_identified".into();
    assert!(read::read_role(&bad).is_err());
    let mut report = tests::passing();
    report.details.roles = vec![original];
    let bytes = encode_report_data(&report).unwrap();
    assert_eq!(
        read::read_document_kind(&bytes).unwrap(),
        crate::report::DocumentKind::JobReport
    );
}
/// D1: only a stress report lists generator or brute roles. A test report
/// that encodes with a checker role is refused once either is added.
#[test]
fn non_stress_reports_refuse_generator_and_brute_roles() {
    let mut report = tests::passing();
    report.details.roles = vec![built(r::Header::NotApplicable)];
    report.details.roles[0].profile = Some(r::JudgeProfile::Simple);
    report.details.roles[0].build = None;
    encode_report_data(&report).unwrap();
    for role in [r::RoleName::Generator, r::RoleName::Brute] {
        let mut extra = built(r::Header::NotApplicable);
        extra.role = role;
        extra.profile = None;
        extra.build = None;
        let mut refused = report.clone();
        refused.details.roles.push(extra);
        let error = encode_report_data(&refused).unwrap_err();
        assert_eq!(
            error.to_string(),
            "generator and brute roles belong to stress jobs",
            "{role:?}"
        );
    }
}
#[test]
fn judge_warning_vectors_round_trip_and_fit() {
    let mut values = Vec::new();
    for detail in [
        r::JudgeScratchDetail::StagedCopyKept,
        r::JudgeScratchDetail::TranscriptKept,
    ] {
        values.push(Warning {
            kind: WarningKind::JudgeScratch {
                run: "r99999".into(),
                detail,
            },
            message: "\"".repeat(129),
        });
    }
    for detail in [
        r::TestlibHeaderDetail::NotIdentified,
        r::TestlibHeaderDetail::NotPinned,
        r::TestlibHeaderDetail::Declared,
    ] {
        values.push(Warning {
            kind: WarningKind::TestlibHeader {
                role: r::RoleName::Interactor,
                detail,
            },
            message: "\"".repeat(129),
        });
    }
    let bytes = encode(&checked_array(&values, 5, schema::warning).unwrap());
    assert_eq!(bytes, include_bytes!("fixtures/judging_warnings.json"));
    for role in [r::RoleName::Generator, r::RoleName::Brute] {
        let mut bad = values[2].clone();
        bad.kind = WarningKind::TestlibHeader {
            role,
            detail: r::TestlibHeaderDetail::NotPinned,
        };
        assert!(schema::warning(&bad).is_err());
    }
    for v in read::read_json(&bytes).unwrap().as_array().unwrap() {
        let decoded = read::read_judge_warning(v).unwrap();
        let bytes = encode(&schema::warning(&decoded).unwrap());
        assert!(bytes.len() <= 768);
        assert_eq!(read::read_json(&bytes).unwrap(), *v);
    }
}
pub(super) fn maximal_role(byte: u8) -> r::RoleReport {
    let full = r::JudgeText {
        head: vec![byte; 128],
        len: u64::MAX,
        sha256: [0xff; 32],
    };
    let mut v = built(r::Header::Candidate {
        path: full.clone(),
        sha256: [0xff; 32],
        pinned: false,
    });
    v.role = r::RoleName::Interactor;
    v.origin = r::RoleOrigin::Built {
        language: "x".repeat(64),
        entry: Some(full.clone()),
    };
    v.capture = r::RoleCapture {
        checked: false,
        sources: vec![full.clone(); 16],
        sources_dropped: u32::MAX - 16,
        support_files: vec![full.clone(); 16],
        support_files_dropped: u32::MAX - 16,
        counts: [u32::MAX; 2],
        bytes: u64::MAX,
        lists_sha256: [0xff; 32],
        content_sha256: [0xff; 32],
    };
    v.build = Some(r::RoleBuild {
        execution: u32::MAX,
        build_id: Some("r99999".into()),
        output_root: Some("builds/r99999".into()),
        result: r::RoleBuildResult::Failed {
            cause: Box::new(r::RoleBuildCause::MissingOutput {
                names: vec![full; 16],
                names_dropped: u32::MAX,
            }),
        },
        artifacts: vec![],
        artifacts_dropped: 0,
    });
    v
}
#[test]
fn role_size_vectors_escape_both_full_capture_lists_and_build_together() {
    let values: Vec<_> = [b'"', b'\\', 0].into_iter().map(maximal_role).collect();
    let bytes = encode(&checked_array(&values, 3, roles::role).unwrap());
    assert_eq!(bytes, include_bytes!("fixtures/role_size_vectors.json"));
    for v in &values {
        assert!(round_trip(v).len() <= 21454);
    }
}
#[test]
fn interactive_configuration_vectors_round_trip() {
    let mut values = Vec::new();
    for checking in [
        r::InteractiveChecking::Interactor,
        r::InteractiveChecking::Checker,
        r::InteractiveChecking::InteractorChecker,
    ] {
        for mode in [r::InteractiveMode::Relay, r::InteractiveMode::Direct] {
            values.push(r::CheckConfig {
                kind: r::CheckMode::Interactive,
                interactive: Some(r::InteractiveConfig {
                    checking,
                    mode,
                    idle_us: (mode == r::InteractiveMode::Relay).then_some(u64::MAX),
                    early_reader_close: true,
                    interactor_allowance_us: u64::MAX,
                    peer_allowance_us: u64::MAX,
                    solution_seed: r::SeedMode::Env,
                    interactor_seed: r::SeedMode::Argv,
                }),
                ..r::CheckConfig::default()
            });
        }
    }
    let bytes = encode(&checked_array(&values, 6, judging::config).unwrap());
    assert_eq!(bytes, include_bytes!("fixtures/interactive_configs.json"));
    for (v, json) in values
        .iter()
        .zip(read::read_json(&bytes).unwrap().as_array().unwrap())
    {
        assert_eq!(*v, read::read_check_config(json).unwrap());
        assert!(encode(&judging::config(v).unwrap()).len() <= 630);
        let mut bad = json.clone();
        bad["interactive"]["peer_allowance_us"] = "01".into();
        assert!(read::read_check_config(&bad).is_err());
        let mut bad = json.clone();
        bad["interactive"]
            .as_object_mut()
            .unwrap()
            .remove("idle_us");
        assert!(read::read_check_config(&bad).is_err());
    }
}
#[test]
fn all_report_shapes_use_the_contract_reservation() {
    for validator in [false, true] {
        for judges in 0..=2 {
            for interactive in [false, true] {
                for output_file in [false, true] {
                    let shape = ReportShape {
                        validator,
                        judges,
                        interactive,
                        output_file,
                    };
                    if interactive && output_file {
                        assert!(shape.test_bound().is_err());
                        continue;
                    }
                    let t = 3072
                        + usize::from(validator) * 2560
                        + usize::from(judges) * 11264
                        + usize::from(interactive) * 19456
                        + usize::from(output_file) * 192;
                    for (n, nt, na) in
                        [(0, 0, 0), (1, 1, 0), (1000, 1000, 1000), (10000, 1000, 999)]
                    {
                        let bound = 131072 + 9216 * n + t * nt + 256 * na;
                        assert_eq!(shaped_report_bound(n, nt, na, shape).unwrap(), bound);
                        let reserved = report_reservation(n, nt, na, Some(shape));
                        if 2 * bound > 256 * 1024 * 1024 {
                            assert!(reserved.is_err());
                        } else {
                            assert_eq!(reserved.unwrap(), 2 * bound as u64);
                        }
                    }
                }
            }
        }
    }
    for populations in [(0, 0, 0), (1, 1, 0), (10000, 10000, 10000)] {
        let (n, nt, na) = populations;
        assert_eq!(
            report_reservation(n, nt, na, None),
            report_reservation(n, nt, na, Some(ReportShape::default()))
        );
    }
    assert_eq!(
        report_reservation(10000, 10000, 10000, None).unwrap(),
        251142144
    );
}
#[test]
fn capture_manifest_has_complete_role_file_lists() {
    let id = tests::identity();
    let roles = [r::CaptureRole {
        role: r::RoleName::Checker,
        files: vec!["judge/é.cpp".into(), "judge/testlib.h".into()],
    }];
    let bytes = encode_capture_manifest(
        "unix",
        &id.id,
        &crate::jobs::CaptureCheck::Checked,
        &["solution.cpp".into()],
        &[],
        &[0; 32],
        &roles,
    )
    .unwrap();
    let v = read::read_json(&bytes).unwrap();
    assert_eq!(v["roles"][0]["role"], "checker");
    assert_eq!(v["roles"][0]["files"][0]["utf8"], "judge/é.cpp");
    assert_eq!(v["roles"][0]["files"].as_array().unwrap().len(), 2);
    assert_eq!(
        read::read_document_kind(&bytes).unwrap(),
        r::DocumentKind::CaptureManifest
    );
    let reversed = [
        r::CaptureRole {
            role: r::RoleName::Validator,
            files: vec![],
        },
        roles[0].clone(),
    ];
    assert!(
        encode_capture_manifest(
            "unix",
            &id.id,
            &crate::jobs::CaptureCheck::Checked,
            &["solution.cpp".into()],
            &[],
            &[0; 32],
            &reversed,
        )
        .is_err()
    );
}
#[test]
fn judging_run_events_use_existing_envelopes_and_bounds() {
    let mut values = Vec::new();
    for (stage, role) in [
        (
            e::Stage::Build,
            e::Role::RoleBuild {
                role: r::RoleName::Interactor,
            },
        ),
        (
            e::Stage::Checker,
            e::Role::Checker {
                test: "x".repeat(64),
            },
        ),
        (
            e::Stage::Validator,
            e::Role::Validator {
                test: "x".repeat(64),
            },
        ),
        (
            e::Stage::Interactor,
            e::Role::Interactor {
                test: "x".repeat(64),
            },
        ),
    ] {
        let event = e::Event {
            seq: u64::MAX,
            payload: e::EventPayload::RunStarted {
                run: e::wire_fixture_run(99999),
                execution: u32::MAX,
                stage,
                role,
                digest: Some([0xff; 32]),
            },
        };
        let bytes = encode_event(&event).unwrap();
        assert!(bytes.len() < 400);
        read::validate_run_started(&bytes).unwrap();
        let json = read::read_json(&bytes).unwrap();
        assert_eq!(read::read_stage(&json["stage"]).unwrap(), stage);
        values.push(Value::Verbatim(bytes));
    }
    assert_eq!(
        encode(&Value::Array(values)),
        include_bytes!("fixtures/judging_run_events.json")
    );
    for (stage, role) in [
        (
            e::Stage::Run,
            e::Role::RoleBuild {
                role: r::RoleName::Checker,
            },
        ),
        (e::Stage::Checker, e::Role::Validator { test: "a".into() }),
        (e::Stage::Validator, e::Role::Test { test: "a".into() }),
        (e::Stage::Interactor, e::Role::Build),
    ] {
        let event = e::Event {
            seq: 0,
            payload: e::EventPayload::RunStarted {
                run: e::wire_fixture_run(0),
                execution: 0,
                stage,
                role,
                digest: Some([0; 32]),
            },
        };
        assert!(encode_event(&event).is_err());
    }
    let valid = String::from_utf8(
        encode_event(&e::Event {
            seq: 0,
            payload: e::EventPayload::RunStarted {
                run: e::wire_fixture_run(0),
                execution: 0,
                stage: e::Stage::Checker,
                role: e::Role::Checker { test: "a".into() },
                digest: Some([0; 32]),
            },
        })
        .unwrap(),
    )
    .unwrap();
    let mismatched = valid.replace(r#""stage":"checker""#, r#""stage":"validator""#);
    assert_ne!(valid, mismatched);
    assert!(read::validate_run_started(mismatched.as_bytes()).is_err());
}

#[test]
fn iteration_warning_vectors_and_maximal_identity_fit_the_warning_bound() {
    let mut values = vec![
        Warning {
            kind: WarningKind::Retention {
                detail: "cwd_overflow",
                run: Some("10003.candidate".into()),
                bytes: 4097,
            },
            message: "CwdOverflow: working directory exceeded its byte limit (4097 bytes)".into(),
        },
        Warning {
            kind: WarningKind::JudgeScratch {
                run: "10003.checker".into(),
                detail: r::JudgeScratchDetail::StagedCopyKept,
            },
            message: "judge staged copies could not be removed".into(),
        },
        Warning {
            kind: WarningKind::Persist {
                artefact: "spec",
                run: "10003.generator".into(),
                error: crate::jobs::engine::PersistError::WorkerUnavailable,
            },
            message: "artefact publication failed".into(),
        },
        Warning {
            kind: WarningKind::Persist {
                artefact: "cleanup_log",
                run: "18446744073709551615.candidate_interactor".into(),
                error: crate::jobs::engine::PersistError::Io {
                    os_error: Some(i32::MIN),
                },
            },
            message: "\"".repeat(129),
        },
    ];
    for warning in &values {
        assert!(encode(&schema::warning(warning).unwrap()).len() <= 756);
    }
    assert_eq!(
        encode(&array(&values, |w| schema::warning(w).unwrap())),
        include_bytes!("fixtures/iteration_warnings.json")
    );
    for bad in [
        "01.candidate",
        "18446744073709551616.candidate",
        "1.unknown",
        "1.candidate.extra",
        "r100000",
    ] {
        values[1].kind = WarningKind::JudgeScratch {
            run: bad.into(),
            detail: r::JudgeScratchDetail::StagedCopyKept,
        };
        assert!(schema::warning(&values[1]).is_err());
    }
}
