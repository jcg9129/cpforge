//! Real processes for the simple-profile batch rows. Only the cleanup and
//! creation-error fixtures inject observations around the real supervisor.
use super::{PROBE, ready_support::Fixture, run_test};
use cpforge::config::{Comparator, ConfigRequest, Environment, Platform, load_config};
use cpforge::jobs::engine::{
    CancelSource, Engine, ExecutionRecord, ExecutionSpec, Executor, JobFuture, JobRequest,
    MemoryPersistence, NoHooks, PreparedRun, RunEvents, RunPreparation, RunResources,
    SupervisorExecutor,
};
use cpforge::jobs::{ReadyJob, prepare_job};
use cpforge::report::{self as r, Coverage, SkipReason, TestVerdict, wire};
use cpforge::storage::prune::PrunePolicy;
use cpforge::supervisor::{CancelToken, Cleanup};
use std::fs;
use std::sync::Arc;
use std::time::Duration;

fn ready(
    f: &Fixture,
    solution: &str,
    validator: Option<&str>,
    checker: Option<&str>,
    extra: &str,
) -> Box<ReadyJob<'static>> {
    f.write("cpforge.toml", "schema = 1\n[run]\ndeadline_s = '2s'\nstdout_cap = 4096\n[languages.script]\nextensions = ['sh']\nsources = ['main.sh']\nbuild = false\n[languages.script.run]\nargv = ['sh', '{source}']\n");
    f.write("problem/main.sh", solution);
    f.write("problem/checker.sh", checker.unwrap_or("exit 0"));
    f.write("problem/validator.sh", validator.unwrap_or("exit 0"));
    f.write("problem/data/é space/words", "asset");
    f.write("problem/tests/é space.in", "input\n");
    f.write("problem/tests/é space.ans", "answer\n");
    f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/é space.in'\nanswer = 'tests/é space.ans'\norigin = 'custom'\nenabled = true\n");
    let kind = if checker.is_some() {
        "checker"
    } else {
        "compare"
    };
    let checker = if checker.is_some() {
        "checker = { sources = ['checker.sh'], language = 'script', profile = 'simple', assets = ['data/é space/words'], deadline_s = '2s', stdout_cap = 4096 }\n"
    } else {
        ""
    };
    let validator = if validator.is_some() {
        "validator = { sources = ['validator.sh'], language = 'script', profile = 'simple', deadline_s = '2s', stdout_cap = 4096 }\n"
    } else {
        ""
    };
    let target = f.target(
        "script",
        &format!("[check]\nkind = '{kind}'\n{checker}{validator}{extra}"),
    );
    let config = load_config(&ConfigRequest {
        environment: Environment::default(),
        platform: Platform::host(),
        current_dir: f.0.clone(),
        config_path: Some(f.path("user.toml")),
        workspace_root: Some(f.0.clone()),
        problem_file: Some(f.path("problem/problem.toml")),
        ..ConfigRequest::default()
    });
    let job = f.job(256 << 20, PrunePolicy::default());
    let frozen = job
        .inputs()
        .freeze_judged(
            &f.store(),
            &["a".parse().unwrap()],
            &target,
            config.judging().unwrap().unwrap(),
            &f.path("problem"),
        )
        .unwrap();
    Box::new(prepare_job(job, frozen, &target, f.inputs(&PROBE)).unwrap())
}

