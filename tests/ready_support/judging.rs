//! Planning and freezing only: injected executable probes never start a process.
use super::ready_support::{Fixture, Tools};
use cpforge::config::{CheckConfig, ConfigRequest, Environment, Platform, TargetPlan, load_config};
use cpforge::exec::{Resolution, ResolvedCommand};
use cpforge::jobs::{CaptureCheck, ReadyError, engine, plan, prepare_job};
use cpforge::report::{RoleName, wire};
use cpforge::storage::inputs::{CaptureError, FreezeInputsError, SourceRole};
use cpforge::storage::prune::PrunePolicy;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

fn config(f: &Fixture, language: &str, check: &str) -> (TargetPlan, Box<CheckConfig>) {
    f.target(language, check);
    let loaded = load_config(&ConfigRequest {
        environment: Environment::default(),
        platform: Platform::host(),
        current_dir: f.0.clone(),
        config_path: Some(f.path("user.toml")),
        workspace_root: Some(f.0.clone()),
        problem_file: Some(f.path("problem/problem.toml")),
        ..ConfigRequest::default()
    });
    (
        loaded.target().unwrap(),
        Box::new(loaded.judging().unwrap().unwrap().clone()),
    )
}

fn fixture() -> Fixture {
    let f = Fixture::new();
    f.write("problem/main.py", "solution");
    f.write("problem/main.cpp", "solution");
    f.write("problem/checker.py", "checker");
    f.write("problem/checker.cpp", "checker");
    f.write("problem/validator.py", "validator");
    f.write("problem/validator.exe", "prebuilt");
    f.write("problem/data/words", "asset");
    f.write("problem/helper.h", "support");
    f.write("problem/tests/a.in", "input");
    f.write("problem/tests/a.ans", "answer");
    f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\nanswer = 'tests/a.ans'\norigin = 'custom'\nenabled = true\n");
    f
}

#[test]
fn execution_counts_and_order_cover_every_batch_role_mix() {
    let f = fixture();
    for language in ["cpp", "python"] {
        for (check, role_builds, per_test, checker, validator) in [
            ("", 0, 1, false, false),
            (
                "[check]\nkind = 'checker'\nchecker = { program = 'validator.exe', profile = 'simple' }",
                0,
                2,
                true,
                false,
            ),
            (
                "[check]\nvalidator = { program = 'validator.exe', profile = 'testlib' }",
                0,
                2,
                false,
                true,
            ),
            (
                "[check]\nkind = 'checker'\nchecker = { sources = ['checker.cpp'], language = 'cpp', profile = 'testlib', assets = ['data/words'] }",
                1,
                2,
                true,
                false,
            ),
            (
                "[check]\nvalidator = { sources = ['validator.py'], language = 'python', profile = 'simple' }",
                0,
                2,
                false,
                true,
            ),
            (
                "[check]\nkind = 'checker'\nchecker = { sources = ['checker.cpp'], language = 'cpp', profile = 'simple' }\nvalidator = { sources = ['checker.cpp'], language = 'cpp', profile = 'simple', assets = ['data/words'] }",
                2,
                3,
                true,
                true,
            ),
        ] {
            let (target, check) = config(&f, language, check);
            let job = f.path("prospective");
            let plan = plan::plan_test_job(&target, Some(&check), 3, 2, true, &job).unwrap();
            let builds = usize::from(target.build.is_some()) + role_builds;
            assert_eq!(plan.executions.len(), builds + 3 * per_test + 2);
            assert_eq!(plan.shape.validator, validator);
            assert_eq!(plan.shape.judges, u8::from(checker));
            assert!(plan.shape.output_file);
            assert_eq!(
                plan.report_bytes,
                wire::report_reservation(plan.executions.len(), 3, 0, Some(plan.shape)).unwrap()
            );
            let expected: Vec<_> = [
                validator.then_some(plan::ExecutionRole::Judge(RoleName::Validator)),
                Some(plan::ExecutionRole::Solution),
                checker.then_some(plan::ExecutionRole::Judge(RoleName::Checker)),
            ]
            .into_iter()
            .flatten()
            .collect();
            for test in 0..3 {
                let actual: Vec<_> = plan
                    .executions
                    .iter()
                    .filter(|e| e.test == Some(test))
                    .map(|e| e.role)
                    .collect();
                assert_eq!(actual, expected);
            }
            let role_order: Vec<_> = plan
                .executions
                .iter()
                .filter_map(|e| match e.role {
                    plan::ExecutionRole::RoleBuild(role) => Some(role),
                    _ => None,
                })
                .collect();
            assert_eq!(
                role_order,
                [RoleName::Checker, RoleName::Validator][..role_builds]
            );
            assert!(!job.exists());
        }
    }
}

