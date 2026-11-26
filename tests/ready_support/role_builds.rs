//! Role producers use the same supervised job path as the solution producer.
use super::{PROBE, ready_support::Fixture, run_test};
use cpforge::config::{ConfigRequest, Environment, Platform, load_config};
use cpforge::jobs::engine::{
    CancelSource, Engine, JobRequest, MemoryPersistence, NoHooks, Role, SupervisorExecutor,
};
use cpforge::jobs::{ReadyJob, prepare_job};
use cpforge::report::{
    BuildResult, Coverage, JobVerdict, RoleBuildResult, RoleName, RoleProvenance, SkipReason,
    TestVerdict, wire,
};
use cpforge::storage::prune::PrunePolicy;
use std::fs;
use std::time::Duration;

#[derive(Default)]
struct ReservationPersistence {
    received: Option<u64>,
}
impl cpforge::jobs::engine::Persistence for ReservationPersistence {
    fn enqueue(
        &mut self,
        _artifact: cpforge::jobs::engine::PendingArtifacts,
    ) -> cpforge::jobs::engine::JobFuture<'_, ()> {
        Box::pin(async {})
    }
    fn settle<'a>(
        &'a mut self,
        _executions: &'a mut [cpforge::jobs::engine::ExecutionReport],
    ) -> cpforge::jobs::engine::JobFuture<'a, Vec<cpforge::jobs::engine::Warning>> {
        Box::pin(async { Vec::new() })
    }
    fn publish_report(
        &mut self,
        _report: std::sync::Arc<cpforge::jobs::engine::ReportData>,
        reservation: Option<cpforge::storage::budget::Reservation>,
    ) -> cpforge::jobs::engine::JobFuture<'_, cpforge::jobs::engine::ReportPersistence> {
        self.received = reservation.as_ref().map(|r| r.bytes());
        Box::pin(async {
            cpforge::jobs::engine::ReportPersistence::Failed {
                error: cpforge::jobs::engine::PersistError::WorkerUnavailable,
                message: "in-memory reservation observation".into(),
            }
        })
    }
}

fn ready(f: &Fixture, language: &str, declarations: &str, tests: bool) -> Box<ReadyJob<'static>> {
    let target = f.target(language, declarations);
    let loaded = load_config(&ConfigRequest {
        environment: Environment::default(),
        platform: Platform::host(),
        current_dir: f.0.clone(),
        config_path: Some(f.path("user.toml")),
        workspace_root: Some(f.0.clone()),
        problem_file: Some(f.path("problem/problem.toml")),
        ..ConfigRequest::default()
    });
    let check = loaded.judging().unwrap().unwrap();
    let selected = if tests {
        f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n");
        f.write("problem/tests/a.in", "");
        f.write("problem/tests/a.ans", "");
        vec!["a".parse().unwrap()]
    } else {
        vec![]
    };
    let job = f.job(256 << 20, PrunePolicy::default());
    let frozen = job
        .inputs()
        .freeze_judged(&f.store(), &selected, &target, check, &f.path("problem"))
        .unwrap();
    Box::new(prepare_job(job, frozen, &target, f.inputs(&PROBE)).unwrap())
}

const ROLES: &str = "[check]\nkind = 'checker'\nchecker = { sources = ['checker.cpp'], language = 'cpp', profile = 'simple' }\nvalidator = { sources = ['validator.cpp'], language = 'cpp', profile = 'simple' }\n";

