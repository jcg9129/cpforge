//! Real-header protocol coverage on both supervised Unix backends.
use super::{PROBE, ready_support::Fixture, run_test};
use cpforge::config::{Comparator, ConfigRequest, Environment, Platform, load_config};
use cpforge::jobs::engine::{
    Engine, ExecutionRecord, Executor, JobRequest, MemoryPersistence, NoHooks, Role,
    SupervisorExecutor,
};
use cpforge::jobs::{ReadyJob, prepare_job};
use cpforge::report::{self as r, JudgeFailClass as C, JudgeReason as R, JudgeResult, TestVerdict};
use cpforge::storage::prune::PrunePolicy;
use std::fs;

const HEADER: &[u8] = include_bytes!("../fixtures/testlib/testlib.h");

fn ready(
    f: &Fixture,
    body: &str,
    extra: &str,
    validator: Option<&str>,
    modified: bool,
) -> Box<ReadyJob<'static>> {
    let compiler = if cfg!(target_os = "macos") {
        "g++-14"
    } else {
        "g++"
    };
    f.write("cpforge.toml", &format!("schema = 1\n[build]\ndeadline_s = 60\n[languages.cpp.build]\nargv = ['{compiler}', '-std=c++17', '-O0', '{{sources}}', '-o', '{{artifact}}']\n"));
    f.write("problem/main.py", "print('answer')\n");
    f.write("problem/checker.cpp", &format!("#include <unistd.h>\n#include <sys/stat.h>\n#include <csignal>\n#include \"testlib.h\"\nint main(int argc, char **argv) {{ registerTestlibCmd(argc, argv); ouf.readToken(); {body} }}\n"));
    let mut header = HEADER.to_vec();
    if modified {
        header.extend_from_slice(b"\n// local change\n");
    }
    fs::write(f.path("problem/testlib.h"), header).unwrap();
    f.write("problem/validator.cpp", "#include \"testlib.h\"\nint main(int argc, char **argv) { registerValidation(argc, argv); inf.readInt(1, 1, \"value\"); inf.readEoln(); inf.readEof(); }\n");
    f.write("problem/tests/a.in", validator.unwrap_or("1\n"));
    f.write("problem/tests/a.ans", "answer\n");
    f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n");
    let validator = if validator.is_some() {
        "validator = { sources = ['validator.cpp'], support_files = ['testlib.h'], language = 'cpp', profile = 'testlib' }\n"
    } else {
        ""
    };
    f.target("python", &format!("[check]\nkind = 'checker'\nchecker = {{ sources = ['checker.cpp'], support_files = ['testlib.h'], language = 'cpp', profile = 'testlib' }}\n{validator}{extra}"));
    reload(f, &["a"])
}
fn reload(f: &Fixture, selected: &[&str]) -> Box<ReadyJob<'static>> {
    let loaded = load_config(&ConfigRequest {
        environment: Environment::default(),
        platform: Platform::host(),
        current_dir: f.0.clone(),
        config_path: Some(f.path("user.toml")),
        workspace_root: Some(f.0.clone()),
        problem_file: Some(f.path("problem/problem.toml")),
        ..ConfigRequest::default()
    });
    let target = loaded.target().unwrap();
    let selected: Vec<_> = selected.iter().map(|id| id.parse().unwrap()).collect();
    let job = f.job(256 << 20, PrunePolicy::default());
    let frozen = job
        .inputs()
        .freeze_judged(
            &f.store(),
            &selected,
            &target,
            loaded.judging().unwrap().unwrap(),
            &f.path("problem"),
        )
        .unwrap();
    Box::new(prepare_job(job, frozen, &target, f.inputs(&PROBE)).unwrap())
}
async fn run(ready: ReadyJob<'static>) -> std::sync::Arc<cpforge::jobs::engine::FinalizedReport> {
    Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap()
    .run(
        &mut SupervisorExecutor::start().unwrap(),
        &mut MemoryPersistence::default(),
        &mut NoHooks,
    )
    .await
}
fn failed(class: C, reason: R) -> JudgeResult {
    JudgeResult::JudgeFailed { class, reason }
}

