//! Portable managed readiness and composed storage lifecycle tests.
mod ready_support;

use cpforge::build::{ExpandedArguments, PlannedTarget};
use cpforge::config::{Cwd, Placeholder, StageCommand, Token};
use cpforge::jobs::{CaptureCheck, CaptureCheckReason, ReadyError, prepare_build_job, prepare_job};
use cpforge::storage::inputs::CapturedSource;
use cpforge::storage::job::{ProbeOutcome, probe};
use cpforge::storage::prune::PrunePolicy;
use ready_support::{Fixture, Tools, released};
use std::fs;

#[test]
fn managed_build_and_build_free_bind_only_copies_and_keep_order() {
    for language in ["cpp", "python"] {
        let f = Fixture::new();
        f.write("problem/first", "old first");
        f.write("problem/second", "old second");
        f.write("problem/helper", "support");
        if language == "python" {
            f.write(
                "user.toml",
                "schema = 1\n[languages.python.run]\nargv = ['python3', '{sources}']\n",
            );
        }
        let target = f.target(
            language,
            "sources = ['second', 'first']\nsupport_files = ['helper']\n",
        );
        // Build-only does not select the unrelated broken test.
        f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'missing'\ninput = 'tests/missing.in'\norigin = 'custom'\nenabled = true\n");
        let registered = f.job(1 << 20, PrunePolicy::default());
        let dir = registered.dir.clone();
        let ready = prepare_build_job(registered, &target, f.inputs(&Tools)).unwrap();
        assert_eq!(ready.capture_check, CaptureCheck::Checked);
        assert!(ready.manifest.entries().is_empty());
        assert!(dir.join("builds").is_dir());
        assert!(matches!(probe(&dir), ProbeOutcome::Held));
        let copies = ready.sources.source_bindings();
        assert_eq!(
            copies,
            vec![
                dir.join("inputs").join("src").join("second"),
                dir.join("inputs").join("src").join("first")
            ]
        );
        assert_eq!(ready.sources.files().len(), 3);
        f.write("problem/first", "new first");
        f.write("problem/second", "new second");
        assert_eq!(fs::read(&copies[0]).unwrap(), b"old second");
        assert_eq!(fs::read(&copies[1]).unwrap(), b"old first");
        let args = match &ready.planned_target {
            PlannedTarget::Build(pending) => {
                assert!(pending.output_root().starts_with(dir.join("builds")));
                pending.expanded_arguments()
            }
            PlannedTarget::BuildFree(run) => run.expanded_arguments(),
        };
        let ExpandedArguments::Argv(args) = args else {
            panic!()
        };
        for path in copies {
            assert!(args.contains(&path.into_os_string()));
        }
        let owner = ready.owner.clone();
        drop(ready);
        f.prune();
        assert!(dir.exists());
        drop(owner);
        f.prune();
        assert!(!dir.exists());
    }
}