fn engine(ready: ReadyJob<'static>) -> Engine<'static> {
    Engine::new(
        ready,
        JobRequest::Test {
            comparator: Comparator::Tokens,
        },
    )
    .unwrap()
}

async fn run(ready: ReadyJob<'static>) -> Arc<cpforge::jobs::engine::FinalizedReport> {
    engine(ready)
        .run(
            &mut SupervisorExecutor::start().unwrap(),
            &mut MemoryPersistence::default(),
            &mut NoHooks,
        )
        .await
}

const CHECK_FILES: &str = "test \"$#\" = 3 && test \"$(cat \"$1\")\" = input && test \"$(cat \"$2\")\" = answer && test \"$(cat \"$3\")\" = answer && test \"$(cat 'data/é space/words')\" = asset || exit 2\nprintf diagnostic >&2\nexit 0\n";
const ADMITTED_PATHS: &str = "test \"$#\" = 3 && test \"$1\" = \"$PWD/input\" && test \"$2\" = \"$PWD/output\" && test \"$3\" = \"$PWD/answer\" && test \"$(cat \"$2\")\" = answer || exit 2\nexit 0\n";
const VALIDATE: &str = "test \"$#\" = 1 && test \"$1\" = \"$PWD/input\" && test \"$(cat \"$1\")\" = input && test \"$(cat)\" = input || exit 2\nexit 0\n";

#[test]
fn b18_private_staging_unicode_paths_assets_and_frozen_sources() {
    run_test(move || async move {
        let f = Fixture::at(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!(
                    "simple é space-{}",
                    cpforge::build::BuildId::generate().unwrap()
                )),
        );
        let ready = ready(&f, "printf answer", Some(VALIDATE), Some(CHECK_FILES), "");
        let dir = ready.dir.clone();
        f.write("problem/checker.sh", "exit 2");
        f.write("problem/tests/é space.in", "live original");
        let report = run(*ready).await;
        let data = report.data();
        assert_eq!(data.tests[0].verdict, Some(TestVerdict::Passed));
        assert_eq!(
            data.tests[0].validation.as_ref().unwrap().outcome,
            r::ValidationOutcome::Valid
        );
        assert_eq!(data.tests[0].first_execution, 0);
        assert_eq!(data.tests[0].execution_count, 3);
        assert_eq!(data.tests[0].judges[0].result, Some(r::JudgeResult::Passed));
        for index in [0, 2] {
            let cwd = dir.join(format!("r{index:05}")).join("cwd");
            for name in ["input", "output", "answer"] {
                assert!(!cwd.join(name).exists());
            }
            assert!(
                !data.executions[index]
                    .summary
                    .as_ref()
                    .unwrap()
                    .notes
                    .contains(&r::RunNote::CwdOverflow)
            );
        }
        assert!(data.details.judge_fingerprint.is_some());
        wire::encode_report(&report).unwrap();
    });
}

#[test]
fn b19_rejection_keeps_one_kib_stderr_with_full_capture_hash() {
    run_test(move || async move {
        use sha2::{Digest, Sha256};
        let f = Fixture::new();
        let report = run(*ready(
            &f,
            "printf answer",
            None,
            Some("i=0; while [ \"$i\" -lt 2000 ]; do printf x >&2; i=$((i+1)); done; exit 1"),
            "",
        ))
        .await;
        assert_eq!(
            report.data().tests[0].verdict,
            Some(TestVerdict::WrongAnswer)
        );
        let judge = &report.data().tests[0].judges[0];
        let stderr = judge.stderr.as_ref().unwrap();
        assert_eq!(stderr.head, vec![b'x'; 1024]);
        assert_eq!(stderr.len, 2000);
        assert_eq!(
            stderr.sha256,
            <[u8; 32]>::from(Sha256::digest(vec![b'x'; 2000]))
        );
        assert!(judge.message.is_none() && judge.report.is_none());
        let doc: serde_json::Value =
            serde_json::from_slice(&wire::encode_stdout_report(&report).unwrap()).unwrap();
        assert_eq!(doc["tests"][0]["judges"][0]["stderr"]["truncated"], true);
        assert_eq!(doc["tests"][0]["check"]["kind"], "rejected");
    });
}

const CRASHES: &[(&str, r::JudgeReason)] = &[
    ("exit 2", r::JudgeReason::ExitCode),
    ("kill -TERM $$", r::JudgeReason::Signal),
    ("exec sleep 60", r::JudgeReason::WallDeadline),
    (
        "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done",
        r::JudgeReason::OutputBudget,
    ),
];

#[test]
fn b20_checker_exit_signal_deadline_and_own_output_limit_are_judge_failures() {
    run_test(move || async move {
        for (script, reason) in CRASHES {
            let f = Fixture::new();
            let report = run(*ready(
                &f,
                "printf answer",
                None,
                Some(script),
                "scoring = { points = true, scale = 100 }\n",
            ))
            .await;
            let test = &report.data().tests[0];
            assert_eq!(
                test.verdict,
                Some(TestVerdict::CheckerFailed),
                "{script}: {test:?}"
            );
            assert!(test.score.is_none());
            assert_eq!(
                test.judges[0].result,
                Some(r::JudgeResult::JudgeFailed {
                    class: r::JudgeFailClass::Crashed,
                    reason: *reason,
                })
            );
            wire::encode_report(&report).unwrap();
        }
    });
}