#[test]
fn caps_are_phase_three_exit_seven_in_required_precedence() {
    let f = fixture();
    let (target, check) = config(
        &f,
        "python",
        "[check]\nkind = 'checker'\nchecker = { program = 'validator.exe', profile = 'simple' }",
    );
    let job = f.path("never-allocated");
    let error = plan::plan_test_job(&target, Some(&check), usize::MAX, 0, false, &job).unwrap_err();
    assert_eq!(error, plan::PlanError::ExecutionCap);
    assert_eq!(error.exit_code(), 7);
    assert_eq!(
        error.to_string(),
        "planned runs exceed the 10 000 execution cap"
    );
    assert_eq!(
        plan::plan_test_job(&target, Some(&check), 1001, 0, false, &job).unwrap_err(),
        plan::PlanError::JudgedTestCap
    );
    let at_cap = plan::plan_test_job(&target, Some(&check), 1000, 8000, false, &job).unwrap();
    assert_eq!(at_cap.executions.len(), 10_000);
    assert_eq!(
        plan::plan_test_job(&target, Some(&check), 1000, 8001, false, &job).unwrap_err(),
        plan::PlanError::ExecutionCap
    );
    let (target, compare) = config(&f, "python", "");
    assert!(plan::plan_test_job(&target, Some(&compare), 1001, 0, false, &job).is_ok());
    // Compare-only bounds equal the existing count and comparison-only reservation.
    for check in [None, Some(compare.as_ref())] {
        let bounds = plan::validate_test_plan(&target, check, 1001, 3, false).unwrap();
        assert_eq!(
            bounds.planned,
            1001 + usize::from(target.build.is_some()) + 3
        );
        assert_eq!(
            bounds.report_bytes,
            wire::report_reservation(bounds.planned, 1001, 0, None).unwrap()
        );
    }
    assert!(plan::judging_supported(Some(&compare)));
    assert!(!plan::judging_supported(Some(&check)));
    assert!(!job.exists());
    assert!(!f.path(".cpforge/runs").exists());
    assert!(
        fs::read_to_string(f.path(".cpforge/job-seq"))
            .unwrap()
            .contains("next = '0'")
    );
}

#[test]
fn interactors_are_declared_but_planning_remains_refused() {
    let f = fixture();
    let (target, check) = config(
        &f,
        "python",
        "[check]\nkind = 'interactive'\nchecker = { program = 'validator.exe', profile = 'simple' }\n[check.interactive]\nchecking = 'checker'\ninteractor = { sources = ['checker.cpp'], language = 'cpp', profile = 'testlib' }",
    );
    let job = f.path("never-allocated");
    assert!(!plan::judging_supported(Some(&check)));
    assert_eq!(
        plan::plan_test_job(&target, Some(&check), 1, 0, false, &job).unwrap_err(),
        plan::PlanError::InteractorUnsupported
    );
    // Four runs per test once a validator is declared, plus the role build.
    let mut with_validator = check.clone();
    with_validator.validator = with_validator.checker.clone();
    assert_eq!(
        plan::plan_test_job(&target, Some(&with_validator), 1000, 5999, false, &job).unwrap_err(),
        plan::PlanError::ReportCap
    );
    assert_eq!(
        plan::plan_test_job(&target, Some(&with_validator), 2500, 0, false, &job).unwrap_err(),
        plan::PlanError::ExecutionCap
    );
    assert!(!job.exists());
}