macro_rules! checker {
    ($name:ident, $body:expr, $extra:expr, $expected:expr, $verdict:expr) => {
        #[test]
        fn $name() {
            run_test(move || async move {
                let f = Fixture::new();
                let ready = ready(&f, $body, $extra, None, false);
                let dir = ready.dir.clone();
                let report = run(*ready).await;
                let test = &report.data().tests[0];
                assert_eq!(test.verdict, Some($verdict));
                let judge = &test.judges[0];
                let expected = $expected;
                assert_eq!(judge.result, Some(expected.clone()));
                if let JudgeResult::Scored { raw, scale } = expected {
                    assert_eq!(test.score, Some(raw));
                    assert_eq!(
                        report.data().details.score,
                        Some(r::AggregateScore::Points {
                            aggregation: r::Aggregation::Sum,
                            value: raw,
                            max: scale,
                            full: raw == scale,
                        })
                    );
                }
                if matches!(
                    expected,
                    JudgeResult::JudgeFailed {
                        class: C::Crashed,
                        ..
                    }
                ) {
                    assert!(judge.report.is_none());
                }
                if let Some(path) = judge.report.as_ref().and_then(|r| r.path.as_ref()) {
                    let retained = path.split('/').fold(dir, |p, c| p.join(c));
                    assert!(retained.is_file());
                }
                r::wire::encode_report(&report).unwrap();
            });
        }
    };
}
const POINTS: &str = "scoring = { points = true, scale = 100 }\n";
fn scored(raw: &str) -> JudgeResult {
    JudgeResult::Scored {
        raw: r::Score::parse(raw).unwrap(),
        scale: r::Score::parse("100").unwrap(),
    }
}
checker!(
    accepted,
    "quitf(_ok, \"ok\");",
    "",
    JudgeResult::Passed,
    TestVerdict::Passed
);
checker!(
    wrong_answer,
    "quitf(_wa, \"wrong\");",
    "",
    JudgeResult::Rejected {
        fail: r::JudgeFail::WrongAnswer
    },
    TestVerdict::WrongAnswer
);
checker!(
    presentation_error,
    "quitf(_pe, \"format\");",
    "",
    JudgeResult::Rejected {
        fail: r::JudgeFail::PresentationError
    },
    TestVerdict::PresentationError
);
checker!(
    unexpected_eof,
    "ouf.readToken(); ouf.readToken();",
    "",
    JudgeResult::Rejected {
        fail: r::JudgeFail::PresentationError
    },
    TestVerdict::PresentationError
);
checker!(
    declared_fail,
    "quitf(_fail, \"broken\");",
    "",
    failed(C::DeclaredFail, R::DeclaredFail),
    TestVerdict::CheckerFailed
);
checker!(
    unset_points,
    "quitpi(\"12.5\");",
    POINTS,
    failed(C::DeclaredFail, R::DeclaredFail),
    TestVerdict::CheckerFailed
);
checker!(
    points_fraction,
    "quitp(12.5);",
    POINTS,
    scored("12.5"),
    TestVerdict::Scored
);
checker!(
    points_zero,
    "quitp(0);",
    POINTS,
    scored("0"),
    TestVerdict::Scored
);
checker!(
    points_full,
    "quitp(100);",
    POINTS,
    scored("100"),
    TestVerdict::Scored
);
checker!(
    points_disabled,
    "quitp(12.5);",
    "",
    failed(C::ProtocolUnsupported, R::PointsNotEnabled),
    TestVerdict::CheckerFailed
);
checker!(
    partial,
    "quitf(_pc(13), \"partial\");",
    "",
    failed(C::ProtocolUnsupported, R::ExitUnsupported),
    TestVerdict::CheckerFailed
);