#[test]
fn b06_invalid_input_skips_solution_and_checker_then_later_test_uses_its_admitted_paths() {
    run_test(move || async move {
        let f = Fixture::new();
        // The checker checks that every appended path names its own actual
        // private directory, whatever slot the plan gave it.
        let mut ready = ready(
            &f,
            "printf answer",
            Some("exit 1"),
            Some(ADMITTED_PATHS),
            "",
        );
        // Re-freeze with a second test. Only the first input is invalid.
        drop(ready);
        f.write(
            "problem/validator.sh",
            "test \"$(cat)\" = input && exit 1; exit 0",
        );
        f.write("problem/tests/second.in", "valid");
        let catalog = fs::read_to_string(f.path("problem/tests.toml")).unwrap();
        f.write("problem/tests.toml", &format!("{catalog}\n[[test]]\nid = 'b'\ninput = 'tests/second.in'\nanswer = 'tests/é space.ans'\norigin = 'custom'\nenabled = true\n"));
        let config = load_config(&ConfigRequest {
            current_dir: f.0.clone(),
            workspace_root: Some(f.0.clone()),
            config_path: Some(f.path("user.toml")),
            problem_file: Some(f.path("problem/problem.toml")),
            ..ConfigRequest::default()
        });
        let target = config.target().unwrap();
        let job = f.job(256 << 20, PrunePolicy::default());
        let frozen = job
            .inputs()
            .freeze_judged(
                &f.store(),
                &["a".parse().unwrap(), "b".parse().unwrap()],
                &target,
                config.judging().unwrap().unwrap(),
                &f.path("problem"),
            )
            .unwrap();
        ready = Box::new(prepare_job(job, frozen, &target, f.inputs(&PROBE)).unwrap());
        let dir = ready.dir.clone();
        let report = run(*ready).await;
        let data = report.data();
        assert_eq!(data.tests[0].verdict, Some(TestVerdict::CheckerFailed));
        assert_eq!(
            data.tests[0].validation.as_ref().unwrap().outcome,
            r::ValidationOutcome::Invalid
        );
        assert!(
            matches!(&data.tests[0].check.as_ref().unwrap().kind, r::CheckKind::Judged { outcome }
            if matches!(**outcome, r::JudgedCheck::JudgeFailed { class: r::JudgeFailClass::TestDataError, .. }))
        );
        for execution in &data.executions[1..3] {
            assert_eq!(
                execution.result.coverage,
                Coverage::Skipped {
                    reason: SkipReason::InputNotValidated
                }
            );
            assert!(execution.run.is_none());
        }
        assert_eq!(data.tests[1].verdict, Some(TestVerdict::Passed));
        assert_eq!(data.tests[1].first_execution, 3);
        assert_eq!(data.executions[3].run.as_ref().unwrap().as_str(), "r00001");
        assert_eq!(data.executions[5].run.as_ref().unwrap().as_str(), "r00003");
        assert_eq!(
            fs::read(dir.join("r00002").join("out").join("stdout")).unwrap(),
            b"answer"
        );
        wire::encode_report(&report).unwrap();
    });
}

#[test]
fn b07_validator_failures_skip_solution_and_preserve_diagnostics() {
    run_test(move || async move {
        for (script, reason) in CRASHES {
            let f = Fixture::new();
            let report = run(*ready(
                &f,
                "printf answer",
                Some(&format!("printf diagnostic >&2; {script}")),
                Some("exit 0"),
                "",
            ))
            .await;
            let test = &report.data().tests[0];
            assert_eq!(test.verdict, Some(TestVerdict::CheckerFailed));
            let v = test.validation.as_ref().unwrap();
            assert_eq!(v.outcome, r::ValidationOutcome::ValidatorFailed);
            assert_eq!(v.stderr.as_ref().unwrap().head, b"diagnostic");
            assert!(
                matches!(&test.check.as_ref().unwrap().kind, r::CheckKind::Judged { outcome }
                if matches!(**outcome, r::JudgedCheck::JudgeFailed { reason: actual, .. } if actual == *reason))
            );
            assert!(report.data().executions[1].run.is_none());
        }
    });
}