#[test]
fn cpp_role_builds_succeed_in_separate_fresh_outputs() {
    run_test(move || async move {
        let f = Fixture::new();
        for name in ["main.cpp", "checker.cpp", "validator.cpp"] {
            f.write(&format!("problem/{name}"), "int main() { return 0; }\n");
        }
        let ready = ready(&f, "cpp", ROLES, false);
        let fingerprint = ready.judging.as_ref().unwrap().judge_fingerprint;
        let budget = ready.budget.clone();
        let dir = ready.dir.clone();
        // Live originals cannot affect either the build or its report evidence.
        f.write("problem/checker.cpp", "invalid live original");
        let engine = Engine::new(
            *ready,
            JobRequest::Test {
                comparator: cpforge::config::Comparator::Tokens,
            },
        )
        .unwrap();
        assert_eq!(
            engine.plan().iter().map(|p| &p.role).collect::<Vec<_>>(),
            vec![
                &Role::Build,
                &Role::RoleBuild {
                    role: RoleName::Checker
                },
                &Role::RoleBuild {
                    role: RoleName::Validator
                },
            ]
        );
        let report = engine
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        let data = report.data();
        assert_eq!(data.result.verdict, JobVerdict::NoTests);
        assert_eq!(data.build.as_ref().unwrap().result, BuildResult::Succeeded);
        assert_eq!(data.details.judge_fingerprint, fingerprint);
        let mut roots = vec![data.build.as_ref().unwrap().output_root.clone().unwrap()];
        for role in &data.details.roles {
            let build = role.build.as_ref().unwrap();
            assert_eq!(build.result, RoleBuildResult::Succeeded);
            assert_eq!(build.artifacts.len(), 1);
            roots.push(dir.join("builds").join(build.build_id.as_ref().unwrap()));
            assert!(matches!(role.provenance, RoleProvenance::Built { .. }));
        }
        assert_ne!(roots[0], roots[1]);
        assert_ne!(roots[1], roots[2]);
        assert_ne!(roots[0], roots[2]);
        assert!(roots.iter().all(|root| root.is_dir()));
        wire::encode_report(&report).unwrap();
        assert_eq!(budget.usage().reserved, 0);
    });
}

#[test]
fn failed_checker_skips_the_validator_build_and_every_test_execution() {
    run_test(move || async move {
        let f = Fixture::new();
        f.write("problem/main.py", "raise Exception('must not run')\n");
        f.write("problem/checker.cpp", "not C++\n");
        f.write("problem/validator.cpp", "int main() {}\n");
        let ready = ready(&f, "python", ROLES, true);
        let report = Engine::new(
            *ready,
            JobRequest::Test {
                comparator: cpforge::config::Comparator::Tokens,
            },
        )
        .unwrap()
        .run(
            &mut SupervisorExecutor::start().unwrap(),
            &mut MemoryPersistence::default(),
            &mut NoHooks,
        )
        .await;
        let data = report.data();
        assert_eq!(data.result.verdict, JobVerdict::BuildFailed);
        assert!(matches!(
            data.details.roles[0].build.as_ref().unwrap().result,
            RoleBuildResult::Failed { .. }
        ));
        let validator = data.details.roles[1].build.as_ref().unwrap();
        assert_eq!(
            validator.result,
            RoleBuildResult::Skipped {
                reason: SkipReason::BuildNotSucceeded
            }
        );
        assert!(validator.build_id.is_none());
        assert!(validator.output_root.is_none());
        for execution in &data.executions[1..] {
            assert_eq!(
                execution.result.coverage,
                Coverage::Skipped {
                    reason: SkipReason::BuildNotSucceeded
                }
            );
            assert!(execution.run.is_none());
        }
        assert_eq!(
            data.tests[0].verdict,
            Some(TestVerdict::NotRun {
                reason: SkipReason::BuildNotSucceeded
            })
        );
        wire::encode_report(&report).unwrap();
    });
}