// These checkers still compile against the real header and register the
// profile, then deliberately violate one observable protocol requirement.
macro_rules! raw {
    ($xml:literal, $code:literal) => {
        concat!(
            "FILE *f = fopen(argv[4], \"wb\"); fputs(\"",
            $xml,
            "\", f); fclose(f); std::_Exit(",
            $code,
            ");"
        )
    };
}
checker!(
    unknown_exit,
    raw!("<result outcome='accepted'/>", "42"),
    "",
    failed(C::ProtocolUnsupported, R::ExitUnsupported),
    TestVerdict::CheckerFailed
);
checker!(
    disagreement,
    raw!("<result outcome='wrong-answer'/>", "0"),
    "",
    failed(C::ReportInvalid, R::Disagreement),
    TestVerdict::CheckerFailed
);
checker!(
    disagreement_exit_one_accepted,
    raw!("<result outcome='accepted'/>", "1"),
    "",
    failed(C::ReportInvalid, R::Disagreement),
    TestVerdict::CheckerFailed
);
checker!(
    disagreement_points_exit_fail,
    raw!("<result outcome='fail'/>", "7"),
    POINTS,
    failed(C::ReportInvalid, R::Disagreement),
    TestVerdict::CheckerFailed
);
checker!(
    missing_report,
    "std::_Exit(0);",
    "",
    failed(C::ReportInvalid, R::ReportMissing),
    TestVerdict::CheckerFailed
);
checker!(
    oversized_report,
    "FILE *f = fopen(argv[4], \"wb\"); for (int i = 0; i < 65537; ++i) fputc('x', f); fclose(f); std::_Exit(0);",
    "",
    failed(C::ReportInvalid, R::ReportTooLarge),
    TestVerdict::CheckerFailed
);
checker!(
    malformed_report,
    raw!("<result outcome='accepted'>", "0"),
    "",
    failed(C::ReportInvalid, R::ReportMalformed),
    TestVerdict::CheckerFailed
);
checker!(
    partial_supported_exit,
    raw!("<result outcome='partially-correct' pctype='2'/>", "0"),
    "",
    failed(C::ProtocolUnsupported, R::OutcomeUnsupported),
    TestVerdict::CheckerFailed
);
checker!(
    points_out_of_range,
    raw!("<result outcome='points' points='100.000000001'/>", "7"),
    POINTS,
    failed(C::ReportInvalid, R::PointsInvalid),
    TestVerdict::CheckerFailed
);
checker!(
    wrong_schema,
    raw!("<result outcome='accepted' points='1'/>", "42"),
    "",
    failed(C::ReportInvalid, R::ReportSchema),
    TestVerdict::CheckerFailed
);
checker!(
    unsupported_encoding,
    raw!(
        "<?xml version='1.0' encoding='koi8-r'?><result outcome='accepted'/>",
        "0"
    ),
    "",
    failed(C::ReportInvalid, R::ReportUndecodable),
    TestVerdict::CheckerFailed
);

#[test]
fn cp1251_message_and_pinned_provenance() {
    run_test(move || async move {
        let f = Fixture::new();
        let ready = ready(
            &f,
            "quitf(_wa, \"%s\", std::string(3000, char(0xdf)).c_str());",
            "",
            None,
            false,
        );
        let report = run(*ready).await;
        let judge = &report.data().tests[0].judges[0];
        let message = judge.message.as_ref().unwrap();
        assert_eq!(message.head, "Я".repeat(2048).as_bytes());
        assert_eq!(message.len, 6000);
        assert_eq!(
            judge.report.as_ref().unwrap().encoding,
            Some(r::JudgeEncoding::Windows1251)
        );
        assert!(matches!(
            report.data().details.roles[0].provenance,
            r::RoleProvenance::Built {
                header: r::Header::Candidate { pinned: true, .. }
            }
        ));
        assert!(!report.data().details.warnings.iter().any(|w| matches!(
            w.kind,
            cpforge::jobs::engine::WarningKind::TestlibHeader { .. }
        )));
    });
}
#[test]
fn modified_header_warns() {
    run_test(move || async move {
        let f = Fixture::new();
        let report = run(*ready(&f, "quitf(_ok, \"ok\");", "", None, true)).await;
        assert_eq!(report.data().tests[0].verdict, Some(TestVerdict::Passed));
        assert!(matches!(
            report.data().details.roles[0].provenance,
            r::RoleProvenance::Built {
                header: r::Header::Candidate { pinned: false, .. }
            }
        ));
        assert!(report.data().details.warnings.iter().any(|w| matches!(
            w.kind,
            cpforge::jobs::engine::WarningKind::TestlibHeader {
                detail: r::TestlibHeaderDetail::NotPinned,
                ..
            }
        )));
    });
}
#[test]
fn validator_accepts_and_rejects_stdin() {
    run_test(move || async move {
        for valid in [true, false] {
            let f = Fixture::new();
            let ready = ready(
                &f,
                "quitf(_ok, \"ok\");",
                "",
                Some(if valid { "1\n" } else { "2\n" }),
                false,
            );
            let report = run(*ready).await;
            let test = &report.data().tests[0];
            assert_eq!(
                test.validation.as_ref().unwrap().outcome,
                if valid {
                    r::ValidationOutcome::Valid
                } else {
                    r::ValidationOutcome::ValidationFailed
                }
            );
            assert_eq!(
                test.verdict,
                Some(if valid {
                    TestVerdict::Passed
                } else {
                    TestVerdict::CheckerFailed
                })
            );
            // The header reports the violated range on stderr, which is kept.
            let stderr = test.validation.as_ref().unwrap().stderr.as_ref();
            assert_eq!(stderr.is_some(), !valid);
            r::wire::encode_report(&report).unwrap();
        }
    });
}