#[test]
fn b08_validator_acceptance_preserves_comparator_results() {
    run_test(move || async move {
        for (solution, verdict) in [
            ("printf answer", TestVerdict::Passed),
            ("printf wrong", TestVerdict::WrongAnswer),
        ] {
            let f = Fixture::new();
            let report = run(*ready(&f, solution, Some(VALIDATE), None, "")).await;
            assert_eq!(report.data().tests[0].verdict, Some(verdict));
            assert!(report.data().tests[0].judges.is_empty());
        }
    });
}

#[test]
fn b16_checker_acceptance_after_solution_failure_is_diagnostic_and_never_success() {
    run_test(move || async move {
        for ((script, _), verdict) in CRASHES.iter().zip([
            TestVerdict::NonzeroExit,
            TestVerdict::Signal,
            TestVerdict::WallDeadline,
            TestVerdict::OutputBudget,
        ]) {
            let f = Fixture::new();
            let report = run(*ready(&f, script, None, Some("exit 0"), "")).await;
            let test = &report.data().tests[0];
            // The solution class decides; the accepting checker never does.
            assert_eq!(test.verdict, Some(verdict), "{script}: {test:?}");
            assert!(test.check.as_ref().unwrap().diagnostic);
            assert_eq!(test.check.as_ref().unwrap().kind, r::CheckKind::Passed);
            assert_eq!(test.judges[0].result, Some(r::JudgeResult::Passed));
            assert_eq!(
                report.data().executions[1].result.coverage,
                Coverage::Started
            );
        }
    });
}

#[derive(Clone, Copy)]
enum Inject {
    Create,
    Unverified,
    Incomplete,
}
struct ObservedExecutor {
    real: SupervisorExecutor,
    index: u32,
    inject: Inject,
}
impl Executor for ObservedExecutor {
    fn prepare_run(
        &mut self,
        preparation: RunPreparation,
        cancel: CancelToken,
    ) -> JobFuture<'_, PreparedRun> {
        self.real.prepare_run(preparation, cancel)
    }
    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        cancel: CancelToken,
        events: RunEvents,
        resources: RunResources,
    ) -> JobFuture<'_, ExecutionRecord> {
        Box::pin(async move {
            let selected = spec.execution == self.index;
            let spec = if selected && matches!(self.inject, Inject::Create) {
                let mut spec = (*spec).clone();
                spec.command.program = spec.directory.join("missing-executable");
                Arc::new(spec)
            } else {
                spec
            };
            let mut record = self.real.execute(spec, cancel, events, resources).await;
            if let ExecutionRecord::Started(root) = &mut record {
                if selected {
                    match self.inject {
                        Inject::Unverified => {
                            root.termination.cleanup = Cleanup::Unverified {
                                reason: cpforge::supervisor::UnverifiedReason::Io,
                            }
                        }
                        Inject::Incomplete => {
                            root.termination.cleanup = Cleanup::Incomplete { survivors: 1 }
                        }
                        Inject::Create => {}
                    }
                }
            }
            record
        })
    }
}

#[test]
fn b03_b11_b17_creation_failure_is_infrastructure_at_each_role() {
    run_test(move || async move {
        for index in 0..3 {
            let f = Fixture::new();
            let engine = engine(*ready(
                &f,
                "printf answer",
                Some(VALIDATE),
                Some("exit 0"),
                "",
            ));
            let report = engine
                .run(
                    &mut ObservedExecutor {
                        real: SupervisorExecutor::start().unwrap(),
                        index,
                        inject: Inject::Create,
                    },
                    &mut MemoryPersistence::default(),
                    &mut NoHooks,
                )
                .await;
            assert_eq!(
                report.data().tests[0].verdict,
                Some(TestVerdict::Infrastructure)
            );
            assert!(matches!(
                report.data().executions[index as usize].result.coverage,
                Coverage::NotStarted {
                    reason: r::NotStartedReason::CreateFailed
                }
            ));
            if index < 2 {
                assert_eq!(
                    report.data().executions[2].result.coverage,
                    Coverage::Skipped {
                        reason: if index == 0 {
                            SkipReason::InputNotValidated
                        } else {
                            SkipReason::NotEligible
                        }
                    }
                );
            }
        }
    });
}