#[test]
fn role_capture_hashes_copies_and_freezes_every_declared_file() {
    let f = fixture();
    let (target, check) = config(
        &f,
        "python",
        "[check]\nkind = 'checker'\nchecker = { sources = ['checker.cpp'], support_files = ['helper.h'], language = 'cpp', profile = 'testlib', assets = ['data/words'] }\nvalidator = { program = 'validator.exe', profile = 'simple' }",
    );
    let job = f.job(1 << 30, PrunePolicy::default());
    let dir = job.dir.clone();
    let frozen = job
        .inputs()
        .freeze_judged(
            &f.store(),
            &["a".parse().unwrap()],
            &target,
            &check,
            &f.path("problem"),
        )
        .unwrap();
    let roles = &frozen.judging().unwrap().roles;
    assert_eq!(roles.len(), 2);
    let files = roles[0].files.files();
    assert_eq!(
        files.iter().map(|f| f.role).collect::<Vec<_>>(),
        [SourceRole::Source, SourceRole::Support, SourceRole::Asset]
    );
    for (file, (name, bytes)) in files.iter().zip([
        ("checker.cpp", b"checker".as_slice()),
        ("helper.h", b"support".as_slice()),
        ("data/words", b"asset".as_slice()),
    ]) {
        assert_eq!(file.copy_path, dir.join("inputs/roles/checker").join(name));
        assert_eq!(file.len, bytes.len() as u64);
        assert_eq!(file.sha256, <[u8; 32]>::from(Sha256::digest(bytes)));
        assert_eq!(fs::read(&file.copy_path).unwrap(), bytes);
        fs::remove_file(f.path(&format!("problem/{name}"))).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&roles[1].files.files()[0].copy_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    fs::remove_file(f.path("problem/validator.exe")).unwrap();
    let ready = prepare_job(job, frozen, &target, f.inputs(&Tools)).unwrap();
    let judging = ready.judging.as_ref().unwrap();
    assert!(judging.judge_fingerprint.is_some());
    assert_eq!(
        judging.report_reservation.bytes(),
        judging.plan.report_bytes
    );
    assert_eq!(ready.budget.usage().reserved, judging.plan.report_bytes);
    assert!(
        judging
            .roles
            .iter()
            .all(|r| r.capture_check == CaptureCheck::Checked)
    );
    assert_eq!(
        judging.roles[0].outputs.as_ref().unwrap().path(),
        dir.join("builds/r00000")
    );
    assert!(matches!(
        *judging.roles[0].runs[0].stage,
        Resolution::AwaitingProducer(_)
    ));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("inputs/capture.json")).unwrap()).unwrap();
    assert_eq!(capture["roles"][0]["role"], "checker");
    assert_eq!(capture["roles"][0]["files"].as_array().unwrap().len(), 3);
    assert_eq!(capture["roles"][1]["role"], "validator");
    assert!(matches!(
        engine::Engine::new(
            ready,
            engine::JobRequest::Test {
                comparator: target.compare.value
            }
        ),
        Err(engine::PlanError::JudgingUnsupported)
    ));
}