#[test]
fn checker_cannot_link_a_solution_output_from_its_own_artifact_directory() {
    run_test(move || async move {
        let f = Fixture::new();
        f.write("problem/main.cpp", "int main() {}\n");
        f.write("problem/checker.cpp", "int main() {}\n");
        // The default solution artifact is named main. If outputs were shared,
        // this probe would find it; a separate checker directory has no main.
        // cpf passes `$0 = cpf` itself (config §5.3), so `$1` is the source.
        let user = fs::read_to_string(f.path("user.toml")).unwrap();
        f.write("user.toml", &format!("{user}\n[languages.judgecpp]\nextensions = ['jcpp']\nsources = ['checker.cpp']\nartifacts = {{ file = 'main' }}\n[languages.judgecpp.build]\nshell = {{ interpreter = ['sh', '-c'], script = 'test ! -e \"$(dirname \"$2\")/main\" && exec \"$3\" \"$1\" -o \"$2\"', args = ['{{source}}', '{{artifact}}', 'c++'] }}\n[languages.judgecpp.run]\nargv = ['{{artifact}}']\n"));
        let declarations = "[check]\nkind = 'checker'\nchecker = { sources = ['checker.cpp'], language = 'judgecpp', profile = 'simple' }\n";
        let ready = ready(&f, "cpp", declarations, false);
        let solution_root = match &ready.planned_target {
            cpforge::build::PlannedTarget::Build(pending) => pending.output_root().to_owned(),
            _ => panic!("solution build"),
        };
        let role_root = ready.judging.as_ref().unwrap().roles[0]
            .pending_build
            .as_ref()
            .unwrap()
            .output_root()
            .to_owned();
        assert_ne!(solution_root, role_root);
        let report = Engine::new(
            *ready,
            JobRequest::Test {
                comparator: cpforge::config::Comparator::Tokens,
            },
        )
        .unwrap()
        .run(
            &mut SupervisorExecutor::start().unwrap(),
            &mut MemoryPersistence::default(),
            &mut NoHooks,
        )
        .await;
        assert_eq!(
            report.data().build.as_ref().unwrap().result,
            BuildResult::Succeeded
        );
        assert_eq!(
            report.data().details.roles[0]
                .build
                .as_ref()
                .unwrap()
                .result,
            RoleBuildResult::Succeeded
        );
    });
}