#[test]
fn b22_unverified_checker_is_unchecked_and_a_crash_still_decides_first() {
    run_test(move || async move {
        for (script, verdict) in [
            ("exit 0", TestVerdict::Unchecked),
            ("kill -TERM $$", TestVerdict::CheckerFailed),
        ] {
            let f = Fixture::new();
            let report = engine(*ready(&f, "printf answer", None, Some(script), ""))
                .run(
                    &mut ObservedExecutor {
                        real: SupervisorExecutor::start().unwrap(),
                        index: 1,
                        inject: Inject::Unverified,
                    },
                    &mut MemoryPersistence::default(),
                    &mut NoHooks,
                )
                .await;
            assert_eq!(report.data().tests[0].verdict, Some(verdict));
        }
        let f = Fixture::new();
        let report = engine(*ready(&f, "printf answer", None, Some("exit 0"), ""))
            .run(
                &mut ObservedExecutor {
                    real: SupervisorExecutor::start().unwrap(),
                    index: 1,
                    inject: Inject::Incomplete,
                },
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        assert_eq!(
            report.data().tests[0].verdict,
            Some(TestVerdict::Infrastructure)
        );
    });
}

#[test]
fn b01_cancel_before_validator_and_b04_b12_b15_cancel_during_each_role() {
    run_test(move || async move {
        let f = Fixture::new();
        let e = engine(*ready(
            &f,
            "printf answer",
            Some(VALIDATE),
            Some("exit 0"),
            "",
        ));
        e.cancellation().request_cancel(CancelSource::Api);
        let report = e
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        assert_eq!(report.data().tests[0].verdict, Some(TestVerdict::Cancelled));
        assert!(report.data().executions.iter().all(|e| e.result.coverage
            == Coverage::Skipped {
                reason: SkipReason::Cancelled
            }));
        for index in 0..3 {
            let f = Fixture::new();
            let scripts = [
                if index == 0 {
                    "printf ready > marker; exec sleep 60"
                } else {
                    VALIDATE
                },
                if index == 1 {
                    "printf ready > marker; exec sleep 60"
                } else {
                    "printf answer"
                },
                if index == 2 {
                    "printf ready > marker; exec sleep 60"
                } else {
                    "exit 0"
                },
            ];
            let ready = ready(&f, scripts[1], Some(scripts[0]), Some(scripts[2]), "");
            let marker = ready
                .dir
                .join(format!("r{index:05}"))
                .join("cwd")
                .join("marker");
            let e = engine(*ready);
            let cancel = e.cancellation();
            let mut executor = SupervisorExecutor::start().unwrap();
            let mut persistence = MemoryPersistence::default();
            let mut hooks = NoHooks;
            let (report, ()) = tokio::join!(
                e.run(&mut executor, &mut persistence, &mut hooks),
                async move {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while !marker.exists() {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    })
                    .await
                    .unwrap();
                    cancel.request_cancel(CancelSource::Api);
                }
            );
            let test = &report.data().tests[0];
            assert_eq!(test.verdict, Some(TestVerdict::Cancelled));
            assert_eq!(
                report.data().executions[index].result.class,
                Some(r::RunClass::Cancelled)
            );
            if index == 2 {
                assert!(
                    matches!(&test.check.as_ref().unwrap().kind, r::CheckKind::Judged { outcome }
                    if matches!(**outcome, r::JudgedCheck::Unchecked { reason: r::CheckingUnchecked::CheckingInterrupted }))
                );
            }
        }
    });
}

fn reload(f: &Fixture) -> Box<ReadyJob<'static>> {
    let config = load_config(&ConfigRequest {
        current_dir: f.0.clone(),
        workspace_root: Some(f.0.clone()),
        config_path: Some(f.path("user.toml")),
        problem_file: Some(f.path("problem/problem.toml")),
        ..ConfigRequest::default()
    });
    let target = config.target().unwrap();
    let job = f.job(256 << 20, PrunePolicy::default());
    let frozen = job
        .inputs()
        .freeze_judged(
            &f.store(),
            &["a".parse().unwrap()],
            &target,
            config.judging().unwrap().unwrap(),
            &f.path("problem"),
        )
        .unwrap();
    Box::new(prepare_job(job, frozen, &target, f.inputs(&PROBE)).unwrap())
}