#[test]
fn foreign_frozen_inputs_and_changed_target_are_refused() {
    let f = Fixture::new();
    f.write("problem/main.cpp", "source");
    f.write("problem/other.cpp", "other");
    let target = f.target("cpp", "");
    let a = f.job(1 << 20, PrunePolicy::default());
    let frozen = a
        .inputs()
        .freeze(
            &f.store(),
            &[],
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    let b = f.job(1 << 20, PrunePolicy::default());
    let b_dir = b.dir.clone();
    assert!(matches!(
        prepare_job(b, frozen, &target, f.inputs(&Tools)),
        Err(ReadyError::WrongJob { .. })
    ));
    assert!(!b_dir.join("builds").exists());
    released(&b_dir);
    drop(a);

    let a = f.job(1 << 20, PrunePolicy::default());
    let frozen = a
        .inputs()
        .freeze(
            &f.store(),
            &[],
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    let different = f.target("cpp", "sources = ['other.cpp']\n");
    assert!(matches!(
        prepare_job(a, frozen, &different, f.inputs(&Tools)),
        Err(ReadyError::WrongTarget)
    ));
}

#[test]
fn source_consuming_stage_alone_is_guarded_and_failure_releases_registration() {
    let f = Fixture::new();
    f.write("problem/main.cpp", "source");
    let mut target = f.target("cpp", "");
    // A build-backed run can explicitly choose the problem directory.
    target.run.cwd.value = Cwd::Problem;
    let ready = prepare_build_job(
        f.job(1 << 20, PrunePolicy::default()),
        &target,
        f.inputs(&Tools),
    )
    .unwrap();
    assert_eq!(ready.capture_check, CaptureCheck::Checked);
    drop(ready);
    let build = target.build.as_mut().unwrap();
    let StageCommand::Argv(argv) = &mut build.command else {
        panic!()
    };
    argv[1].tokens = vec![Token::Placeholder(Placeholder::Workspace)];
    let registered = f.job(1 << 20, PrunePolicy::default());
    let dir = registered.dir.clone();
    assert!(matches!(
        prepare_build_job(registered, &target, f.inputs(&Tools)),
        Err(ReadyError::Guard(_))
    ));
    released(&dir);
    f.prune();
    assert!(!dir.exists());
}

#[test]
fn shell_args_are_guarded_but_script_is_unchecked() {
    let f = Fixture::new();
    f.write("problem/main.cpp", "source");
    f.write("user.toml", "schema = 1\n[languages.cpp.build]\nshell = { interpreter = ['sh', '-c'], script = 'cat {workspace}/main.cpp', args = ['{sources}'] }\n");
    let target = f.target("cpp", "");
    let ready = prepare_build_job(
        f.job(1 << 20, PrunePolicy::default()),
        &target,
        f.inputs(&Tools),
    )
    .unwrap();
    assert_eq!(
        ready.capture_check,
        CaptureCheck::Unchecked {
            reasons: vec![CaptureCheckReason::ShellStage]
        }
    );
    drop(ready);
    let mut target = target;
    target.build.as_mut().unwrap().cwd.value = Cwd::Workspace;
    assert!(matches!(
        prepare_build_job(
            f.job(1 << 20, PrunePolicy::default()),
            &target,
            f.inputs(&Tools)
        ),
        Err(ReadyError::Guard(_))
    ));
}

#[test]
fn interrupted_freeze_returns_no_ready_job_and_partial_storage_is_prunable() {
    let f = Fixture::new();
    f.write("problem/main.cpp", &"x".repeat(128 * 1024));
    let target = f.target("cpp", "");
    let registered = f.job(70 * 1024, PrunePolicy::default());
    let dir = registered.dir.clone();
    // Budget injection allows the first chunk, then refuses the second. The
    // consuming build-only path cannot return a partially usable ready value.
    let result = prepare_build_job(registered, &target, f.inputs(&Tools));
    assert!(matches!(result, Err(ReadyError::Freeze(_))));
    assert_eq!(
        fs::metadata(dir.join("inputs").join("src").join("main.cpp"))
            .unwrap()
            .len(),
        64 * 1024
    );
    released(&dir);
    f.prune();
    assert!(!dir.exists());
}

#[test]
fn ready_manifest_preserves_selected_tests_and_failed_freeze_cannot_be_bypassed() {
    let f = Fixture::new();
    f.write("problem/main.cpp", "source");
    f.write("problem/tests/one.in", "old input");
    f.write("problem/tests/one.ans", "");
    f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'one'\ninput = 'tests/one.in'\nanswer = 'tests/one.ans'\norigin = 'custom'\nenabled = true\n");
    let target = f.target("cpp", "");
    let registered = f.job(1 << 20, PrunePolicy::default());
    let ids = [cpforge::names::Identifier::new("one").unwrap()];
    let frozen = registered
        .inputs()
        .freeze(
            &f.store(),
            &ids,
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    let ready = prepare_job(registered, frozen, &target, f.inputs(&Tools)).unwrap();
    f.write("problem/tests/one.in", "new input");
    let entries = ready.manifest.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, ids[0]);
    assert_eq!(fs::read(&entries[0].input.path).unwrap(), b"old input");
    assert!(matches!(
        entries[0].answer,
        cpforge::tests_store::FrozenAnswer::Present { len: 0, .. }
    ));
    drop(ready);

    let registered = f.job(1 << 20, PrunePolicy::default());
    let frozen = registered
        .inputs()
        .freeze(
            &f.store(),
            &[],
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    // A later failed input operation poisons the registration even when the
    // caller retained an earlier complete FrozenInputs value.
    assert!(
        registered
            .inputs()
            .freeze(
                &f.store(),
                &[],
                &CapturedSource::from_target(&target),
                &f.path("problem")
            )
            .is_err()
    );
    let dir = registered.dir.clone();
    assert!(matches!(
        prepare_job(registered, frozen, &target, f.inputs(&Tools)),
        Err(ReadyError::FailedInputs)
    ));
    f.prune();
    assert!(!dir.exists());
}

#[test]
fn build_free_run_guard_failures_return_no_ready_job() {
    for placeholder in [Placeholder::ProblemDir, Placeholder::Workspace] {
        let f = Fixture::new();
        f.write("problem/main.py", "print('old')");
        let mut target = f.target("python", "");
        let StageCommand::Argv(argv) = &mut target.run.command else {
            panic!()
        };
        argv[1].tokens = vec![Token::Placeholder(placeholder)];
        let job = f.job(1 << 20, PrunePolicy::default());
        let dir = job.dir.clone();
        assert!(matches!(
            prepare_build_job(job, &target, f.inputs(&Tools)),
            Err(ReadyError::Guard(_))
        ));
        f.prune();
        assert!(!dir.exists());
    }
}

fn producer_success() -> cpforge::build::BuildExecutionOutcome {
    cpforge::build::BuildExecutionOutcome {
        termination: cpforge::supervisor::Termination::Exited { code: 0 },
        violations: Vec::new(),
        stop_trigger: None,
        enforced_at: None,
        cleanup: cpforge::supervisor::Cleanup::Confirmed,
        errors: Vec::new(),
        stdout: cpforge::report::StreamEvidence::default(),
        stderr: cpforge::report::StreamEvidence::default(),
    }
}

#[test]
fn build_backed_consumer_also_uses_captured_bindings() {
    let f = Fixture::new();
    f.write("problem/main.cpp", "old source");
    f.write(
        "user.toml",
        "schema = 1\n[languages.cpp.run]\nargv = ['echo', '{source}', '{sources}']\n",
    );
    let target = f.target("cpp", "");
    let ready = prepare_build_job(
        f.job(1 << 20, PrunePolicy::default()),
        &target,
        f.inputs(&Tools),
    )
    .unwrap();
    let copy = ready.sources.source_bindings().remove(0).into_os_string();
    f.write("problem/main.cpp", "edited");
    let PlannedTarget::Build(pending) = ready.planned_target else {
        panic!()
    };
    // Feed build planning a successful producer observation and a real regular output;
    // no program is executed by this filesystem-only capability test.
    for output in pending.required_outputs() {
        fs::write(output, b"artifact").unwrap();
    }
    let build = cpforge::build::finish_build(pending, producer_success()).unwrap();
    let cwd = ready.dir.join("r1").join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let stage = build.run(cwd).unwrap();
    let cpforge::exec::ResolvedCommand::Argv { args, .. } = stage.command else {
        panic!()
    };
    assert_eq!(args, vec![copy.clone(), copy]);
}

#[test]
fn managed_build_output_limit_failure_exposes_no_consumer_and_removes_outputs() {
    let f = Fixture::new();
    f.write("problem/main.cpp", "source");
    let target = f.target("cpp", "");
    let mut inputs = f.inputs(&Tools);
    inputs.build_bytes = 1;
    let ready = prepare_build_job(f.job(1 << 20, PrunePolicy::default()), &target, inputs).unwrap();
    let PlannedTarget::Build(pending) = ready.planned_target else {
        panic!()
    };
    let root = pending.output_root().to_owned();
    for output in pending.required_outputs() {
        fs::write(output, b"too large").unwrap();
    }
    let failure = cpforge::build::finish_build(pending, producer_success()).unwrap_err();
    assert!(matches!(
        *failure.kind,
        cpforge::build::BuildFailureKind::BuildOutputTooLarge
    ));
    let overflow = failure.over_cap.unwrap();
    assert_eq!(overflow.limit, 1);
    let removal = overflow.removal.unwrap();
    assert!(removal.errors.is_empty() && removal.skipped.is_empty());
    assert!(!root.exists());
}

#[test]
fn interruption_while_freezing_tests_never_reaches_source_capture_or_readiness() {
    let f = Fixture::new();
    f.write("problem/main.cpp", "source");
    f.write("problem/tests/one.in", &"x".repeat(128 * 1024));
    f.write("problem/tests.toml", "schema = 1\n[[test]]\nid = 'one'\ninput = 'tests/one.in'\norigin = 'custom'\nenabled = true\n");
    let target = f.target("cpp", "");
    let registered = f.job(70 * 1024, PrunePolicy::default());
    let dir = registered.dir.clone();
    let result = (|| {
        let frozen = registered
            .inputs()
            .freeze(
                &f.store(),
                &[cpforge::names::Identifier::new("one").unwrap()],
                &CapturedSource::from_target(&target),
                &f.path("problem"),
            )
            .map_err(ReadyError::Freeze)?;
        prepare_job(registered, frozen, &target, f.inputs(&Tools))
    })();
    assert!(matches!(result, Err(ReadyError::Freeze(_))));
    assert_eq!(
        fs::metadata(dir.join("inputs").join("one.in"))
            .unwrap()
            .len(),
        64 * 1024
    );
    assert!(!dir.join("inputs").join("src").exists());
    assert!(!dir.join("builds").exists());
    released(&dir);
    f.prune();
    assert!(!dir.exists());
}

#[test]
fn capture_manifest_is_the_last_ready_step_and_is_never_clobbered() {
    let f = Fixture::new();
    f.write("problem/main.py", "source\n");
    let target = f.target("python", "");
    let registered = f.job(1 << 20, PrunePolicy::default());
    let dir = registered.dir.clone();
    let frozen = registered
        .inputs()
        .freeze(
            &f.store(),
            &[],
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    fs::write(dir.join("inputs/capture.json"), b"existing manifest").unwrap();
    let result = prepare_job(registered, frozen, &target, f.inputs(&Tools));
    assert!(matches!(result, Err(ReadyError::Storage(_))));
    assert_eq!(
        fs::read(dir.join("inputs/capture.json")).unwrap(),
        b"existing manifest"
    );
}

#[test]
fn capture_manifest_budget_refusal_never_returns_a_ready_job() {
    let f = Fixture::new();
    f.write("problem/main.py", "source\n");
    let target = f.target("python", "");
    let registered = f.job(1 << 20, PrunePolicy::default());
    let frozen = registered
        .inputs()
        .freeze(
            &f.store(),
            &[],
            &CapturedSource::from_target(&target),
            &f.path("problem"),
        )
        .unwrap();
    let dir = registered.dir.clone();
    let budget = registered.budget.clone();
    let allowance = budget
        .reserve(budget.limit() - budget.usage().retained)
        .unwrap();
    let result = prepare_job(registered, frozen, &target, f.inputs(&Tools));
    assert!(matches!(result, Err(ReadyError::Storage(_))));
    assert!(!dir.join("inputs/capture.json").exists());
    drop(allowance);
}
