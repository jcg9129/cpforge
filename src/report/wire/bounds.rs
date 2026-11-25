//! Structural maxima use the field maxima in §3.2, including token slots at
//! 32 bytes. This deliberately over-approximates today's shorter enum tokens.
//! The same ordered writer encodes these and the actual engine fixtures below.
use super::*;
use crate::{jobs::engine as e, report as r, supervisor as p};
use std::path::PathBuf;
use std::time::Duration;

fn tok() -> Value {
    s("x".repeat(32))
}
fn txt() -> Value {
    s("\"".repeat(129))
} // exactly 260 serialized bytes
fn max_native(cap: usize, binary: bool) -> Value {
    let bytes = vec![if binary { 0xff } else { b'"' }; cap];
    obj([
        ("head", native::ns(&bytes)),
        ("len", u(u64::MAX)),
        ("truncated", b(true)),
        ("sha256", hex(&[0xff; 32])),
    ])
}
fn repeated(value: &Value, n: usize) -> Value {
    Value::Array(vec![value.clone(); n])
}
fn max_cause() -> Value {
    obj([
        ("kind", s("capture_loss")),
        ("stream", tok()),
        ("loss", tok()),
    ])
}
fn max_class() -> Value {
    obj([
        ("kind", s("infrastructure")),
        ("causes", repeated(&max_cause(), 8)),
        ("causes_dropped", n(u32::MAX)),
    ])
}
fn max_signal() -> Value {
    obj([
        ("kind", s("signal")),
        ("signal", i(i32::MIN)),
        ("name", tok()),
        ("core_dumped", b(false)),
    ])
}
fn max_status() -> Value {
    obj([
        ("kind", s("exited")),
        ("code", i(i32::MIN)),
        ("ntstatus", tok()),
    ])
}
fn max_trigger() -> Value {
    obj([("kind", s("output_budget")), ("stream", tok())])
}
fn max_violation() -> Value {
    obj([
        ("kind", s("output_budget")),
        ("stream", tok()),
        ("offset", u(u64::MAX)),
    ])
}
fn max_cleanup() -> Value {
    obj([("kind", s("unverified")), ("reason", tok())])
}
fn max_execution() -> Value {
    obj([
        ("kind", s("started")),
        ("status", max_status()),
        ("stop_trigger", max_trigger()),
        ("enforced_at_us", u(u64::MAX)),
        ("violations", repeated(&max_violation(), 3)),
        ("cleanup", max_cleanup()),
        ("kill_attempts", n(u32::MAX)),
    ])
}
fn max_loss() -> Value {
    obj([("kind", s("write_error")), ("os_error", i(i32::MIN))])
}
fn max_stream() -> Value {
    obj([
        ("observed", u(u64::MAX)),
        ("stored", u(u64::MAX)),
        ("forwarded", u(u64::MAX)),
        ("eof", b(false)),
        ("complete", b(false)),
        ("exceeded_at", u(u64::MAX)),
        ("losses", repeated(&max_loss(), 6)),
        ("file", s("r99999/out/stdout")),
    ])
}
fn max_measurement() -> Value {
    obj([
        ("kind", tok()),
        ("value", u(u64::MAX)),
        ("unit", tok()),
        ("scope", tok()),
        ("aggregation", tok()),
        ("completeness", tok()),
        ("method", tok()),
        ("reason", tok()),
    ])
}
fn max_limit() -> Value {
    obj([
        ("kind", tok()),
        ("configured", u(u64::MAX)),
        ("installed", u(u64::MAX)),
        ("mechanism", tok()),
        ("outer", obj([("kind", s("known")), ("bytes", u(u64::MAX))])),
        ("normalised", u(u64::MAX)),
        ("inherited_soft", u(u64::MAX)),
        ("inherited_hard", u(u64::MAX)),
    ])
}
fn max_timing() -> Value {
    obj([
        ("setup_us", u(u64::MAX)),
        ("wall_us", u(u64::MAX)),
        ("observer", tok()),
    ])
}
fn max_summary() -> Value {
    obj([
        ("class", max_class()),
        ("execution", max_execution()),
        ("timing", max_timing()),
        ("deadline_us", u(u64::MAX)),
        ("stdout", max_stream()),
        ("stderr", max_stream()),
        ("measurements", repeated(&max_measurement(), 5)),
        ("limits", repeated(&max_limit(), 2)),
        ("supervisor_errors", repeated(&tok(), 8)),
        ("notes", repeated(&tok(), 16)),
        ("notes_dropped", n(u32::MAX)),
        ("cwd_overflow_bytes", u(u64::MAX)),
    ])
}
fn max_persist() -> Value {
    obj([
        ("kind", s("io")),
        ("os_error", i(i32::MIN)),
        ("message", txt()),
    ])
}
fn max_reference() -> Value {
    obj([
        ("digest", hex(&[0xff; 32])),
        ("path", s("r99999/record.json")),
        ("error", max_persist()),
    ])
}
fn max_role() -> Value {
    obj([
        ("kind", s("hook")),
        ("name", s("x".repeat(64))),
        ("trigger", tok()),
    ])
}
fn max_coverage() -> Value {
    obj([
        ("kind", s("not_started")),
        (
            "reason",
            obj([
                ("kind", s("create_failed")),
                ("api", tok()),
                ("os_error", i(i32::MIN)),
            ]),
        ),
    ])
}
fn max_entry(binary: bool) -> Value {
    obj([
        ("index", n(u32::MAX)),
        ("stage", tok()),
        ("role", max_role()),
        ("coverage", max_coverage()),
        ("run", s("r99999")),
        ("program", max_native(256, binary)),
        ("spec", max_reference()),
        ("record", max_reference()),
        ("summary", max_summary()),
        ("class", Value::Null),
        ("invariant", tok()),
    ])
}
fn max_aggregate() -> Value {
    obj([
        ("test", s("x".repeat(64))),
        ("iterations", n(u32::MAX)),
        ("succeeded", n(u32::MAX)),
        (
            "wall_us",
            obj([
                ("min", u(u64::MAX)),
                ("median", u(u64::MAX)),
                ("max", u(u64::MAX)),
            ]),
        ),
    ])
}
fn max_warning(binary: bool) -> Value {
    obj([
        ("kind", s("recovery")),
        ("paths", n(u32::MAX)),
        ("first", max_native(128, binary)),
        ("message", txt()),
    ])
}
fn max_hook() -> Value {
    obj([
        ("name", s("x".repeat(64))),
        ("trigger", tok()),
        ("execution", n(u32::MAX)),
        (
            "outcome",
            obj([("kind", s("internal_error")), ("invariant", tok())]),
        ),
    ])
}
fn max_capture(binary: bool) -> Value {
    obj([
        (
            "check",
            obj([("kind", s("unchecked")), ("reasons", repeated(&tok(), 4))]),
        ),
        ("sources", repeated(&max_native(128, binary), 16)),
        ("sources_dropped", n(u32::MAX)),
        ("support_files", repeated(&max_native(128, binary), 16)),
        ("support_files_dropped", n(u32::MAX)),
        (
            "counts",
            obj([("sources", n(u32::MAX)), ("support_files", n(u32::MAX))]),
        ),
        ("bytes", u(u64::MAX)),
        ("lists_sha256", hex(&[0xff; 32])),
        ("manifest", s("inputs/capture.json")),
    ])
}
fn max_build(binary: bool) -> Value {
    obj([
        ("execution", n(u32::MAX)),
        ("build_id", s("r99999")),
        ("output_root", s("builds/r99999")),
        (
            "result",
            obj([
                ("kind", s("failed")),
                (
                    "cause",
                    obj([
                        ("kind", s("missing_output")),
                        ("names", repeated(&max_native(128, binary), 16)),
                        ("names_dropped", n(u32::MAX)),
                    ]),
                ),
            ]),
        ),
        ("artifacts", Value::Array(vec![])),
        ("artifacts_dropped", n(u32::MAX)),
    ])
}
fn max_diagnostic(binary: bool) -> Value {
    obj([
        ("file", max_native(256, binary)),
        ("key", txt()),
        ("line", n(u32::MAX)),
        ("col", n(u32::MAX)),
        ("message", txt()),
    ])
}
fn max_test(class: Value, check: Value) -> Value {
    obj([
        ("id", s("x".repeat(64))),
        ("origin", tok()),
        ("answer", tok()),
        ("input_sha256", hex(&[0xff; 32])),
        ("answer_sha256", hex(&[0xff; 32])),
        (
            "executions",
            obj([("first", n(u32::MAX)), ("count", n(u32::MAX))]),
        ),
        ("class", class),
        ("check", check),
        ("diagnostic", b(false)),
        (
            "verdict",
            obj([("kind", s("internal_error")), ("invariant", tok())]),
        ),
    ])
}
fn max_diag_check() -> Value {
    let mut fields = vec![
        ("kind", s("failed")),
        ("fail", s("wrong_answer")),
        ("reason", tok()),
        ("diag", Value::Null),
    ];
    // Replace with the longest valid JSON object of exactly the reserved size;
    // this tests the reservation even if current diagnostics use less of it.
    fields[3].1 = obj([("reserved", s("x".repeat(2033)))]); // 2048 bytes
    assert_eq!(encode(&fields[3].1).len(), 2048);
    Value::Object(fields)
}