#[test]
fn simple_cpp_checker_builds_and_judges_with_three_staged_arguments() {
    run_test(move || async move {
        let f = Fixture::new();
        drop(ready(&f, "printf answer", None, Some("exit 0"), ""));
        f.write("problem/checker.cpp", "#include <fstream>\n#include <string>\nint main(int argc, char **argv) { if(argc != 4) return 2; std::string in, out, ans; std::ifstream(argv[1]) >> in; std::ifstream(argv[2]) >> out; std::ifstream(argv[3]) >> ans; return in == \"input\" && out == \"answer\" && ans == \"answer\" ? 0 : 1; }\n");
        let problem = fs::read_to_string(f.path("problem/problem.toml")).unwrap();
        f.write(
            "problem/problem.toml",
            &problem.replace(
                "sources = ['checker.sh'], language = 'script'",
                "sources = ['checker.cpp'], language = 'cpp'",
            ),
        );
        let report = run(*reload(&f)).await;
        assert_eq!(report.data().tests[0].verdict, Some(TestVerdict::Passed));
        assert_eq!(
            report.data().details.roles[0]
                .build
                .as_ref()
                .unwrap()
                .result,
            r::RoleBuildResult::Succeeded
        );
        wire::encode_report(&report).unwrap();
    });
}

#[test]
fn shell_checker_arguments_start_at_one_and_answer_none_is_an_empty_file() {
    run_test(move || async move {
        let f = Fixture::new();
        drop(ready(
            &f,
            "printf answer",
            None,
            Some("exit 0"),
            "answer = 'none'\n",
        ));
        let workspace = fs::read_to_string(f.path("cpforge.toml")).unwrap();
        f.write("cpforge.toml", &format!("{workspace}\n[languages.judge]\nextensions = ['judge']\nsources = ['checker.sh']\nbuild = false\n[languages.judge.run]\nshell = {{ interpreter = ['sh', '-c'], script = 'test \"$0\" = cpf && test \"$#\" = 3 && test \"$(cat \"$1\")\" = input && test \"$(cat \"$2\")\" = answer && test -f \"$3\" && test ! -s \"$3\"' }}\n"));
        let problem = fs::read_to_string(f.path("problem/problem.toml")).unwrap();
        f.write(
            "problem/problem.toml",
            &problem.replace(
                "sources = ['checker.sh'], language = 'script'",
                "sources = ['checker.sh'], language = 'judge'",
            ),
        );
        let catalog = fs::read_to_string(f.path("problem/tests.toml")).unwrap();
        f.write(
            "problem/tests.toml",
            &catalog.replace("answer = 'tests/é space.ans'\n", ""),
        );
        let report = run(*reload(&f)).await;
        assert_eq!(report.data().tests[0].verdict, Some(TestVerdict::Passed));
        assert!(matches!(
            report.data().tests[0].frozen.answer,
            cpforge::tests_store::FrozenAnswer::Absent
        ));
    });
}

struct CancelAfter {
    real: SupervisorExecutor,
    cancel: cpforge::jobs::engine::CancelState,
    index: u32,
}
impl Executor for CancelAfter {
    fn prepare_run(
        &mut self,
        preparation: RunPreparation,
        cancel: CancelToken,
    ) -> JobFuture<'_, PreparedRun> {
        self.real.prepare_run(preparation, cancel)
    }
    fn execute(
        &mut self,
        spec: Arc<ExecutionSpec>,
        cancel: CancelToken,
        events: RunEvents,
        resources: RunResources,
    ) -> JobFuture<'_, ExecutionRecord> {
        Box::pin(async move {
            let index = spec.execution;
            let record = self.real.execute(spec, cancel, events, resources).await;
            if index == self.index {
                self.cancel.request_cancel(CancelSource::Api);
            }
            record
        })
    }
}