#[test]
fn profile_arguments_and_staging_use_each_runs_private_directory() {
    for profile in ["simple", "testlib"] {
        let f = fixture();
        let (target, check) = config(
            &f,
            "python",
            &format!(
                "[check]\nkind = 'checker'\nchecker = {{ sources = ['checker.py'], language = 'python', profile = '{profile}', assets = ['data/words'], deadline_s = '500ms', stdout_cap = 23, stderr_cap = 24 }}\nvalidator = {{ program = 'validator.exe', profile = '{profile}' }}"
            ),
        );
        let job = f.job(1 << 30, PrunePolicy::default());
        let dir = job.dir.clone();
        let frozen = job
            .inputs()
            .freeze_judged(
                &f.store(),
                &["a".parse().unwrap()],
                &target,
                &check,
                &f.path("problem"),
            )
            .unwrap();
        let mut inputs = f.inputs(&Tools);
        inputs.judge_options.output_file = true;
        let ready = prepare_job(job, frozen, &target, inputs).unwrap();
        let judging = ready.judging.as_ref().unwrap();
        assert!(judging.plan.shape.output_file);
        let checker = &judging.roles[0];
        let validator = &judging.roles[1];
        assert!(checker.outputs.is_none());
        assert!(checker.build_stage.is_none());
        let run = &checker.runs[0];
        let cwd = dir.join("r00002").join("cwd");
        let Resolution::Resolved(stage) = run.stage.as_ref() else {
            panic!("build-free role")
        };
        let ResolvedCommand::Argv { args, .. } = &stage.command else {
            panic!("argv")
        };
        assert_eq!(stage.cwd, cwd);
        assert_eq!(stage.deadline.milliseconds.value, 500);
        assert_eq!(stage.stdout.bytes.value, 23);
        assert_eq!(stage.stderr.bytes.value, 24);
        let n = if profile == "testlib" { 5 } else { 3 };
        let mut expected = vec![
            cwd.join("input").into_os_string(),
            cwd.join("output").into_os_string(),
            cwd.join("answer").into_os_string(),
        ];
        if profile == "testlib" {
            expected.extend([cwd.join("report").into_os_string(), "-appes".into()]);
        }
        assert_eq!(&args[args.len() - n..], expected);
        assert_eq!(
            run.staged[0].source,
            Some(dir.join("inputs/roles/checker/data/words"))
        );
        assert_eq!(run.staged[0].destination, cwd.join("data/words"));
        assert_eq!(run.staged[2].source, Some(dir.join("r00001/out/output")));
        assert!(run.stdin.is_none());
        let validator = &validator.runs[0];
        let Resolution::Resolved(stage) = validator.stage.as_ref() else {
            panic!()
        };
        let ResolvedCommand::Argv { program, args } = &stage.command else {
            panic!()
        };
        assert_eq!(
            program.path,
            dir.join("inputs/roles/validator/validator.exe")
        );
        assert_eq!(validator.stdin, Some(dir.join("r00000/cwd/input")));
        assert_eq!(args.len(), usize::from(profile == "simple"));
        for e in &judging.plan.executions {
            assert_eq!(e.layout.judge_report, e.layout.dir.join("judge/report"));
            assert_eq!(e.layout.judge_tout, e.layout.dir.join("judge/tout"));
            assert_eq!(e.layout.transcript, e.layout.dir.join("transcript"));
            assert!(!e.layout.cwd.exists());
        }
    }
}

#[test]
fn capture_failures_are_terminal_and_preserve_existing_destinations() {
    for (path, collision) in [
        ("missing", false),
        ("data", false),
        (".cpforge/cache/file", false),
        ("validator.exe", true),
    ] {
        let f = fixture();
        let (target, check) = config(
            &f,
            "python",
            &format!("[check]\nvalidator = {{ program = '{path}', profile = 'simple' }}"),
        );
        let job = f.job(1 << 30, PrunePolicy::default());
        let destination = job.dir.join("inputs/roles/validator").join(path);
        if collision {
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::write(&destination, b"keep").unwrap();
        }
        let error = job
            .inputs()
            .freeze_judged(&f.store(), &[], &target, &check, &f.path("problem"))
            .unwrap_err();
        assert!(matches!(error, FreezeInputsError::Sources(_)));
        if collision {
            assert!(matches!(
                error,
                FreezeInputsError::Sources(CaptureError::SourceCollision { .. })
            ));
            assert_eq!(fs::read(destination).unwrap(), b"keep");
        }
        assert!(
            job.inputs()
                .freeze_sources(&[], &f.path("problem"))
                .is_err()
        );
    }
}