checker!(
    report_link,
    "symlink(argv[1], argv[4]); std::_Exit(0);",
    "",
    failed(C::ReportInvalid, R::ReportNotRegular),
    TestVerdict::CheckerFailed
);
checker!(
    report_fifo,
    "mkfifo(argv[4], 0600); std::_Exit(0);",
    "",
    failed(C::ReportInvalid, R::ReportNotRegular),
    TestVerdict::CheckerFailed
);
checker!(
    report_permission,
    "FILE *f = fopen(argv[4], \"wb\"); fclose(f); chmod(argv[4], 0000); std::_Exit(0);",
    "",
    failed(C::ReportInvalid, R::ReportUnreadable),
    TestVerdict::CheckerFailed
);
checker!(
    crash_before_read,
    "FILE *f = fopen(argv[4], \"wb\"); fputs(\"<result outcome='accepted'/>\", f); fclose(f); raise(SIGKILL);",
    "",
    failed(C::Crashed, R::Signal),
    TestVerdict::CheckerFailed
);
checker!(
    report_at_bound,
    "FILE *f = fopen(argv[4], \"wb\"); fputs(\"<result outcome='accepted'>\", f); for (int i = 0; i < 65500; ++i) fputc(' ', f); fputs(\"</result>\", f); fclose(f); std::_Exit(0);",
    "",
    JudgeResult::Passed,
    TestVerdict::Passed
);

#[test]
fn aggregation_and_empty_selection() {
    run_test(move || async move {
        for aggregation in ["sum", "min"] {
            let f = Fixture::new();
            let extra = format!(
                "scoring = {{ points = true, scale = 100, aggregation = '{aggregation}' }}\n"
            );
            let body = "int n = inf.readInt(); if (n == 1) quitp(12.5); if (n == 2) quitf(_ok, \"full\"); if (n == 3) quitf(_wa, \"wrong\"); quitf(_fail, \"broken\");";
            drop(ready(&f, body, &extra, None, false));
            let mut manifest = String::from("schema = 1\n");
            for (index, id) in ["a", "b", "c", "d"].into_iter().enumerate() {
                f.write(
                    &format!("problem/tests/{id}.in"),
                    &format!("{}\n", index + 1),
                );
                // Each entry needs its own answer file: two entries may not share a path (storage §8.1).
                f.write(&format!("problem/tests/{id}.ans"), "ok\n");
                manifest.push_str(&format!("[[test]]\nid = '{id}'\ninput = 'tests/{id}.in'\nanswer = 'tests/{id}.ans'\norigin = 'custom'\nenabled = true\n"));
            }
            f.write("problem/tests.toml", &manifest);
            let report = run(*reload(&f, &["a", "b", "c"])).await;
            assert_eq!(
                report.data().details.score,
                Some(r::AggregateScore::Points {
                    aggregation: if aggregation == "sum" {
                        r::Aggregation::Sum
                    } else {
                        r::Aggregation::Min
                    },
                    value: r::Score::parse(if aggregation == "sum" { "112.5" } else { "0" })
                        .unwrap(),
                    max: r::Score::parse(if aggregation == "sum" { "300" } else { "100" }).unwrap(),
                    full: false,
                })
            );
            let report = run(*reload(&f, &["a", "d"])).await;
            assert_eq!(
                report.data().details.score,
                Some(r::AggregateScore::Incomplete {
                    aggregation: if aggregation == "sum" {
                        r::Aggregation::Sum
                    } else {
                        r::Aggregation::Min
                    },
                    missing: 1,
                })
            );
            let report = run(*reload(&f, &[])).await;
            assert_eq!(
                report.data().details.score,
                Some(r::AggregateScore::NoTests)
            );
            assert_eq!(report.data().result.verdict, r::JobVerdict::NoTests);
        }
    });
}