#[test]
fn b09_b13_cancellation_between_roles_skips_the_next_admission() {
    run_test(move || async move {
        for index in [0, 1] {
            let f = Fixture::new();
            let e = engine(*ready(
                &f,
                "printf answer",
                Some(VALIDATE),
                Some("exit 0"),
                "",
            ));
            let cancel = e.cancellation();
            let report = e
                .run(
                    &mut CancelAfter {
                        real: SupervisorExecutor::start().unwrap(),
                        cancel,
                        index,
                    },
                    &mut MemoryPersistence::default(),
                    &mut NoHooks,
                )
                .await;
            assert_eq!(report.data().tests[0].verdict, Some(TestVerdict::Cancelled));
            assert_eq!(
                report.data().executions[index as usize].result.class,
                Some(r::RunClass::ExitedZero)
            );
            assert_eq!(
                report.data().executions[index as usize + 1].result.coverage,
                Coverage::Skipped {
                    reason: SkipReason::Cancelled
                }
            );
        }
    });
}

#[test]
fn b02_b10_failed_solution_build_skips_judging_including_validator() {
    run_test(move || async move {
        for validator in [None, Some(VALIDATE)] {
            let f = Fixture::new();
            drop(ready(&f, "printf answer", validator, Some("exit 0"), ""));
            // Use the ordinary producer path without needing a compiler.
            let workspace = fs::read_to_string(f.path("cpforge.toml")).unwrap();
            f.write("cpforge.toml", &format!("{workspace}\n[languages.failing]\nextensions = ['fail']\nsources = ['main.sh']\nartifacts = {{ file = 'main' }}\n[languages.failing.build]\nargv = ['sh', '-c', 'exit 1']\n[languages.failing.run]\nargv = ['{{artifact}}']\n"));
            let problem = fs::read_to_string(f.path("problem/problem.toml")).unwrap();
            f.write(
                "problem/problem.toml",
                &problem.replacen("language = 'script'", "language = 'failing'", 1),
            );
            let report = run(*reload(&f)).await;
            assert_eq!(report.data().result.verdict, r::JobVerdict::BuildFailed);
            assert_eq!(
                report.data().tests[0].verdict,
                Some(TestVerdict::NotRun {
                    reason: SkipReason::BuildNotSucceeded
                })
            );
            assert!(
                report.data().executions[1..]
                    .iter()
                    .all(|e| e.result.coverage
                        == Coverage::Skipped {
                            reason: SkipReason::BuildNotSucceeded
                        })
            );
        }
    });
}

#[test]
fn b08_invalid_float_answer_stays_test_data_error_with_a_validator() {
    run_test(move || async move {
        let f = Fixture::new();
        drop(ready(&f, "printf 1", Some(VALIDATE), None, ""));
        f.write("problem/tests/é space.ans", "not a number");
        let comparator = Comparator::Float(cpforge::config::FloatComparator {
            abs: Some(1e-6),
            rel: None,
            ulps: None,
            rel_base: cpforge::config::RelativeBase::Answer,
            combine: cpforge::config::Combine::Either,
            int_exact: false,
        });
        let report = Engine::new(*reload(&f), JobRequest::Test { comparator })
            .unwrap()
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        assert_eq!(
            report.data().tests[0].verdict,
            Some(TestVerdict::CheckerFailed)
        );
        assert!(matches!(
            report.data().tests[0].check.as_ref().unwrap().kind,
            r::CheckKind::CheckerFailed { .. }
        ));
    });
}

#[test]
fn prebuilt_simple_checker_runs_with_declared_provenance() {
    run_test(move || async move {
        let f = Fixture::new();
        drop(ready(&f, "printf answer", None, Some("exit 0"), ""));
        f.write(
            "problem/prebuilt",
            "#!/bin/sh\ntest \"$#\" = 3 && test \"$(cat \"$2\")\" = answer\n",
        );
        let problem = fs::read_to_string(f.path("problem/problem.toml")).unwrap();
        f.write(
            "problem/problem.toml",
            &problem.replace(
                "sources = ['checker.sh'], language = 'script'",
                "program = 'prebuilt'",
            ),
        );
        let report = run(*reload(&f)).await;
        assert_eq!(report.data().tests[0].verdict, Some(TestVerdict::Passed));
        assert_eq!(
            report.data().details.roles[0].provenance,
            r::RoleProvenance::Declared
        );
        assert!(report.data().details.roles[0].build.is_none());
    });
}