#[test]
fn role_aliases_and_role_capture_budget_are_checked() {
    let f = fixture();
    fs::hard_link(f.path("problem/checker.py"), f.path("problem/alias.py")).unwrap();
    let (target, check) = config(
        &f,
        "python",
        "[check]\nvalidator = { sources = ['checker.py'], language = 'python', profile = 'simple', assets = ['alias.py'] }",
    );
    let job = f.job(1 << 30, PrunePolicy::default());
    assert!(matches!(
        job.inputs()
            .freeze_judged(&f.store(), &[], &target, &check, &f.path("problem")),
        Err(FreezeInputsError::Sources(CaptureError::SameFile { .. }))
    ));
    assert!(!job.dir.join("inputs/roles/validator/checker.py").exists());
    drop(job);
    let (target, check) = config(
        &f,
        "python",
        "[check]\nvalidator = { program = 'validator.exe', profile = 'simple', assets = ['data/words'] }",
    );
    let job = f.job(4096, PrunePolicy::default());
    fs::write(f.path("problem/data/words"), vec![b'x'; 8192]).unwrap();
    assert!(
        job.inputs()
            .freeze_judged(&f.store(), &[], &target, &check, &f.path("problem"))
            .is_err()
    );
    assert!(job.budget.usage().retained + job.budget.usage().reserved <= job.budget.limit());
}

#[cfg(unix)]
#[test]
fn role_capture_refuses_links_in_files_and_ancestors() {
    use std::os::unix::fs::symlink;
    let f = fixture();
    symlink(f.path("problem/validator.exe"), f.path("problem/linked")).unwrap();
    symlink(f.path("problem/data"), f.path("problem/linked-dir")).unwrap();
    for path in ["linked", "linked-dir/words"] {
        let (target, check) = config(
            &f,
            "python",
            &format!("[check]\nvalidator = {{ program = '{path}', profile = 'simple' }}"),
        );
        let job = f.job(1 << 30, PrunePolicy::default());
        assert!(
            job.inputs()
                .freeze_judged(&f.store(), &[], &target, &check, &f.path("problem"))
                .is_err()
        );
        assert!(!job.dir.join("inputs/roles/validator").exists());
    }
}

#[test]
fn role_paths_cannot_leave_the_problem() {
    let f = fixture();
    // A drive prefix is only non-relative on Windows (config §6.2: "relative").
    let mut paths = vec!["../outside", "/absolute", "data/../validator.exe"];
    if cfg!(windows) {
        paths.push("C:/outside");
    }
    for path in paths {
        f.write("problem/problem.toml", &format!("schema = 1\n[problem]\nid = 'local:p'\n[config]\nlanguage = 'python'\n[check]\nvalidator = {{ program = '{path}', profile = 'simple' }}"));
        let loaded = load_config(&ConfigRequest {
            current_dir: f.0.clone(),
            config_path: Some(f.path("user.toml")),
            workspace_root: Some(f.0.clone()),
            problem_file: Some(f.path("problem/problem.toml")),
            ..ConfigRequest::default()
        });
        assert!(loaded.judging().is_err(), "{path}");
    }
    assert!(!f.path(".cpforge/runs").exists());
}

#[test]
fn absent_answers_are_intentionally_empty_only_under_answer_none() {
    for answer in ["none", "file"] {
        let f = fixture();
        f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'a'\ninput = 'tests/a.in'\norigin = 'custom'\nenabled = true\n");
        let (target, check) = config(
            &f,
            "python",
            &format!(
                "[check]\nkind = 'checker'\nanswer = '{answer}'\nchecker = {{ program = 'validator.exe', profile = 'simple' }}"
            ),
        );
        let job = f.job(1 << 30, PrunePolicy::default());
        let frozen = job
            .inputs()
            .freeze_judged(
                &f.store(),
                &["a".parse().unwrap()],
                &target,
                &check,
                &f.path("problem"),
            )
            .unwrap();
        let result = prepare_job(job, frozen, &target, f.inputs(&Tools));
        if answer == "none" {
            let ready = result.unwrap();
            assert!(ready.judging.as_ref().unwrap().roles[0].runs[0].staged.iter().any(|s| s.destination.ends_with(Path::new("cwd/answer")) && s.source.is_none()));
        } else {
            assert!(
                matches!(&result, Err(ReadyError::Judging(m)) if m.ends_with(": a")),
                "{result:?}"
            );
        }
    }
}