#[test]
fn file_io_structural_maxima_and_ten_thousand_entries_fit_the_shape_formula() {
    let config = r::FileIo {
        input: Some(crate::names::PortableComponent::new(format!("{}a", "é".repeat(127))).unwrap()),
        output: Some(
            crate::names::PortableComponent::new(format!("{}b", "é".repeat(127))).unwrap(),
        ),
    };
    let io = super::io::config(&config).unwrap();
    assert_eq!(encode(&io).len() + 6, 562);
    let collected = super::io::output_file(&r::OutputFile::Collected {
        bytes: u64::MAX,
        sha256: [0xff; 32],
        path: "r99999/out/output".into(),
    })
    .unwrap();
    assert!(encode(&collected).len() + 15 <= 169);
    let mut check = max_diag_check();
    replace(
        &mut check,
        "diag",
        obj([("reserved", s(format!("{}x", "\"".repeat(1016))))]),
    );
    let mut test = max_test(max_signal(), check);
    let Value::Object(fields) = &mut test else {
        unreachable!()
    };
    fields.push(("output_file", collected));
    let class = run::class(&r::RunClass::OutputBudget {
        streams: r::BudgetStreams {
            stdout: true,
            stderr: true,
            output_file: true,
        },
    })
    .unwrap();
    assert!(encode(&class).len() <= 204);
    let vector = obj([("io", io.clone()), ("test", test.clone()), ("class", class)]);
    assert_eq!(
        encode(&vector),
        include_bytes!("fixtures/file_io_size_vectors.json")
    );
    let shape = ReportShape {
        output_file: true,
        ..ReportShape::default()
    };
    assert!(encode(&test).len() < shape.test_bound().unwrap());
    for binary in [false, true] {
        let mut header = max_header(binary);
        let Value::Object(fields) = &mut header else {
            unreachable!()
        };
        let target = &mut fields
            .iter_mut()
            .find(|(key, _)| *key == "target")
            .unwrap()
            .1;
        let Value::Object(fields) = target else {
            unreachable!()
        };
        fields.insert(5, ("io", io.clone()));
        let actual = encode(&header).len()
            + 10_000
                * (encode(&max_entry(binary)).len()
                    + encode(&test).len()
                    + encode(&max_aggregate()).len())
            + 3 * 9999;
        assert!(actual <= shaped_report_bound(10_000, 10_000, 10_000, shape).unwrap());
    }
}
fn replace(v: &mut Value, key: &str, value: Value) {
    let Value::Object(fields) = v else {
        panic!("object")
    };
    fields.iter_mut().find(|(name, _)| *name == key).unwrap().1 = value;
}
fn max_header(binary: bool) -> Value {
    let report = tests::passing();
    let mut value = super::report_value(
        &report,
        Some(&ReportPersistence::Failed {
            error: e::PersistError::Io {
                os_error: Some(i32::MIN),
            },
            message: "\"".repeat(129),
        }),
    )
    .unwrap();
    replace(&mut value, "workspace", max_native(1024, binary));
    replace(
        &mut value,
        "problem",
        obj([
            ("identity", s(format!("local:{}", "x".repeat(64)))),
            ("name", s("x".repeat(64))),
            ("kind", txt()),
            ("dir", s("x".repeat(75))),
        ]),
    );
    let mut dec_fields = vec![("kind", s("float"))];
    for key in [
        "abs",
        "rel",
        "rel_base",
        "combine",
        "ulps",
        "int_exact",
        "reserved_a",
        "reserved_b",
    ] {
        dec_fields.push((
            key,
            obj([("text", s("x".repeat(32))), ("bits", s("f".repeat(16)))]),
        ));
    }
    replace(
        &mut value,
        "target",
        obj([
            ("name", s("x".repeat(64))),
            ("language", s("x".repeat(64))),
            ("profile", s("x".repeat(64))),
            ("build_free", b(false)),
            ("compare", Value::Object(dec_fields)),
            (
                "run_deadline",
                obj([("us", u(u64::MAX)), ("source", tok())]),
            ),
            (
                "published_limits",
                obj([("time_ms", n(u32::MAX)), ("memory_bytes", u(u64::MAX))]),
            ),
            ("local_limits", repeated(&max_limit(), 2)),
        ]),
    );
    replace(&mut value, "capture", max_capture(binary));
    replace(
        &mut value,
        "fingerprints",
        obj([("source", hex(&[0xff; 32])), ("tests", hex(&[0xff; 32]))]),
    );
    replace(&mut value, "build", max_build(binary));
    replace(&mut value, "executions", Value::Array(vec![]));
    replace(&mut value, "tests", Value::Array(vec![]));
    replace(&mut value, "aggregates", Value::Array(vec![]));
    let mut result = super::schema::result(&report.result, &report.details).unwrap();
    replace(&mut result, "verdict", tok());
    replace(&mut result, "primary_verdict", tok());
    replace(&mut result, "partial", b(false));
    replace(
        &mut result,
        "cancellation",
        obj([
            ("requested_at_us", u(u64::MAX)),
            ("source", tok()),
            ("phase", tok()),
        ]),
    );
    replace(
        &mut result,
        "pre_delivery_exit",
        obj([("code", i(i32::MIN)), ("reraise", tok())]),
    );
    replace(&mut result, "counts", max_counts());
    replace(
        &mut result,
        "internal",
        obj([
            ("invariant", tok()),
            (
                "locus",
                obj([("kind", s("test")), ("test", s("x".repeat(64)))]),
            ),
            ("message", txt()),
        ]),
    );
    replace(&mut value, "result", result);
    replace(
        &mut value,
        "fact",
        obj([
            (
                "outcome",
                obj([
                    ("kind", s("not_published")),
                    ("reason", tok()),
                    ("message", txt()),
                ]),
            ),
            ("warnings", repeated(&tok(), 2)),
        ]),
    );
    replace(&mut value, "hooks", repeated(&max_hook(), 32));
    replace(&mut value, "hooks_dropped", n(u32::MAX));
    replace(&mut value, "warnings", repeated(&max_warning(binary), 24));
    replace(&mut value, "warnings_dropped", n(u32::MAX));
    replace(&mut value, "cpf_version", s("x".repeat(32)));
    replace(&mut value, "command", tok());
    replace(&mut value, "events", obj([("dropped", u(u64::MAX))]));
    replace(
        &mut value,
        "timing",
        obj([
            ("started_at", s("9999-12-31T23:59:59.999Z")),
            ("finished_at", s("9999-12-31T23:59:59.999Z")),
            ("duration_us", u(u64::MAX)),
        ]),
    );
    value
}
fn max_counts() -> Value {
    fn fill(v: &mut Value) {
        match v {
            Value::Number(v) => *v = i64::from(u32::MAX),
            Value::Object(fields) => {
                for (_, v) in fields {
                    fill(v)
                }
            }
            _ => {}
        }
    }
    let mut value = super::schema::counts(r::Counts::default());
    fill(&mut value);
    value
}
fn row(name: &str, value: &Value, sum: usize, bound: usize) {
    let actual = encode(value).len();
    let oracle = format!("{name}: {actual} <= table {sum} <= bound {bound}");
    assert!(
        include_str!("fixtures/bounds.txt")
            .lines()
            .any(|line| line.ends_with(&oracle)),
        "size differs from the independent oracle: {oracle}"
    );
    eprintln!(
        "{name}: serialized={actual}, table={sum}, bound={bound}, table slack={}",
        sum as i64 - actual as i64
    );
    assert!(actual <= sum, "{name}: {actual} > table sum {sum}");
    assert!(actual <= bound, "{name}: {actual} > bound {bound}");
}
#[test]
fn every_arithmetic_row_in_both_native_forms() {
    for binary in [false, true] {
        row("Cause", &max_cause(), 129, 129);
        row("class", &max_class(), 1122, 1122);
        row("non-infrastructure class", &max_signal(), 169, 169);
        row("execution summary", &max_execution(), 804, 804);
        row("stream", &max_stream(), 608, 608);
        row("Measurement", &max_measurement(), 346, 346);
        row("InstalledLimit", &max_limit(), 360, 360);
        row("RunSummary", &max_summary(), 6774, 7 * 1024);
        row("Ref", &max_reference(), 451, 451);
        row("executions entry", &max_entry(binary), 8851, 9 * 1024);
        row(
            "tests entry with diagnostic",
            &max_test(max_signal(), max_diag_check()),
            2868,
            3 * 1024,
        );
        row(
            "tests entry infrastructure",
            &max_test(max_class(), kind("passed")),
            1723,
            3 * 1024,
        );
        row("aggregate", &max_aggregate(), 224, 256);
        row("warning", &max_warning(binary), 741, 768);
        row("hook", &max_hook(), 244, 244);
        row("capture", &max_capture(binary), 13311, 13311);
        row("build", &max_build(binary), 6664, 6664);
        row("report header", &max_header(binary), 53645, 64 * 1024);
        row("Diagnostic", &max_diagnostic(binary), 1237, 1237);
        let mut refusal = super::refusal_value(&tests::refusal()).unwrap();
        let header = max_header(binary);
        let Value::Object(header) = header else {
            unreachable!()
        };
        for (key, value) in header {
            if matches!(
                key,
                "job"
                    | "workspace"
                    | "problem"
                    | "result"
                    | "warnings"
                    | "warnings_dropped"
                    | "events"
                    | "timing"
                    | "persistence"
            ) {
                replace(&mut refusal, key, value);
            }
        }
        replace(
            &mut refusal,
            "refusal",
            obj([
                ("class", tok()),
                (
                    "error",
                    obj([
                        ("kind", tok()),
                        ("message", txt()),
                        ("path", max_native(256, binary)),
                    ]),
                ),
                ("diagnostics", repeated(&max_diagnostic(binary), 64)),
                ("diagnostics_dropped", n(u32::MAX)),
            ]),
        );
        row("refusal", &refusal, 103966, 104 * 1024);
        let event = obj([
            ("event", s("run_finished")),
            ("seq", u(u64::MAX)),
            ("run", s("r99999")),
            ("summary", max_summary()),
        ]);
        row("run_finished event", &event, 6873, 7 * 1024);
        let event = obj([
            ("event", s("output_chunk")),
            ("seq", u(u64::MAX)),
            ("run", s("r99999")),
            ("stream", tok()),
            ("bytes", native::raw(&[0xff; 4096])),
        ]);
        row("output_chunk event", &event, 5621, 7 * 1024);
    }
}
/// About 110 MB, so it runs once per CI job: in the default-feature pass, and
/// not again under `--all-features`.
#[test]
#[cfg_attr(
    feature = "storage-test-hooks",
    ignore = "large fixture; covered by the default-feature run"
)]
fn ten_thousand_full_structural_entries_fit_reserved_report() {
    // A single buffer holds the final event envelope with the report inside
    // it, so the report bytes are never copied. Each entry goes through the
    // canonical writer once before its bytes are repeated.
    const ENVELOPE: &[u8] = b"{\"event\":\"report\",\"report\":";
    let probe = encode(&obj([("event", s("report")), ("report", Value::Null)]));
    assert_eq!(&probe[..ENVELOPE.len()], ENVELOPE);
    let header = max_header(false);
    let Value::Object(fields) = header else {
        unreachable!()
    };
    let bound = report_bound(10_000).unwrap();
    let mut bytes = Vec::with_capacity(ENVELOPE.len() + bound + 1);
    bytes.extend_from_slice(ENVELOPE);
    let start = bytes.len();
    bytes.push(b'{');
    for (j, (key, value)) in fields.into_iter().enumerate() {
        if j > 0 {
            bytes.push(b',');
        }
        bytes.extend(encode(&s(key)));
        bytes.push(b':');
        let entry = match key {
            "executions" => Some(max_entry(false)),
            "tests" => Some(max_test(max_signal(), max_diag_check())),
            "aggregates" => Some(max_aggregate()),
            _ => None,
        };
        if let Some(entry) = entry {
            let entry = encode(&entry);
            bytes.push(b'[');
            for j in 0..10_000 {
                if j > 0 {
                    bytes.push(b',');
                }
                bytes.extend_from_slice(&entry);
            }
            bytes.push(b']');
        } else {
            bytes.extend(encode(&value));
        }
    }
    bytes.push(b'}');
    let len = bytes.len() - start;
    let table = 53645 + 11946 * 10_000;
    eprintln!("10000 executions: serialized={len}, table={table}, reserved={bound}");
    assert!(len <= table);
    assert!(len <= bound);
    bytes.push(b'}');
    assert_eq!(bytes.len(), len + 28);
    assert_eq!(bytes.len() + 1, len + 29);
}

