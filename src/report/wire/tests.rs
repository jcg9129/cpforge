use super::*;
use crate::{jobs::engine as e, report as r, supervisor as p};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

pub(super) fn identity() -> e::JobIdentity {
    e::JobIdentity {
        id: crate::storage::job::JobId::parse("20260101000000000-0123456789abcdef").unwrap(),
        dir: PathBuf::from(".cpforge/runs/20260101000000000-0123456789abcdef"),
        sequence: (
            crate::storage::job::Epoch::parse("0123456789abcdef0123456789abcdef").unwrap(),
            1,
        ),
        kind: r::JobKind::Test,
    }
}
pub(super) fn small_summary() -> e::RunSummary {
    let now = Instant::now();
    e::RunSummary {
        observation: e::SummaryEvidence::default(),
        class: r::RunClass::ExitedZero,
        record: Arc::new(e::ExecutionRecord::Started(Box::new(p::RootOutcome {
            #[cfg(all(
                feature = "storage-test-hooks",
                any(target_os = "linux", target_os = "macos")
            ))]
            milestones: None,
            kill_attempts: 0,
            stdout: None,
            stderr: None,
            status: p::Termination::Exited { code: 0 },
            stop_trigger: None,
            enforced_at: None,
            termination: p::TerminationEvidence {
                requested_at: None,
                method: None,
                cleanup: p::Cleanup::Confirmed,
            },
            violations: vec![],
            measurements: vec![],
            limits: vec![],
            timing: p::RootTiming {
                setup: Duration::from_micros(1),
                create_end: now,
                exit_observed: Some(now),
                observer: p::ObserverKind::Polling,
                wall: Some(Duration::from_micros(2)),
                method: p::WallMethod::Observed,
                grace: Duration::from_millis(50),
            },
            notes: vec![],
            errors: vec![],
        }))),
        deadline: Duration::from_micros(3),
        notes: vec![],
        cwd_overflow_bytes: None,
    }
}
pub(super) fn passing() -> e::ReportData {
    let summary = Arc::new(small_summary());
    let execution = e::ExecutionReport {
        plan: e::wire_fixture_plan(0, e::Role::Test { test: "a".into() }),
        run: Some(e::wire_fixture_run(0)),
        program: Some(PathBuf::from("program")),
        spec: Some(e::ArtifactRef {
            digest: Some([0; 32]),
            path: Some("r00000/spec.json".into()),
            error: None,
        }),
        record: Some(e::ArtifactRef {
            digest: None,
            path: Some("r00000/record.json".into()),
            error: None,
        }),
        summary: Some(summary.clone()),
        result: r::ExecutionResult {
            coverage: r::Coverage::Started,
            class: Some(r::RunClass::ExitedZero),
            invariant: None,
        },
        evidence: Some(summary.record.clone()),
    };
    let test = e::TestReport {
        frozen: crate::tests_store::FrozenTest {
            id: crate::names::Identifier::new("a").unwrap(),
            origin: crate::tests_store::Origin::Sample,
            input: crate::storage::inputs::FrozenFile {
                path: "input".into(),
                len: 0,
                sha256: [0; 32],
            },
            answer: crate::tests_store::FrozenAnswer::Present {
                path: "answer".into(),
                len: 0,
                sha256: [0; 32],
            },
            settings: Default::default(),
        },
        first_execution: 0,
        execution_count: 1,
        class: Some(r::RunClass::ExitedZero),
        check: Some(r::CheckOutcome {
            kind: r::CheckKind::Passed,
            diagnostic: false,
        }),
        verdict: Some(r::TestVerdict::Passed),
    };
    let counts = r::Counts {
        primary: r::Cov {
            planned: 1,
            started: 1,
            ..Default::default()
        },
        tests: 1,
        by_verdict: r::ByVerdict {
            passed: 1,
            ..Default::default()
        },
        by_class: r::ByClass {
            exited_zero: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    e::ReportData {
        details: e::ReportDetails {
            problem: Some(Problem {
                identity: "local:a".into(),
                name: "a".into(),
                kind: "batch".into(),
                dir: "problems/a".into(),
            }),
            target: None,
            fingerprints: None,
            fact: Some(Fact {
                outcome: FactOutcome::Published,
                warnings: vec![],
            }),
            hooks: vec![],
            hooks_dropped: 0,
            warnings: vec![],
            warnings_dropped: 0,
            pre_delivery_exit: PreDeliveryExit {
                code: 0,
                reraise: None,
            },
            internal_message: String::new(),
        },
        schema: 1,
        document: "job_report",
        platform: "unix",
        outcome: "finished",
        cpf_version: "0.1.0",
        command: r::JobKind::Test,
        job: identity(),
        workspace: "workspace".into(),
        problem: Some(crate::exec::ProblemBinding {
            dir: "problems/a".into(),
            id: "a".into(),
        }),
        target: None,
        comparator: Some(crate::config::Comparator::Tokens),
        capture: None,
        build: None,
        executions: vec![execution],
        tests: vec![test],
        aggregates: vec![],
        result: e::ReportResult {
            verdict: r::JobVerdict::Passed,
            primary_verdict: r::JobVerdict::Passed,
            partial: false,
            cancellation: None,
            internal: None,
            counts,
        },
        warnings: vec![],
        events: e::EventSummary { dropped: 0 },
        timing: e::Timing {
            started_at: UNIX_EPOCH,
            finished_at: UNIX_EPOCH + Duration::from_micros(10),
            duration: Duration::from_micros(10),
        },
    }
}
pub(super) fn refusal() -> Refusal {
    Refusal {
        platform: "unix",
        cpf_version: "0.1.0".into(),
        command: r::JobKind::Test,
        job: None,
        workspace: None,
        problem: None,
        class: FailureClass::Config,
        error: DocumentError {
            kind: "invalid_config",
            message: "invalid configuration".into(),
            path: None,
        },
        diagnostics: vec![],
        diagnostics_dropped: 0,
        warnings: vec![],
        warnings_dropped: 0,
        events_dropped: 0,
        timing: e::Timing {
            started_at: UNIX_EPOCH,
            finished_at: UNIX_EPOCH,
            duration: Duration::ZERO,
        },
        cancellation: None,
        pre_delivery_exit: PreDeliveryExit {
            code: 7,
            reraise: None,
        },
    }
}

#[test]
fn native_and_summary_contract_vectors() {
    for (value, json) in [
        ("a/é", "{\"utf8\":\"a/é\"}"),
        ("a\n", "{\"bytes_b64\":\"YQo=\"}"),
        ("", "{\"utf8\":\"\"}"),
    ] {
        assert_eq!(encode_native(OsStr::new(value)), json.as_bytes());
        assert_eq!(
            decode_native(json.as_bytes()).unwrap(),
            OsString::from(value)
        );
    }
    assert_eq!(encode_bytes(b"a"), br#"{"bytes_b64":"YQ=="}"#);
    for (value, head, len, truncated) in [
        ("abcdefgh", "abcd", 8, true),
        ("abé", "abé", 4, false),
        ("abcé", "abc", 5, true),
    ] {
        let actual = encode_native_summary(OsStr::new(value), 4);
        let expected = obj([
            ("head", native::ns(head.as_bytes())),
            ("len", u(len)),
            ("truncated", b(truncated)),
            ("sha256", hex(&Sha256::digest(value.as_bytes()))),
        ]);
        assert_eq!(actual, encode(&expected));
    }
    for bad in [
        br#"{"bytes_b64":"Yf9="}"#.as_slice(),
        br#"{"bytes_b64":"YQ=="}"#,
        br#"{"utf8":"a","utf8":"b"}"#,
        br#"{"utf8":"a\n"}"#,
        br#"{}"#,
        br#"{"utf8":1}"#,
        br#"{"utf8":"a","bytes_b64":"YQ=="}"#,
    ] {
        assert!(decode_native(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        decode_native(br#"{"ignored":true,"utf8":"a"}"#).unwrap(),
        OsString::from("a")
    );
    assert_eq!(base64(&[0xed, 0xa0, 0x80]), "7aCA");
}
#[cfg(unix)]
#[test]
fn unix_native_round_trip() {
    use std::os::unix::ffi::OsStringExt;
    let native = OsString::from_vec(vec![b'a', 0xff]);
    assert_eq!(encode_native(&native), br#"{"bytes_b64":"Yf8="}"#);
    assert_eq!(decode_native(&encode_native(&native)).unwrap(), native);
}
#[cfg(windows)]
#[test]
fn windows_wtf8_round_trip() {
    use std::os::windows::ffi::OsStringExt;
    for units in [&[0xd800][..], &[0xd83d, 0xde00, 0xdc00][..]] {
        let native = OsString::from_wide(units);
        assert_eq!(decode_native(&encode_native(&native)).unwrap(), native);
    }
    assert_eq!(
        encode_native(&OsString::from_wide(&[0xd800])),
        br#"{"bytes_b64":"7aCA"}"#
    );
    assert!(decode_native(br#"{"bytes_b64":"7aC97biA"}"#).is_err());
}
#[test]
fn decimal_strings_and_reader_acceptance() {
    for value in [0, u64::MAX] {
        let bytes = encode(&u(value));
        assert_eq!(
            read::read_u64(&read::read_json(&bytes).unwrap()).unwrap(),
            value
        );
    }
    for value in [0, -1, i64::MIN, i64::MAX] {
        let bytes = encode(&s(value.to_string()));
        assert_eq!(
            read::read_i64(&read::read_json(&bytes).unwrap()).unwrap(),
            value
        );
    }
    assert_eq!(encode(&u(u64::MAX)), br#""18446744073709551615""#);
    for bad in [
        "\"01\"",
        "\"-0\"",
        "\"+1\"",
        "\"-1\"",
        "\"\"",
        "\"18446744073709551616\"",
        "0",
        "1.0",
        "1e0",
        "null",
    ] {
        let parsed = serde_json::from_str(bad).unwrap();
        assert!(read::read_u64(&parsed).is_err(), "{bad}");
    }
    for bad in [
        "\"01\"",
        "\"-0\"",
        "\"+1\"",
        "\"-01\"",
        "\"9223372036854775808\"",
        "0",
    ] {
        assert!(read::read_i64(&serde_json::from_str(bad).unwrap()).is_err());
    }
    for bad in [
        b"{\"a\":1,\"a\":2}".as_slice(),
        b"{\"outer\":{\"x\":1,\"x\":2}}",
        b"1.0",
        b"1e0",
        b"-0",
        &[0xff],
    ] {
        assert!(read::read_json(bad).is_err());
    }
    for bad in ["1.0", "1e0", "-1", "4294967296", "\"1\""] {
        assert!(read::read_u32(&serde_json::from_str(bad).unwrap()).is_err());
    }
    let exited = run::status(p::Termination::Exited { code: 0xc0000005 }).unwrap();
    assert_eq!(
        encode(&exited),
        br#"{"kind":"exited","code":3221225477,"ntstatus":"status_access_violation"}"#
    );
}
#[test]
fn canonical_base64_vectors() {
    for (bytes, expected) in [
        (b"".as_slice(), ""),
        (b"f", "Zg=="),
        (b"fo", "Zm8="),
        (b"foo", "Zm9v"),
        (b"foob", "Zm9vYg=="),
        (b"fooba", "Zm9vYmE="),
        (b"foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(bytes), expected);
        assert_eq!(decode_base64(expected).unwrap(), bytes);
    }
    for bad in [
        "Yf9=", "YQ", "YQ=", "YQ===", "YQ==\n", "YQ__", "YQ==YQ==", "YR==", "====", "YQ=A",
    ] {
        assert!(decode_base64(bad).is_err(), "{bad}");
    }
    for len in 0..300 {
        let bytes = (0..len).map(|v| (v * 73) as u8).collect::<Vec<_>>();
        assert_eq!(decode_base64(&base64(&bytes)).unwrap(), bytes);
    }
}
#[test]
fn canonical_escaping_text_and_prefixes() {
    assert_eq!(
        encode(&s("\0\u{8}\u{c}\u{1f}\n\r\t\"\\/é\u{7f}")),
        b"\"\\u0000\\u0008\\u000c\\u001f\\n\\r\\t\\\"\\\\/\xc3\xa9\x7f\""
    );
    // Nothing outside U+0000-U+001F, `"` and `\` is escaped, U+2028/U+2029 included.
    assert_eq!(
        encode(&s("\u{2028}\u{2029}\u{fffd}")),
        b"\"\xe2\x80\xa8\xe2\x80\xa9\xef\xbf\xbd\""
    );
    for input in [
        "\"".repeat(1000),
        "\0".repeat(1000),
        "é".repeat(1000),
        "x".repeat(258),
        "x".repeat(259),
    ] {
        let text = value::text(&input);
        let encoded = encode(&text);
        assert!(encoded.len() <= 260);
        let parsed: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        if input.len() > 258 {
            assert!(parsed.as_str().unwrap().ends_with('…'));
        }
    }
    for schema in [1, 2, u32::MAX] {
        let prefix =
            format!("{{\"schema\":{schema},\"document\":\"job_report\",\"platform\":\"unix\"}}");
        assert_eq!(
            read::report_schema_prefix(prefix.as_bytes()).unwrap(),
            schema
        );
        assert_eq!(
            read::report_schema_prefix(&prefix.as_bytes()[..32]).unwrap(),
            schema
        );
    }
    for prefix in [
        "{\"schema\":01,\"document\":\"job_report\"}",
        "{\"schema\":1.0,\"document\":\"job_report\"}",
        "{\"schema\":1,\"document\":\"error\"}",
        "{\"schema\":1,\"d",
        "{\"schema\":1,",
    ] {
        assert!(read::report_schema_prefix(prefix.as_bytes()).is_err());
    }
}
#[test]
fn report_refusal_error_golden_bytes() {
    let report = passing();
    let actual = encode_report_data(&report).unwrap();
    assert_eq!(actual, include_bytes!("fixtures/passing.json"));
    assert!(actual.starts_with(b"{\"schema\":1,\"document\":\"job_report\",\"platform\":"));
    let stdout = encode_stdout_report_data(
        &report,
        &ReportPersistence::Failed {
            error: e::PersistError::Timeout,
            message: "timeout".into(),
        },
    )
    .unwrap();
    assert_eq!(actual.last(), Some(&b'}'));
    let mut expected = actual[..actual.len() - 1].to_vec();
    let suffix=b",\"persistence\":{\"report\":{\"kind\":\"failed\",\"error\":{\"kind\":\"timeout\"}},\"durability\":null}}";
    expected.extend_from_slice(suffix);
    assert_eq!(stdout, expected);
    assert_eq!(
        encode_refusal(&refusal()).unwrap(),
        include_bytes!("fixtures/refusal.json")
    );
    let error = ErrorDocument {
        platform: "unix",
        command: "config_show".into(),
        class: FailureClass::Config,
        exit: 7,
        error: DocumentError {
            kind: "invalid_config",
            message: "invalid configuration".into(),
            path: None,
        },
        diagnostics: vec![],
        diagnostics_dropped: 0,
    };
    assert_eq!(
        encode_error(&error).unwrap(),
        include_bytes!("fixtures/error.json")
    );
    assert_eq!(
        encode_final_refusal(&refusal()).unwrap(),
        include_bytes!("fixtures/final_refusal.json")
    );
}
#[test]
fn every_event_envelope_golden_bytes() {
    let id = identity();
    let run = e::wire_fixture_run(0);
    let summary = Arc::new(small_summary());
    let events = [
        e::EventPayload::JobStarted {
            job: id.id.clone(),
            sequence: id.sequence.clone(),
            kind: r::JobKind::Test,
        },
        e::EventPayload::RunStarted {
            run: run.clone(),
            execution: 0,
            stage: e::Stage::Run,
            role: e::Role::Test { test: "a".into() },
            digest: Some([0; 32]),
        },
        e::EventPayload::OutputChunk {
            run: run.clone(),
            stream: p::Stream::Stdout,
            bytes: vec![b'a', 0xff],
        },
        e::EventPayload::RunFinished { run, summary },
        e::EventPayload::Progress { done: 1, total: 1 },
    ];
    let fixtures: [&[u8]; 5] = [
        include_bytes!("fixtures/job_started.json"),
        include_bytes!("fixtures/run_started.json"),
        include_bytes!("fixtures/output_chunk.json"),
        include_bytes!("fixtures/run_finished.json"),
        include_bytes!("fixtures/progress.json"),
    ];
    for (seq, (payload, golden)) in events.into_iter().zip(fixtures).enumerate() {
        assert_eq!(
            encode_event(&e::Event {
                seq: seq as u64,
                payload
            })
            .unwrap(),
            golden
        );
    }
    assert_eq!(
        encode_event_header("unix", r::JobKind::Test, "0.1.0", true).unwrap(),
        include_bytes!("fixtures/header.json")
    );
    assert_eq!(
        job_finished(5, &id.id, r::JobVerdict::Passed, None, 0).unwrap(),
        include_bytes!("fixtures/job_finished.json")
    );
}
#[test]
fn capped_lists_and_required_nullable_members() {
    let mut report = passing();
    report.details.hooks = (0..35)
        .map(|_| Hook {
            name: "hook".into(),
            trigger: "after_test",
            execution: None,
            outcome: HookResult::NotPlanned { reason: "disabled" },
        })
        .collect();
    report.details.hooks_dropped = 2;
    report.details.warnings = (0..27)
        .map(|_| Warning {
            kind: WarningKind::ProgressDropped { lines: 1 },
            message: String::new(),
        })
        .collect();
    report.details.warnings_dropped = 4;
    let value = read::read_json(&encode_report_data(&report).unwrap()).unwrap();
    assert_eq!(value["hooks"].as_array().unwrap().len(), 32);
    assert_eq!(value["hooks_dropped"], 5);
    assert_eq!(value["warnings"].as_array().unwrap().len(), 24);
    assert_eq!(value["warnings_dropped"], 7);
    report.executions[0].summary = None;
    report.executions[0].result.invariant = Some(r::Invariant::SummaryUnderivable);
    let bytes = encode_report_data(&report).unwrap();
    let v = read::read_json(&bytes).unwrap();
    assert!(v["executions"][0]["summary"].is_null());
    assert_eq!(v["executions"][0]["class"]["kind"], "exited_zero");
    report.executions[0].result.class = None;
    let v = read::read_json(&encode_report_data(&report).unwrap()).unwrap();
    assert!(v["executions"][0]["class"].is_null());
    let mut refusal = refusal();
    refusal.diagnostics = vec![
        Diagnostic {
            file: None,
            key: None,
            line: None,
            col: None,
            message: String::new()
        };
        67
    ];
    let v = read::read_json(&encode_refusal(&refusal).unwrap()).unwrap();
    assert_eq!(v["refusal"]["diagnostics_dropped"], 3);
    assert_eq!(v["refusal"]["diagnostics"].as_array().unwrap().len(), 64);
}
#[test]
fn spec_record_manifest_and_global_config_mapping() {
    let policy = p::StreamPolicy {
        sink: p::Sink::Discard,
        budget: u64::MAX,
        on_exceed: p::Exceed::Kill,
        events: false,
    };
    let spec = Spec {
        platform: "unix",
        stage: e::Stage::Run,
        program: "a\n".into(),
        args: vec!["é".into()],
        cwd: "cwd".into(),
        env: vec![("X".into(), "value".into())],
        stdin: SpecStdin::Null,
        stdout: policy.clone(),
        stderr: policy,
        wall_deadline: Duration::from_micros(1),
        limits: vec![],
        measure: vec![],
    };
    let encoded = encode_spec(&spec).unwrap();
    assert!(encoded.starts_with(b"{\"schema\":1,\"document\":"));
    let value = read::read_json(&encoded).unwrap();
    assert_eq!(value["program"]["bytes_b64"], "YQo=");
    assert_eq!(value["stdout"]["budget"], u64::MAX.to_string());
    let summary = small_summary();
    let reference = e::ArtifactRef {
        digest: Some([0; 32]),
        path: None,
        error: Some(e::PersistError::Timeout),
    };
    let encoded = encode_record(&Record {
        platform: "unix",
        spec: &reference,
        execution: &summary.record,
    })
    .unwrap();
    assert!(encoded.starts_with(b"{\"schema\":1,\"document\":"));
    read::read_json(&encoded).unwrap();
    let id = identity();
    let encoded = encode_capture_manifest(
        "unix",
        &id.id,
        &crate::jobs::CaptureCheck::Checked,
        &["a/é".into()],
        &[],
        &[0; 32],
    )
    .unwrap();
    let value = read::read_json(&encoded).unwrap();
    assert_eq!(value["sources"][0]["utf8"], "a/é");
    let encoded = encode_config_show(
        "unix",
        &ConfigValue::Table(vec![
            ("z".into(), ConfigValue::Integer(-1)),
            (
                "a".into(),
                ConfigValue::Array(vec![
                    ConfigValue::Float(-0.0),
                    ConfigValue::Boolean(true),
                    ConfigValue::Native("a\n".into()),
                ]),
            ),
        ]),
    )
    .unwrap();
    assert_eq!(encoded,br#"{"schema":1,"document":"config_show","platform":"unix","config":{"z":"-1","a":[{"text":"-0e0","bits":"8000000000000000"},true,{"bytes_b64":"YQo="}]}}"#);
}

#[test]
fn diagnostics_are_embedded_verbatim_and_event_chunks_preserve_bytes() {
    let outcome = crate::compare::check(
        &crate::config::Comparator::Tokens,
        &[0xff; 65],
        Some(&[0xfe; 65]),
        true,
    );
    let crate::compare::CheckOutcome::Failed(diag) = outcome else {
        panic!("mismatch");
    };
    let expected = serde_json::to_vec(&diag).unwrap();
    let wire = super::schema::check(&r::CheckKind::Failed { diag }).unwrap();
    let mut golden =
        b"{\"kind\":\"failed\",\"fail\":\"wrong_answer\",\"reason\":\"token_mismatch\",\"diag\":"
            .to_vec();
    golden.extend(expected);
    golden.push(b'}');
    assert_eq!(encode(&wire), golden);
    let mut original = Vec::new();
    for (seq, bytes) in [vec![0xc3], vec![0xa9]].into_iter().enumerate() {
        let event = e::Event {
            seq: seq as u64,
            payload: e::EventPayload::OutputChunk {
                run: e::wire_fixture_run(0),
                stream: p::Stream::Stdout,
                bytes,
            },
        };
        let encoded = encode_event(&event).unwrap();
        let value = read::read_json(&encoded).unwrap();
        original.extend(decode_base64(value["bytes"]["bytes_b64"].as_str().unwrap()).unwrap());
    }
    assert_eq!(original, "é".as_bytes());
}

#[test]
fn missing_evidence_and_out_of_domain_values_fail_encoding() {
    let mut report = passing();
    report.schema = 2;
    assert!(encode_report_data(&report).is_err());
    let event = e::Event {
        seq: 0,
        payload: e::EventPayload::RunStarted {
            run: e::wire_fixture_run(0),
            execution: 0,
            stage: e::Stage::Run,
            role: e::Role::Raw,
            digest: None,
        },
    };
    assert!(encode_event(&event).is_err());
    let event = e::Event {
        seq: 0,
        payload: e::EventPayload::OutputChunk {
            run: e::wire_fixture_run(0),
            stream: p::Stream::Stdout,
            bytes: vec![0; 4097],
        },
    };
    assert!(encode_event(&event).is_err());
    assert!(report_bound(10_001).is_err());
    assert!(value::code(-2_147_483_649).is_err());
    assert!(value::code(4_294_967_296).is_err());
    for error in [
        e::PersistError::Timeout,
        e::PersistError::QueueFull,
        e::PersistError::Cancelled,
        e::PersistError::WorkerUnavailable,
        e::PersistError::Io {
            os_error: Some(i32::MIN),
        },
    ] {
        let reference = e::ArtifactRef {
            digest: Some([0xff; 32]),
            path: None,
            error: Some(error),
        };
        let v = run::reference(&reference).unwrap();
        assert!(encode(&v).len() <= 451);
    }
    let both = e::ArtifactRef {
        digest: None,
        path: Some("r00000/spec.json".into()),
        error: Some(e::PersistError::Timeout),
    };
    assert!(run::reference(&both).is_err());
    for (raw, wire) in [
        ("setrlimit RLIMIT_AS", "\"setrlimit_rlimit_as\""),
        ("killpg(SIGKILL)", "\"killpg_sigkill\""),
        ("wait4 ru_maxrss", "\"wait4_ru_maxrss\""),
        ("unsupported", "\"unsupported\""),
    ] {
        assert_eq!(
            encode(&run::supervisor_token(raw).unwrap()),
            wire.as_bytes()
        );
    }
    assert!(run::supervisor_token("(+)").is_err());
    let mut report = passing();
    report.details.problem.as_mut().unwrap().identity = "local:b".into();
    assert!(encode_report_data(&report).is_err());
}

#[test]
fn spec_and_record_size_limits_are_enforced() {
    let policy = p::StreamPolicy {
        sink: p::Sink::Discard,
        budget: u64::MAX,
        on_exceed: p::Exceed::Kill,
        events: false,
    };
    let mut spec = Spec {
        platform: "unix",
        stage: e::Stage::Run,
        program: "program".into(),
        args: vec!["\"".repeat(100_000).into()],
        cwd: "cwd".into(),
        env: vec![],
        stdin: SpecStdin::Null,
        stdout: policy.clone(),
        stderr: policy,
        wall_deadline: Duration::from_micros(u64::MAX),
        limits: vec![],
        measure: vec![],
    };
    let bytes = encode_spec(&spec).unwrap();
    assert!(bytes.len() > 200_000 && bytes.len() <= 256 * 1024);
    spec.args = vec!["\"".repeat(140_000).into()];
    assert!(encode_spec(&spec).is_err());
    let mut summary = small_summary();
    let e::ExecutionRecord::Started(root) = Arc::make_mut(&mut summary.record) else {
        unreachable!()
    };
    let capture = p::StreamCapture {
        observed: u64::MAX,
        retained: vec![],
        stored: u64::MAX,
        forwarded: u64::MAX,
        eof: false,
        exceeded_at: Some(u64::MAX),
        losses: vec![
            p::CaptureLoss::BudgetDiscard,
            p::CaptureLoss::WriteError(i32::MIN),
            p::CaptureLoss::WriteStalled,
            p::CaptureLoss::NoEofBeforeDrainDeadline,
            p::CaptureLoss::PipeRead(i32::MIN),
            p::CaptureLoss::ForwardIncomplete,
        ],
        sink: None,
    };
    root.stdout = Some(capture.clone());
    root.stderr = Some(capture);
    for stream in [&mut root.stdout, &mut root.stderr].into_iter().flatten() {
        stream.sink = Some("\"".repeat(2038).into());
    }
    let reference = e::ArtifactRef {
        digest: Some([0xff; 32]),
        path: Some("r00000/spec.json".into()),
        error: None,
    };
    let record = Record {
        platform: "unix",
        spec: &reference,
        execution: &summary.record,
    };
    let bytes = encode_record(&record).unwrap();
    assert!(bytes.len() > 8192 && bytes.len() <= 64 * 1024);
    let mut oversized = summary.record.as_ref().clone();
    let e::ExecutionRecord::Started(root) = &mut oversized else {
        unreachable!()
    };
    root.stdout.as_mut().unwrap().sink = Some("\"".repeat(2043).into());
    assert!(
        encode_record(&Record {
            execution: &oversized,
            ..record
        })
        .is_err()
    );
}

#[test]
fn uncaptured_started_stream_is_incomplete_even_after_eof() {
    let absent = run::stream(None, None).unwrap();
    let absent = read::read_json(&encode(&absent)).unwrap();
    assert_eq!(absent["complete"], false);
    assert!(absent["file"].is_null());
    let discarded = p::StreamCapture {
        observed: 123,
        stored: 0,
        forwarded: 0,
        retained: Vec::new(),
        eof: true,
        exceeded_at: None,
        losses: Vec::new(),
        sink: None,
    };
    let value = run::stream(Some(&discarded), None).unwrap();
    let value = read::read_json(&encode(&value)).unwrap();
    assert_eq!(value["observed"], "123");
    assert_eq!(value["eof"], true);
    assert_eq!(value["complete"], false);
    assert!(value["file"].is_null());
}

#[test]
fn darwin_cleanup_failures_use_contract_reason_tokens() {
    for (reason, token) in [
        (p::UnverifiedReason::Io, "io"),
        (p::UnverifiedReason::InfoFailed, "info_failed"),
        (p::UnverifiedReason::Truncated, "truncated"),
    ] {
        assert_eq!(
            encode(&run::cleanup(p::Cleanup::Unverified { reason })),
            format!(r#"{{"kind":"unverified","reason":"{token}"}}"#).as_bytes(),
        );
    }
}

#[test]
fn transferred_creation_record_and_summary_golden_bytes() {
    let execution = e::ExecutionRecord::Transferred;
    let reference = e::ArtifactRef {
        digest: Some([0; 32]),
        path: Some("r00000/spec.json".into()),
        error: None,
    };
    let bytes = encode_record(&Record {
        platform: "unix",
        spec: &reference,
        execution: &execution,
    })
    .unwrap();
    assert_eq!(bytes, include_bytes!("fixtures/transferred_record.json"));
    let input = r::RunInput::transferred();
    let summary = e::RunSummary {
        observation: e::SummaryEvidence {
            capture_files: [None, None],
            not_started_timing: Some((Duration::from_micros(1), "polling")),
        },
        class: r::classify_run(&input),
        record: Arc::new(execution),
        deadline: Duration::from_micros(3),
        notes: r::run_notes(&input),
        cwd_overflow_bytes: None,
    };
    assert_eq!(
        encode_run_summary(&summary).unwrap(),
        include_bytes!("fixtures/transferred_summary.json")
    );
    let result = r::ExecutionResult {
        coverage: r::Coverage::Transferred,
        class: Some(summary.class.clone()),
        invariant: None,
    };
    assert_eq!(r::execution_invariant(&result), None);
    let counts = r::coverage_counts(&[result], &[], &[]);
    assert!(counts.partial);
    assert_eq!(counts.primary.planned, 1);
    assert_eq!(counts.primary.transferred, 1);
    assert_eq!(counts.primary.started, 0);
    assert_eq!(counts.primary.not_started, 0);
    assert_eq!(counts.primary.admitted, 0);
    assert_eq!(counts.primary.invalid, 0);
    assert_eq!(counts.by_class.cancelled, 1);
}

#[test]
fn persistence_errors_match_golden_decoder_and_report_lane_schema() {
    let errors = [
        e::PersistError::Timeout,
        e::PersistError::QueueFull,
        e::PersistError::Cancelled,
        e::PersistError::WorkerUnavailable,
        e::PersistError::Io { os_error: None },
    ];
    let actual = value::encode(&Value::Array(
        errors
            .iter()
            .map(|error| run::persist(error, "publication failed"))
            .collect(),
    ));
    assert_eq!(actual, include_bytes!("fixtures/persist_errors.json"));
    let decoded = read::read_json(&actual).unwrap();
    assert_eq!(decoded[2], serde_json::json!({"kind": "cancelled"}));
    for error in [e::PersistError::QueueFull, e::PersistError::Cancelled] {
        assert!(
            schema::persistence(&ReportPersistence::Failed {
                error,
                message: String::new()
            })
            .is_err()
        );
    }
}