struct Observed {
    real: SupervisorExecutor,
    budget: std::sync::Arc<cpforge::storage::budget::JobBudget>,
    unverified: bool,
    _held: Option<cpforge::storage::budget::Reservation>,
}
impl Executor for Observed {
    fn prepare_run(
        &mut self,
        preparation: cpforge::jobs::engine::RunPreparation,
        cancel: cpforge::supervisor::CancelToken,
    ) -> cpforge::jobs::engine::JobFuture<'_, cpforge::jobs::engine::PreparedRun> {
        self.real.prepare_run(preparation, cancel)
    }
    fn finish_judge(
        &mut self,
        work: Box<dyn FnOnce() + Send>,
        deadline: tokio::time::Instant,
    ) -> cpforge::jobs::engine::JobFuture<'_, Result<(), cpforge::report::RunNote>> {
        self.real.finish_judge(work, deadline)
    }
    fn execute(
        &mut self,
        spec: std::sync::Arc<cpforge::jobs::engine::ExecutionSpec>,
        cancel: cpforge::supervisor::CancelToken,
        events: cpforge::jobs::engine::RunEvents,
        resources: cpforge::jobs::engine::RunResources,
    ) -> cpforge::jobs::engine::JobFuture<'_, cpforge::jobs::engine::ExecutionRecord> {
        Box::pin(async move {
            let checker = matches!(&spec.role, Role::Checker { .. });
            let mut record = self.real.execute(spec, cancel, events, resources).await;
            if checker {
                if self.unverified {
                    if let ExecutionRecord::Started(root) = &mut record {
                        root.termination.cleanup = cpforge::supervisor::Cleanup::Unverified {
                            reason: cpforge::supervisor::UnverifiedReason::Io,
                        };
                    }
                } else {
                    let usage = self.budget.usage();
                    self._held = Some(
                        self.budget
                            .reserve(self.budget.limit() - usage.retained - usage.reserved)
                            .unwrap(),
                    );
                }
            }
            record
        })
    }
}
#[test]
fn unverified_cleanup_and_extraction_reservation_refusal() {
    run_test(move || async move {
        for unverified in [true, false] {
            let f = Fixture::new();
            let ready = ready(&f, "quitf(_ok, \"ok\");", "", None, false);
            let budget = ready.budget.clone();
            let engine = Engine::new(
                *ready,
                JobRequest::Test {
                    comparator: Comparator::Tokens,
                },
            )
            .unwrap();
            let mut executor = Observed {
                real: SupervisorExecutor::start().unwrap(),
                budget,
                unverified,
                _held: None,
            };
            let report = engine
                .run(
                    &mut executor,
                    &mut MemoryPersistence::default(),
                    &mut NoHooks,
                )
                .await;
            let test = &report.data().tests[0];
            if unverified {
                assert_eq!(test.verdict, Some(TestVerdict::Unchecked));
                assert_eq!(test.judges[0].result, Some(JudgeResult::Unverified));
                assert!(test.judges[0].report.is_none());
            } else {
                assert_eq!(test.verdict, Some(TestVerdict::Infrastructure));
                assert_eq!(
                    test.judges[0].result,
                    Some(JudgeResult::NotRead {
                        reason: r::JudgeNotRead::ExtractionFailed
                    })
                );
                assert!(test.judges[0].report.as_ref().unwrap().path.is_none());
            }
            r::wire::encode_report(&report).unwrap();
        }
    });
}

#[test]
fn vendored_header_and_license_hashes() {
    use sha2::{Digest, Sha256};
    assert_eq!(
        format!("{:x}", Sha256::digest(HEADER)),
        "d92ca8a6b3690d34d5c56c9394d99798130e21cb0096db817d91e265f8d958c3"
    );
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(include_bytes!("../fixtures/testlib/LICENSE"))
        ),
        "edf4e4b144f24cc4d8070b7a18d700f9429e850f3353736dd628a0a3cacff6c9"
    );
}