#[test]
fn actual_engine_values_fill_every_available_cap() {
    let mut summary = tests::small_summary();
    summary.deadline = Duration::from_micros(u64::MAX);
    summary.cwd_overflow_bytes = Some(u64::MAX);
    summary.class = r::RunClass::Infrastructure {
        causes: r::InfrastructureCauses::new([
            r::InfrastructureCause::NotStarted {
                reason: r::NotStartedReason::ContainmentUnavailable,
            },
            r::InfrastructureCause::StatusUnknown,
            r::InfrastructureCause::CleanupIncomplete {
                survivors: u32::MAX,
            },
            r::InfrastructureCause::SupervisorError {
                error: r::SupervisorErrorKind::Register,
            },
            r::InfrastructureCause::SupervisorError {
                error: r::SupervisorErrorKind::Wait,
            },
            r::InfrastructureCause::SupervisorError {
                error: r::SupervisorErrorKind::PipeRead,
            },
            r::InfrastructureCause::SupervisorError {
                error: r::SupervisorErrorKind::Kill,
            },
            r::InfrastructureCause::SupervisorError {
                error: r::SupervisorErrorKind::Reap,
            },
            r::InfrastructureCause::SupervisorError {
                error: r::SupervisorErrorKind::Worker,
            },
        ]),
    };
    let record = std::sync::Arc::make_mut(&mut summary.record);
    let e::ExecutionRecord::Started(root) = record else {
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
    root.status = p::Termination::Exited { code: 0xc0000005 };
    root.stop_trigger = Some(p::StopTrigger::OutputBudget(p::Stream::Stdout));
    root.enforced_at = Some(Duration::from_micros(u64::MAX));
    root.violations = vec![
        p::Violation::WallDeadline {
            observed_by: root.timing.create_end,
        },
        p::Violation::OutputBudget {
            stream: p::Stream::Stdout,
            offset: u64::MAX,
        },
        p::Violation::OutputBudget {
            stream: p::Stream::Stderr,
            offset: u64::MAX,
        },
    ];
    root.termination.cleanup = p::Cleanup::Unverified {
        reason: p::UnverifiedReason::NamespaceUnknown,
    };
    root.timing.setup = Duration::from_micros(u64::MAX);
    root.timing.wall = Some(Duration::from_micros(u64::MAX));
    root.measurements = [
        p::MetricKind::PeakRss,
        p::MetricKind::PeakWorkingSet,
        p::MetricKind::PeakJobCommit,
        p::MetricKind::CpuTime,
        p::MetricKind::ProcessLifetime,
    ]
    .into_iter()
    .map(|kind| p::Measurement {
        kind,
        value: Some(u64::MAX),
        unit: p::Unit::Microseconds,
        scope: p::Scope::RootAndWaitedDescendants,
        aggregation: p::Aggregation::MaxOfProcesses,
        completeness: p::Completeness::Partial,
        method: "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        reason: Some("cleanup_not_confirmed".into()),
    })
    .collect();
    root.limits = [
        p::LocalLimit::AddressSpace { bytes: u64::MAX },
        p::LocalLimit::JobCommit { bytes: u64::MAX },
    ]
    .into_iter()
    .map(|configured| p::InstalledLimit {
        configured,
        inherited: Some((u64::MAX, u64::MAX)),
        normalised: u64::MAX,
        installed: Some(u64::MAX),
        mechanism: "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        outer: p::OuterRestriction::Known(u64::MAX),
    })
    .collect();
    root.errors = vec![
        p::SupervisorError::Register(i32::MIN),
        p::SupervisorError::Wait(i32::MIN),
        p::SupervisorError::Kill(i32::MIN),
        p::SupervisorError::Reap,
        p::SupervisorError::ReapTimeout,
    ];
    root.notes = vec![
        p::Note::ObserverFallback,
        p::Note::Transferred,
        p::Note::DeadlineGrace,
        p::Note::LimitLowered,
        p::Note::LateTrigger(p::StopTrigger::Cancelled),
    ];
    summary.notes = vec![
        r::RunNote::CleanupUnverified,
        r::RunNote::CwdOverflow,
        r::RunNote::TruncatedStdout,
        r::RunNote::TruncatedStderr,
    ];
    summary.observation.capture_files = [
        Some(PathBuf::from("r99999/out/stdout")),
        Some(PathBuf::from("r99999/out/stderr")),
    ];
    let encoded = encode_run_summary(&summary).unwrap();
    assert!(encoded.len() <= 6774);
    let parsed = read::read_json(&encoded).unwrap();
    assert_eq!(parsed["measurements"].as_array().unwrap().len(), 5);
    assert_eq!(parsed["limits"].as_array().unwrap().len(), 2);
    assert_eq!(parsed["stdout"]["losses"].as_array().unwrap().len(), 6);
    // Unmeasured reasons replace a successful overflow and keep schema 1's
    // token and summary bounds, including alongside all other policy notes.
    summary.cwd_overflow_bytes = None;
    for note in [
        r::RunNote::CwdUnmeasuredTimeout,
        r::RunNote::CwdUnmeasuredIo,
        r::RunNote::CwdUnmeasuredCancelled,
    ] {
        summary.notes[1] = note;
        assert!(note.wire_token().len() <= 32);
        assert!(encode_run_summary(&summary).unwrap().len() <= 6774);
    }
    // Both native representations are exercised through the actual build and
    // capture adapters, in addition to the conservative structural maxima.
    for name in ["\"".repeat(512), "\n".repeat(512)] {
        let build = e::BuildReport {
            execution: u32::MAX,
            build_id: None,
            output_root: None,
            result: r::BuildResult::Failed {
                cause: r::BuildCause::MissingOutput {
                    names: vec![name.clone().into(); 20],
                    names_dropped: 3,
                },
            },
            artifacts: vec![],
        };
        let value = super::schema::build(&build, std::path::Path::new(".")).unwrap();
        assert!(encode(&value).len() <= 6664);
        let capture = e::CaptureReport {
            check: crate::jobs::CaptureCheck::Unchecked {
                reasons: vec![crate::jobs::CaptureCheckReason::ShellStage; 4],
            },
            sources: vec![name.clone().into(); 20],
            support_files: vec![name.into(); 20],
            sources_dropped: 0,
            support_files_dropped: 0,
            counts: [20, 20],
            bytes: u64::MAX,
            lists_sha256: [0xff; 32],
            manifest: Some("inputs/capture.json".into()),
        };
        let value = super::schema::capture(&capture).unwrap();
        assert!(encode(&value).len() <= 13311);
    }
}