#[test]
fn cancellation_during_role_build_cancels_later_builds_and_tests() {
    run_test(move || async move {
        let f = Fixture::new();
        f.write("problem/main.py", "");
        f.write("problem/checker.cpp", "int main() {}\n");
        f.write("problem/validator.cpp", "int main() {}\n");
        // A marker written by the producer makes cancellation deterministic.
        let user = fs::read_to_string(f.path("user.toml")).unwrap();
        f.write("user.toml", &format!("{user}\n[languages.judgecpp]\nextensions = ['jcpp']\nsources = ['checker.cpp']\nartifacts = {{ file = 'main' }}\n[languages.judgecpp.build]\nshell = {{ interpreter = ['sh', '-c'], script = 'printf started > role-started; exec sleep 60' }}\n[languages.judgecpp.run]\nargv = ['{{artifact}}']\n"));
        let check = ROLES.replacen("language = 'cpp'", "language = 'judgecpp'", 1);
        let ready = ready(&f, "python", &check, true);
        let marker = ready.dir.join("r00000").join("cwd").join("role-started");
        let engine = Engine::new(
            *ready,
            JobRequest::Test {
                comparator: cpforge::config::Comparator::Tokens,
            },
        )
        .unwrap();
        let cancel = engine.cancellation();
        let request = async {
            tokio::time::timeout(Duration::from_secs(20), async {
                while !marker.exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            cancel.request_cancel(CancelSource::Api);
        };
        let mut executor = SupervisorExecutor::start().unwrap();
        let mut persistence = MemoryPersistence::default();
        let mut hooks = NoHooks;
        let (report, ()) = tokio::join!(
            engine.run(&mut executor, &mut persistence, &mut hooks),
            request
        );
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        for role in &report.data().details.roles {
            assert_eq!(
                role.build.as_ref().unwrap().result,
                RoleBuildResult::Cancelled
            );
        }
        assert!(report.data().executions[0].run.is_some());
        assert!(
            report.data().executions[1..]
                .iter()
                .all(|e| e.result.coverage
                    == Coverage::Skipped {
                        reason: SkipReason::Cancelled
                    })
        );
        wire::encode_report(&report).unwrap();
    });
}

#[test]
fn prebuilt_role_records_declared_origin_without_a_build_or_second_reservation() {
    run_test(move || async move {
        let f = Fixture::new();
        f.write("problem/main.py", "");
        f.write("problem/checker", "#!/bin/sh\nexit 0\n");
        let ready = ready(
            &f,
            "python",
            "[check]\nkind = 'checker'\nchecker = { program = 'checker', profile = 'testlib' }\n",
            false,
        );
        let budget = ready.budget.clone();
        let held = ready.judging.as_ref().unwrap().report_reservation.bytes();
        assert_eq!(budget.usage().reserved, held);
        // Occupy all other headroom. A second reservation would refuse this job.
        let usage = budget.usage();
        let rest = budget
            .reserve(budget.limit() - usage.retained - usage.reserved)
            .unwrap();
        let engine = Engine::new(
            *ready,
            JobRequest::Test {
                comparator: cpforge::config::Comparator::Tokens,
            },
        )
        .unwrap();
        assert!(engine.plan().is_empty());
        let mut persistence = ReservationPersistence::default();
        let report = engine
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut persistence,
                &mut NoHooks,
            )
            .await;
        assert_eq!(persistence.received, Some(held));
        assert_eq!(report.data().result.verdict, JobVerdict::NoTests);
        let role = &report.data().details.roles[0];
        assert_eq!(role.provenance, RoleProvenance::Declared);
        assert!(role.build.is_none());
        assert_eq!(role.capture.counts, [1, 0]);
        assert_eq!(report.data().details.warnings.len(), 1);
        // Report persistence receives the original reservation even at the cap.
        assert!(!matches!(
            report.persistence(),
            cpforge::jobs::engine::ReportPersistence::Refusal
        ));
        drop(rest);
        assert_eq!(budget.usage().reserved, 0);
        wire::encode_report(&report).unwrap();
    });
}

#[test]
fn python_roles_have_provenance_but_no_build_executions() {
    run_test(move || async move {
        let f = Fixture::new();
        f.write("problem/main.py", "");
        f.write("problem/checker.py", "");
        let ready = ready(
            &f,
            "python",
            "[check]\nkind = 'checker'\nchecker = { sources = ['checker.py'], language = 'python', profile = 'simple' }\n",
            false,
        );
        let engine = Engine::new(
            *ready,
            JobRequest::Test {
                comparator: cpforge::config::Comparator::Tokens,
            },
        )
        .unwrap();
        assert!(engine.plan().is_empty());
        let report = engine
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        let role = &report.data().details.roles[0];
        assert!(role.build.is_none());
        assert!(matches!(
            role.provenance,
            RoleProvenance::Built {
                header: cpforge::report::Header::NotApplicable
            }
        ));
        assert!(report.data().details.judge_fingerprint.is_some());
        wire::encode_report(&report).unwrap();
    });
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn cancellation_before_later_build_outweighs_a_previous_checker_failure() {
    run_test(move || async move {
        let f = Fixture::new();
        f.write("problem/main.py", "");
        f.write("problem/checker.cpp", "not C++");
        f.write("problem/validator.cpp", "int main() {}\n");
        let ready = ready(&f, "python", ROLES, true);
        let engine = Engine::new(
            *ready,
            JobRequest::Test {
                comparator: cpforge::config::Comparator::Tokens,
            },
        )
        .unwrap();
        let cancel = engine.cancellation();
        let engine = engine.observe(move |point| {
            if point == cpforge::jobs::engine::Point::BeforeAdmission(1) {
                cancel.request_cancel(CancelSource::Api);
            }
        });
        let report = engine
            .run(
                &mut SupervisorExecutor::start().unwrap(),
                &mut MemoryPersistence::default(),
                &mut NoHooks,
            )
            .await;
        assert_eq!(report.data().result.verdict, JobVerdict::Cancelled);
        assert!(matches!(
            report.data().details.roles[0]
                .build
                .as_ref()
                .unwrap()
                .result,
            RoleBuildResult::Failed { .. }
        ));
        assert_eq!(
            report.data().details.roles[1]
                .build
                .as_ref()
                .unwrap()
                .result,
            RoleBuildResult::Cancelled
        );
        assert!(
            report.data().executions[1..]
                .iter()
                .all(|e| e.result.coverage
                    == Coverage::Skipped {
                        reason: SkipReason::Cancelled
                    })
        );
    });
}