#[test]
fn judge_fingerprint_tracks_role_bytes_and_ignores_display() {
    let f = fixture();
    let mut hashes = Vec::new();
    for (display, bytes) in [("cpf", "asset"), ("judge-cf", "asset"), ("cpf", "changed")] {
        f.write("problem/data/words", bytes);
        let (target, check) = config(
            &f,
            "python",
            &format!(
                "[check]\ndisplay = '{display}'\nvalidator = {{ program = 'validator.exe', profile = 'simple', assets = ['data/words'] }}"
            ),
        );
        let job = f.job(1 << 30, PrunePolicy::default());
        let frozen = job
            .inputs()
            .freeze_judged(&f.store(), &[], &target, &check, &f.path("problem"))
            .unwrap();
        let ready = prepare_job(job, frozen, &target, f.inputs(&Tools)).unwrap();
        let judging = ready.judging.as_ref().unwrap();
        use cpforge::storage::facts::fingerprint::{
            JudgeFingerprintFile, JudgeFingerprintOrigin, JudgeFingerprintRole, judge_fingerprint,
        };
        let role = &judging.roles[0];
        let expected = JudgeFingerprintRole {
            profile: role.captured.spec.profile.value,
            files: role
                .captured
                .files
                .files()
                .iter()
                .map(|file| JudgeFingerprintFile {
                    path: file.problem_path.components().iter().collect(),
                    copied_bytes: fs::read(&file.copy_path).unwrap(),
                })
                .collect(),
            origin: JudgeFingerprintOrigin::Prebuilt {
                program: "validator.exe".into(),
                assets: vec!["data/words".into()],
            },
            deadline_us: None,
            stdout_cap: None,
            stderr_cap: None,
            seed: cpforge::report::SeedMode::None,
        };
        assert_eq!(
            judging.judge_fingerprint,
            judge_fingerprint(&check.report_config(), &[None, Some(expected), None]).unwrap()
        );
        hashes.push(judging.judge_fingerprint.unwrap());
    }
    assert_eq!(hashes[0], hashes[1]);
    assert_ne!(hashes[0], hashes[2]);
}

#[test]
fn role_stages_preserve_language_artifacts_and_role_entry() {
    let f = fixture();
    let (target, check) = config(
        &f,
        "cpp",
        "[check]\nvalidator = { sources = ['validator.py'], language = 'java', entry = 'judge.Validator', profile = 'simple' }",
    );
    let job = f.job(1 << 30, PrunePolicy::default());
    let dir = job.dir.clone();
    let frozen = job
        .inputs()
        .freeze_judged(&f.store(), &[], &target, &check, &f.path("problem"))
        .unwrap();
    let ready = prepare_job(job, frozen, &target, f.inputs(&Tools)).unwrap();
    let role = &ready.judging.as_ref().unwrap().roles[0];
    assert_eq!(
        role.outputs.as_ref().unwrap().path(),
        dir.join("builds/r00001")
    );
    let Some(cpforge::config::Artifacts::Dir { required }) = &role.target.artifacts else {
        panic!()
    };
    assert_eq!(required[0].value, "judge/Validator.class");
    assert_eq!(role.target.entry.as_ref().unwrap().value, "judge.Validator");
}

#[test]
fn report_headroom_is_reserved_separately_and_budget_failure_starts_nothing() {
    let f = fixture();
    let (target, check) = config(
        &f,
        "python",
        "[check]\nvalidator = { program = 'validator.exe', profile = 'simple' }",
    );
    let job = f.job(8192, PrunePolicy::default());
    let dir = job.dir.clone();
    let budget = job.budget.clone();
    let frozen = job
        .inputs()
        .freeze_judged(&f.store(), &[], &target, &check, &f.path("problem"))
        .unwrap();
    assert!(matches!(
        prepare_job(job, frozen, &target, f.inputs(&Tools)),
        Err(ReadyError::Storage(_))
    ));
    assert_eq!(budget.usage().reserved, 0);
    assert!(!dir.join("r00000").exists());
}
