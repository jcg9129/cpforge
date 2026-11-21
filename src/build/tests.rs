use super::*;
use crate::config::{ConfigRequest, Environment, load_config};
use crate::exec::{Executable, NativeProbe, ProbeResult, ResolvedCommand};
use crate::supervisor::Stream;
use std::cell::RefCell;

#[derive(Debug)]
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("build-unit-{}", BuildId::generate().unwrap()));
        fs::create_dir_all(&path).unwrap();
        let directory = Self(fs::canonicalize(path).unwrap());
        for name in [
            "builds",
            "build-cwd",
            "tools",
            "inputs/src",
            "run-one",
            "run-two",
        ] {
            fs::create_dir_all(directory.path(name)).unwrap();
        }
        directory
    }

    /// Join per component: a canonical Windows root is a verbatim path, in
    /// which `/` is not a separator.
    fn path(&self, relative: &str) -> PathBuf {
        under(&self.0, relative)
    }

    fn target(&self, language: &str, extra: &str) -> TargetPlan {
        let config = self.path("config.toml");
        fs::write(&config, format!("schema = 1\n{extra}")).unwrap();
        load_config(&ConfigRequest {
            config_path: Some(config),
            current_dir: self.0.clone(),
            environment: Environment {
                home: Some(self.path("home")),
                ..Environment::default()
            },
            language: Some(language.into()),
            ..ConfigRequest::default()
        })
        .target()
        .unwrap()
    }

    fn inputs<'probe>(
        &self,
        target: &TargetPlan,
        limit: u64,
        probe: &'probe dyn Probe,
    ) -> PlanInputs<'probe> {
        let sources = target
            .sources
            .iter()
            .map(|source| {
                source
                    .value
                    .components()
                    .iter()
                    .fold(self.path("inputs/src"), |path, part| path.join(part))
            })
            .collect();
        PlanInputs {
            sources,
            dirs: InvocationDirs {
                run_cwd: self.path("build-cwd"),
                problem: Some(exec::ProblemBinding {
                    dir: self.0.clone(),
                    id: "p".into(),
                }),
                workspace: Some(self.0.clone()),
            },
            baseline: Baseline::from_entries(vec![
                ("PATH".into(), self.path("tools").into_os_string()),
                ("PATHEXT".into(), ".EXE".into()),
            ]),
            platform: Platform::host(),
            build_bytes: limit,
            probe,
        }
    }

    fn outputs(&self, id: &str) -> FreshBuildOutputs {
        allocate(
            &BuildArea::new(self.path("builds")).unwrap(),
            BuildId::new(id).unwrap(),
        )
        .unwrap()
    }

    fn pending<'probe>(
        &self,
        target: &TargetPlan,
        id: &str,
        limit: u64,
        probe: &'probe dyn Probe,
    ) -> PendingBuild<'probe> {
        match plan_target(
            target,
            self.inputs(target, limit, probe),
            Some(self.outputs(id)),
        )
        .unwrap()
        {
            PlannedTarget::Build(pending) => pending,
            PlannedTarget::BuildFree(_) => panic!("expected build"),
        }
    }
}

fn under(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(base.to_path_buf(), |path, part| path.join(part))
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Synthetic installed tools; generated programs still use native inspection.
#[derive(Debug)]
struct Tools {
    directory: PathBuf,
    missing: Vec<String>,
    calls: RefCell<Vec<PathBuf>>,
}

impl Tools {
    fn new(directory: &Directory) -> Self {
        Self {
            directory: directory.path("tools"),
            missing: Vec::new(),
            calls: RefCell::default(),
        }
    }
}

impl Probe for Tools {
    fn probe(&self, path: &Path) -> io::Result<ProbeResult> {
        self.calls.borrow_mut().push(path.into());
        if path.parent() == Some(self.directory.as_path()) {
            let name = path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase();
            return Ok(if self.missing.contains(&name) {
                ProbeResult::NotFound
            } else {
                ProbeResult::Regular(Executable::Yes)
            });
        }
        NativeProbe.probe(path)
    }
}

fn success() -> BuildExecutionOutcome {
    BuildExecutionOutcome {
        termination: Termination::Exited { code: 0 },
        violations: Vec::new(),
        stop_trigger: None,
        enforced_at: None,
        cleanup: crate::supervisor::Cleanup::Confirmed,
        errors: Vec::new(),
        stdout: StreamEvidence::default(),
        stderr: StreamEvidence::default(),
    }
}

fn file_artifact(pending: &PendingBuild<'_>, target: &TargetPlan) -> PathBuf {
    ArtifactLayout::new(
        pending.output_root(),
        target.artifacts.as_ref(),
        Platform::host(),
    )
    .unwrap()
    .file
    .unwrap()
}

fn write_executable(path: &Path) {
    fs::write(path, "fixture").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn program(stage: &ResolvedStage) -> &Path {
    match &stage.command {
        ResolvedCommand::Argv { program, .. } => &program.path,
        ResolvedCommand::Shell { interpreter, .. } => &interpreter.path,
    }
}

#[test]
fn artifact_layout_uses_target_platform_and_final_extension() {
    let directory = Directory::new();
    let target = directory.target("cpp", "");
    for (platform, root, expected) in [
        (Platform::Linux, "/build", "/build/main"),
        (Platform::Windows, "C:\\build", "C:\\build\\main.exe"),
    ] {
        let layout =
            ArtifactLayout::new(Path::new(root), target.artifacts.as_ref(), platform).unwrap();
        assert_eq!(layout.file, Some(PathBuf::from(expected)));
        assert_eq!(layout.required, vec![PathBuf::from(expected)]);
    }
    let mut artifacts = target.artifacts.unwrap();
    let Artifacts::File { name } = &mut artifacts else {
        panic!("file artifact")
    };
    name.value = "main.bin".into();
    let layout =
        ArtifactLayout::new(Path::new("C:\\build"), Some(&artifacts), Platform::Windows).unwrap();
    assert_eq!(layout.file.unwrap(), PathBuf::from("C:\\build\\main.bin"));
    let Artifacts::File { name } = &mut artifacts else {
        unreachable!()
    };
    name.value = "dir.with.dot\\main".into();
    let layout =
        ArtifactLayout::new(Path::new("C:\\build"), Some(&artifacts), Platform::Windows).unwrap();
    assert_eq!(
        layout.file.unwrap(),
        PathBuf::from("C:\\build\\dir.with.dot\\main.exe")
    );
    // Valid for Linux, but `..` for Windows: refused, never joined.
    let Artifacts::File { name } = &mut artifacts else {
        unreachable!()
    };
    name.value = "..\\escape".into();
    assert!(ArtifactLayout::new(Path::new("/build"), Some(&artifacts), Platform::Linux).is_ok());
    assert!(matches!(
        ArtifactLayout::new(Path::new("C:\\build"), Some(&artifacts), Platform::Windows),
        Err(PlanError::InvalidArtifactName(_))
    ));
}

#[test]
fn allocation_is_exclusive_for_files_and_directories() {
    let directory = Directory::new();
    let area = BuildArea::new(directory.path("builds")).unwrap();
    fs::write(area.path().join("file"), "existing").unwrap();
    assert!(allocate(&area, BuildId::new("file").unwrap()).is_err());
    let first = allocate(&area, BuildId::new("attempt").unwrap()).unwrap();
    assert_eq!(first.path(), area.path().join("attempt"));
    assert!(allocate(&area, first.id().clone()).is_err());
    for invalid in ["", ".", "..", "../old", "a/b", "a\\b", "C:old", "CON"] {
        assert!(BuildId::new(invalid).is_err(), "{invalid}");
    }
}

#[cfg(unix)]
#[test]
fn allocation_refuses_existing_links_including_dangling_links() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let area = BuildArea::new(directory.path("builds")).unwrap();
    for (id, destination) in [
        ("link", directory.path("tools")),
        ("dangling", directory.path("absent")),
    ] {
        symlink(destination, area.path().join(id)).unwrap();
        assert!(matches!(
            allocate(&area, BuildId::new(id).unwrap()).unwrap_err().kind,
            storage::ErrorKind::LinkInPath { .. }
        ));
    }
}

#[test]
fn missing_java_fails_preflight_before_a_build_is_available() {
    let directory = Directory::new();
    let target = directory.target("java", "");
    let mut probe = Tools::new(&directory);
    probe.missing.push("java".into());
    let error = plan_target(
        &target,
        directory.inputs(&target, 100, &probe),
        Some(directory.outputs("java")),
    )
    .unwrap_err();
    assert!(
        error
            .errors
            .iter()
            .any(|error| matches!(error, PlanError::Resolve(errors)
        if errors.errors.iter().any(|error| error.stage == crate::config::StageKind::Run)))
    );
    assert!(
        probe
            .calls
            .borrow()
            .iter()
            .any(|path| path.file_stem() == Some(OsStr::new("javac")))
    );
    assert!(
        probe
            .calls
            .borrow()
            .iter()
            .any(|path| path.file_stem() == Some(OsStr::new("java")))
    );
}

#[test]
fn planning_collects_both_stage_failures_and_preserves_source_bindings() {
    let directory = Directory::new();
    let target = directory.target("java", "[languages.java]\nsources = ['Z.java', 'A.java']\n");
    let mut probe = Tools::new(&directory);
    probe.missing = vec!["java".into(), "javac".into()];
    let errors = plan_target(
        &target,
        directory.inputs(&target, 100, &probe),
        Some(directory.outputs("missing")),
    )
    .unwrap_err();
    assert_eq!(
        errors
            .errors
            .iter()
            .filter(|error| matches!(error, PlanError::Resolve(_)))
            .count(),
        2
    );
    probe.missing.clear();
    let mut inputs = directory.inputs(&target, 100, &probe);
    inputs.sources = vec![
        directory.path("captured/Z.java"),
        directory.path("captured/A.java"),
    ];
    let pending =
        match plan_target(&target, inputs.clone(), Some(directory.outputs("ordered"))).unwrap() {
            PlannedTarget::Build(pending) => pending,
            PlannedTarget::BuildFree(_) => panic!("expected build"),
        };
    let ExpandedArguments::Argv(args) = pending.expanded_arguments() else {
        panic!("argv")
    };
    assert_eq!(
        &args[args.len() - 2..],
        inputs
            .sources
            .iter()
            .map(|path| path.as_os_str().to_owned())
            .collect::<Vec<_>>()
    );
    inputs.sources.pop();
    let errors = plan_target(&target, inputs, Some(directory.outputs("short"))).unwrap_err();
    assert!(errors.errors.iter().any(|error| matches!(
        error,
        PlanError::SourceCount {
            expected: 2,
            supplied: 1
        }
    )));
}

#[test]
fn exit_zero_requires_the_declared_file() {
    let directory = Directory::new();
    let target = directory.target("cpp", "");
    let probe = Tools::new(&directory);
    let pending = directory.pending(&target, "missing", 100, &probe);
    let artifact = file_artifact(&pending, &target);
    let error = finish_build(pending, success()).unwrap_err();
    assert!(matches!(*error.kind, BuildFailureKind::OutputMissing(path) if path == artifact));
}

#[test]
fn declared_directory_is_not_a_regular_output() {
    let directory = Directory::new();
    let target = directory.target("cpp", "");
    let probe = Tools::new(&directory);
    let pending = directory.pending(&target, "directory", 100, &probe);
    fs::create_dir(file_artifact(&pending, &target)).unwrap();
    assert!(matches!(
        *finish_build(pending, success()).unwrap_err().kind,
        BuildFailureKind::OutputNotRegular(_)
    ));
}

#[cfg(unix)]
#[test]
fn linked_artifact_and_required_class_through_link_are_refused() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let probe = Tools::new(&directory);
    let cpp = directory.target("cpp", "");
    let pending = directory.pending(&cpp, "linked-file", 100, &probe);
    write_executable(&directory.path("outside"));
    symlink(directory.path("outside"), file_artifact(&pending, &cpp)).unwrap();
    assert!(matches!(
        *finish_build(pending, success()).unwrap_err().kind,
        BuildFailureKind::OutputThroughLink(_)
    ));

    let java = directory.target("java", "[languages.java]\nentry = 'a.b.Grader'\n");
    let pending = directory.pending(&java, "linked-class", 1, &probe);
    let root = pending.output_root().to_owned();
    fs::write(root.join("auxiliary"), "oversized").unwrap();
    fs::create_dir_all(directory.path("classes/b")).unwrap();
    fs::write(directory.path("classes/b/Grader.class"), "class").unwrap();
    symlink(directory.path("classes"), pending.output_root().join("a")).unwrap();
    let error = finish_build(pending, success()).unwrap_err();
    assert!(matches!(
        error.kind.as_ref(),
        BuildFailureKind::OutputThroughLink(_)
    ));
    assert_eq!(
        reported_build(RunClass::ExitedZero, Err(&error)),
        report::BuildResult::Failed {
            cause: report::BuildCause::MissingOutput {
                names: vec![under(Path::new(""), "a/b/Grader.class").into_os_string()],
                names_dropped: 0,
            },
        }
    );
    let removal = error.over_cap.as_ref().unwrap().removal.as_ref().unwrap();
    assert!(removal.errors.is_empty() && removal.skipped.is_empty());
    assert!(!root.exists());
    assert!(directory.path("classes/b/Grader.class").is_file());
}

#[test]
fn zero_exit_does_not_override_timeout_output_limit_or_supervisor_errors() {
    let directory = Directory::new();
    let target = directory.target("cpp", "");
    let probe = Tools::new(&directory);
    let mut outcomes = Vec::new();
    let mut timeout = success();
    timeout.violations.push(Violation::WallDeadline {
        observed_by: std::time::Instant::now(),
    });
    outcomes.push(timeout);
    let mut output_limit = success();
    output_limit.stop_trigger = Some(StopTrigger::OutputBudget(Stream::Stdout));
    outcomes.push(output_limit);
    let mut error = success();
    error.errors.push(SupervisorError::Wait(5));
    outcomes.push(error);
    let mut survivors = success();
    survivors.cleanup = crate::supervisor::Cleanup::Incomplete { survivors: 1 };
    outcomes.push(survivors);
    let mut unknown = success();
    unknown.termination = Termination::Unknown;
    outcomes.push(unknown);
    for (index, outcome) in outcomes.into_iter().enumerate() {
        let pending = directory.pending(&target, &format!("outcome-{index}"), 100, &probe);
        write_executable(&file_artifact(&pending, &target));
        assert!(matches!(
            *finish_build(pending, outcome).unwrap_err().kind,
            BuildFailureKind::ProducerFailed(_)
        ));
    }
}

#[test]
fn auxiliary_bytes_count_and_overflow_removes_the_whole_build() {
    let directory = Directory::new();
    let target = directory.target("java", "");
    let probe = Tools::new(&directory);
    let pending = directory.pending(&target, "oversize", 10, &probe);
    let root = pending.output_root().to_owned();
    fs::write(root.join("Main.class"), "class").unwrap();
    fs::write(root.join("auxiliary"), "0123456789").unwrap();
    let error = finish_build(pending, success()).unwrap_err();
    assert!(matches!(*error.kind, BuildFailureKind::BuildOutputTooLarge));
    let overflow = error.over_cap.unwrap();
    assert_eq!((overflow.bytes, overflow.limit), (15, 10));
    let report = overflow.removal.unwrap();
    assert!(report.errors.is_empty() && report.skipped.is_empty());
    assert!(!root.exists());
}

fn reported_build(
    class: RunClass,
    validation: Result<&ValidatedBuild, &BuildFailure>,
) -> report::BuildResult {
    report::build_result(
        &report::ExecutionResult {
            coverage: report::Coverage::Started,
            class: Some(class),
            invariant: None,
        },
        &PostBuild::from_validation(validation),
    )
}

#[test]
fn overflow_preserves_validation_order_and_removal_evidence() {
    let directory = Directory::new();
    let target = directory.target(
        "cpp",
        "[languages.cpp.run]\nargv = ['{artifact_dir}/launcher.exe']\n",
    );
    let probe = Tools::new(&directory);
    for (id, declared, generated) in [
        ("missing", false, false),
        ("invalid", true, false),
        ("oversize-only", true, true),
    ] {
        let pending = directory.pending(&target, id, 1, &probe);
        let root = pending.output_root().to_owned();
        let artifact = file_artifact(&pending, &target);
        fs::write(root.join("auxiliary"), "oversized").unwrap();
        if declared {
            fs::write(&artifact, "data").unwrap();
        }
        if generated {
            write_executable(&root.join("launcher.exe"));
        }
        let error = finish_build(pending, success()).unwrap_err();
        let post = PostBuild::from_validation(Err(&error));
        let overflow = error.over_cap.as_ref().expect("overflow retained");
        assert_eq!(overflow.measurement.bytes, overflow.bytes);
        assert_eq!(post.output_too_large, Some((overflow.bytes, 1)));
        let removal = overflow.removal.as_ref().unwrap();
        assert!(removal.errors.is_empty() && removal.skipped.is_empty());
        assert!(!root.exists());
        let report::BuildResult::Failed { cause } =
            reported_build(RunClass::ExitedZero, Err(&error))
        else {
            panic!("expected build failure: {error}");
        };
        if !declared {
            assert!(matches!(
                error.kind.as_ref(),
                BuildFailureKind::OutputMissing(_)
            ));
            assert_eq!(
                cause,
                report::BuildCause::MissingOutput {
                    names: vec![artifact.strip_prefix(&root).unwrap().as_os_str().to_owned()],
                    names_dropped: 0,
                }
            );
            // The generated program was inspected before the tree was removed,
            // even though the missing declared output supplies the primary cause.
            assert!(post.generated_program_invalid.is_some());
        } else if !generated {
            assert!(matches!(
                error.kind.as_ref(),
                BuildFailureKind::GeneratedProgramNotLaunchable(_)
            ));
            assert!(matches!(
                cause,
                report::BuildCause::GeneratedProgramInvalid { .. }
            ));
        } else {
            assert!(matches!(
                error.kind.as_ref(),
                BuildFailureKind::BuildOutputTooLarge
            ));
            assert_eq!(
                cause,
                report::BuildCause::OutputTooLarge {
                    bytes: overflow.bytes,
                    cap: 1
                }
            );
            assert!(post.generated_program_invalid.is_none());
        }
        assert!(error.to_string().contains("removal: Ok"));
    }
}

#[test]
fn overflow_with_nonregular_declared_output_is_missing_output() {
    let directory = Directory::new();
    let target = directory.target("cpp", "[languages.cpp.run]\nargv = ['java']\n");
    let probe = Tools::new(&directory);
    let pending = directory.pending(&target, "nonregular-overflow", 1, &probe);
    fs::create_dir(file_artifact(&pending, &target)).unwrap();
    fs::write(pending.output_root().join("auxiliary"), "oversized").unwrap();
    let root = pending.output_root().to_owned();
    let error = finish_build(pending, success()).unwrap_err();
    assert!(matches!(
        error.kind.as_ref(),
        BuildFailureKind::OutputNotRegular(_)
    ));
    assert!(matches!(
        reported_build(RunClass::ExitedZero, Err(&error)),
        report::BuildResult::Failed {
            cause: report::BuildCause::MissingOutput { .. }
        }
    ));
    assert!(error.over_cap.as_ref().unwrap().removal.is_ok());
    assert!(!root.exists());
}

#[test]
fn discard_breaches_and_enforcement_timing_do_not_skip_validation() {
    let directory = Directory::new();
    let target = directory.target("cpp", "");
    let probe = Tools::new(&directory);
    let mut outcome = success();
    outcome.stderr.exceeded = true;
    outcome
        .stderr
        .losses
        .push(supervisor::CaptureLoss::BudgetDiscard);
    outcome.violations.push(Violation::OutputBudget {
        stream: Stream::Stderr,
        offset: 5,
    });
    outcome.enforced_at = Some(Duration::from_millis(1));
    for valid in [false, true] {
        let mut pending = directory.pending(
            &target,
            if valid {
                "discard-valid"
            } else {
                "discard-missing"
            },
            100,
            &probe,
        );
        pending.build.stderr.policy = crate::config::CapPolicy::Discard;
        let record = outcome.run_input([pending.build.stdout.policy, pending.build.stderr.policy]);
        assert_eq!(report::classify_run(&record), RunClass::ExitedZero);
        assert!(report::run_notes(&record).contains(&report::RunNote::TruncatedStderr));
        if valid {
            write_executable(&file_artifact(&pending, &target));
        }
        let validation = finish_build(pending, outcome.clone());
        if valid {
            let validated = validation.unwrap();
            assert_eq!(
                reported_build(RunClass::ExitedZero, Ok(&validated)),
                report::BuildResult::Succeeded
            );
            assert!(validated.run(directory.path("run-one")).is_ok());
        } else {
            let failure = validation.unwrap_err();
            assert!(matches!(
                failure.kind.as_ref(),
                BuildFailureKind::OutputMissing(_)
            ));
        }
    }
}

#[test]
fn producer_failures_follow_run_class_and_never_validate_outputs() {
    let directory = Directory::new();
    let target = directory.target("cpp", "");
    let probe = Tools::new(&directory);
    let mut kill = success();
    kill.stop_trigger = Some(StopTrigger::OutputBudget(Stream::Stderr));
    kill.stderr.exceeded = true;
    let mut incomplete = success();
    incomplete.cleanup = supervisor::Cleanup::Incomplete { survivors: 1 };
    let mut capture_loss = success();
    capture_loss
        .stdout
        .losses
        .push(supervisor::CaptureLoss::WriteError(5));
    for (id, outcome, expected) in [
        (
            "kill",
            kill,
            RunClass::OutputBudget {
                streams: report::BudgetStreams {
                    stdout: false,
                    stderr: true,
                },
            },
        ),
        (
            "incomplete",
            incomplete,
            RunClass::Infrastructure {
                causes: report::InfrastructureCauses::new([
                    report::InfrastructureCause::CleanupIncomplete { survivors: 1 },
                ]),
            },
        ),
        (
            "capture-loss",
            capture_loss,
            RunClass::Infrastructure {
                causes: report::InfrastructureCauses::new([
                    report::InfrastructureCause::CaptureLoss {
                        stream: report::Stream::Stdout,
                        loss: report::LossKind::WriteError,
                    },
                ]),
            },
        ),
    ] {
        let pending = directory.pending(&target, id, 1, &probe);
        let record = outcome.run_input([pending.build.stdout.policy, pending.build.stderr.policy]);
        let class = report::classify_run(&record);
        assert_eq!(class, expected);
        // Missing outputs and overflow would fail validation if it ran. Leaving
        // this tree untouched proves the producer gate precedes that work.
        let auxiliary = pending.output_root().join("auxiliary");
        fs::write(&auxiliary, "oversized").unwrap();
        let failure = finish_build(pending, outcome).unwrap_err();
        assert!(matches!(
            failure.kind.as_ref(),
            BuildFailureKind::ProducerFailed(_)
        ));
        assert!(failure.over_cap.is_none());
        assert_eq!(
            PostBuild::from_validation(Err(&failure)),
            PostBuild::default()
        );
        assert!(auxiliary.is_file());
        let result = reported_build(class, Err(&failure));
        match expected {
            RunClass::Infrastructure { causes } => {
                assert_eq!(result, report::BuildResult::Infrastructure { causes })
            }
            class => assert_eq!(
                result,
                report::BuildResult::Failed {
                    cause: report::BuildCause::Run(class)
                }
            ),
        }
    }
}

#[test]
fn validated_build_runs_twice_without_repeating_program_lookup() {
    let directory = Directory::new();
    let target = directory.target("cpp", "");
    let probe = Tools::new(&directory);
    let pending = directory.pending(&target, "valid", 100, &probe);
    let artifact = file_artifact(&pending, &target);
    write_executable(&artifact);
    let validated = finish_build(pending, success()).unwrap();
    let calls = probe.calls.borrow().len();
    // Outputs are not revalidated per test. Admission owns writer cleanup.
    fs::remove_file(&artifact).unwrap();
    let first = validated.run(directory.path("run-one")).unwrap();
    let second = validated.run(directory.path("run-two")).unwrap();
    assert_eq!(program(&first), program(&second));
    assert_eq!(program(&first), artifact);
    assert_eq!(first.cwd, directory.path("run-one"));
    assert_eq!(second.cwd, directory.path("run-two"));
    assert_eq!(probe.calls.borrow().len(), calls);
    assert!(validated.run(PathBuf::from("relative")).is_err());
}

#[test]
fn python_is_preflighted_and_runs_without_an_output_allocation() {
    let directory = Directory::new();
    let target = directory.target("python", "");
    let probe = Tools::new(&directory);
    let planned = plan_target(&target, directory.inputs(&target, 0, &probe), None).unwrap();
    let PlannedTarget::BuildFree(target) = planned else {
        panic!("build-free")
    };
    let stage = target.run(directory.path("run-one")).unwrap();
    assert!(program(&stage).starts_with(directory.path("tools")));
    assert_eq!(stage.cwd, directory.path("run-one"));
    assert_eq!(fs::read_dir(directory.path("builds")).unwrap().count(), 0);
    let ExpandedArguments::Argv(args) = target.expanded_arguments() else {
        panic!("argv")
    };
    assert_eq!(
        args.last().unwrap(),
        directory.path("inputs/src/main.py").as_os_str()
    );
}

#[test]
fn java_checks_required_files_without_treating_class_as_an_executable() {
    let directory = Directory::new();
    let target = directory.target("java", "[languages.java]\nentry = 'a.b.Grader'\n");
    let probe = Tools::new(&directory);
    let missing = directory.pending(&target, "missing-class", 100, &probe);
    assert!(matches!(
        *finish_build(missing, success()).unwrap_err().kind,
        BuildFailureKind::OutputMissing(_)
    ));
    let pending = directory.pending(&target, "classes", 100, &probe);
    let root = pending.output_root().to_owned();
    fs::create_dir_all(under(&root, "a/b")).unwrap();
    fs::write(under(&root, "a/b/Grader.class"), "class").unwrap();
    fs::write(under(&root, "a/b/Grader$Helper.class"), "inner").unwrap();
    let validated = finish_build(pending, success()).unwrap();
    assert!(under(validated.output_root(), "a/b/Grader$Helper.class").is_file());
    let stage = validated.run(directory.path("run-one")).unwrap();
    assert_eq!(program(&stage).file_stem(), Some(OsStr::new("java")));
    let ResolvedCommand::Argv { args, .. } = stage.command else {
        panic!("argv")
    };
    assert_eq!(
        args,
        vec![
            OsString::from("-cp"),
            root.into_os_string(),
            OsString::from("a.b.Grader")
        ]
    );
}

#[cfg(unix)]
#[test]
fn generated_program_launchability_is_separate_from_declared_output_presence() {
    let directory = Directory::new();
    let target = directory.target(
        "cpp",
        "[languages.cpp.run]\nargv = ['{artifact_dir}/launcher']\n",
    );
    let probe = Tools::new(&directory);
    let pending = directory.pending(&target, "launcher", 100, &probe);
    fs::write(file_artifact(&pending, &target), "not executable").unwrap();
    assert!(matches!(
        *finish_build(pending, success()).unwrap_err().kind,
        BuildFailureKind::GeneratedProgramNotLaunchable(_)
    ));
    let target = directory.target("cpp", "");
    let pending = directory.pending(&target, "not-executable", 100, &probe);
    fs::write(file_artifact(&pending, &target), "not executable").unwrap();
    assert!(matches!(
        *finish_build(pending, success()).unwrap_err().kind,
        BuildFailureKind::GeneratedProgramNotLaunchable(_)
    ));
}

#[test]
fn a_file_artifact_need_not_be_the_run_program() {
    let directory = Directory::new();
    let target = directory.target("cpp", "[languages.cpp.run]\nargv = ['java']\n");
    let probe = Tools::new(&directory);
    let pending = directory.pending(&target, "data-file", 100, &probe);
    fs::write(file_artifact(&pending, &target), "data").unwrap();
    let validated = finish_build(pending, success()).unwrap();
    let stage = validated.run(directory.path("run-one")).unwrap();
    assert_eq!(program(&stage).file_stem(), Some(OsStr::new("java")));
}

#[test]
fn shell_arguments_are_exposed_and_shell_runs_require_validation() {
    let directory = Directory::new();
    let target = directory.target("cpp", r#"
[languages.cpp.build]
shell = { interpreter = ['sh', '-c'], script = 'build "$1" "$2"', args = ['{sources}', '{artifact}'] }
[languages.cpp.run]
shell = { interpreter = ['sh', '-c'], script = 'run "$1"', args = ['{artifact}'] }
"#);
    let probe = Tools::new(&directory);
    let pending = directory.pending(&target, "shell-failed", 100, &probe);
    let artifact = file_artifact(&pending, &target);
    assert_eq!(
        pending.expanded_arguments(),
        &ExpandedArguments::ShellArgs(vec![
            directory.path("inputs/src/main.cpp").into_os_string(),
            artifact.into_os_string(),
        ])
    );
    assert!(matches!(
        *finish_build(pending, success()).unwrap_err().kind,
        BuildFailureKind::OutputMissing(_)
    ));
    let pending = directory.pending(&target, "shell-valid", 100, &probe);
    fs::write(file_artifact(&pending, &target), "data").unwrap();
    let validated = finish_build(pending, success()).unwrap();
    let stage = validated.run(directory.path("run-one")).unwrap();
    assert!(matches!(&stage.command, ResolvedCommand::Shell { .. }));
    assert!(stage.launch_command(Platform::host()).is_ok());
}

#[test]
fn generated_build_program_and_build_cwd_inside_outputs_are_planning_errors() {
    let directory = Directory::new();
    let probe = Tools::new(&directory);
    let target = directory.target("cpp", "[languages.cpp.build]\nargv = ['{artifact}']\n");
    let errors = plan_target(
        &target,
        directory.inputs(&target, 100, &probe),
        Some(directory.outputs("own-producer")),
    )
    .unwrap_err();
    assert!(
        errors
            .errors
            .iter()
            .any(|error| matches!(error, PlanError::Resolve(errors)
        if errors.errors.iter().any(|error| error.kind == exec::ResolveErrorKind::OwnProducer)))
    );
    let target = directory.target(
        "cpp",
        "[languages.cpp.build]\ncwd = { path = '{artifact_dir}' }\n",
    );
    let errors = plan_target(
        &target,
        directory.inputs(&target, 100, &probe),
        Some(directory.outputs("build-cwd")),
    )
    .unwrap_err();
    assert!(
        errors
            .errors
            .iter()
            .any(|error| matches!(error, PlanError::BuildCwdInsideOutputs))
    );
}

#[test]
fn every_explicit_java_requirement_is_enforced() {
    let directory = Directory::new();
    let target = directory.target(
        "java",
        "[languages.java]\nartifacts = { dir = true, required = ['Main.class', 'Aux.class'] }\n",
    );
    let probe = Tools::new(&directory);
    let absent = directory.pending(&target, "all-absent", 100, &probe);
    let failure = finish_build(absent, success()).unwrap_err();
    assert_eq!(
        PostBuild::from_validation(Err(&failure)).missing_outputs,
        vec![OsString::from("Main.class"), OsString::from("Aux.class")]
    );
    let pending = directory.pending(&target, "required", 100, &probe);
    fs::write(pending.output_root().join("Main.class"), "class").unwrap();
    let missing = pending.output_root().join("Aux.class");
    assert!(
        matches!(*finish_build(pending, success()).unwrap_err().kind,
        BuildFailureKind::OutputMissing(path) if path == missing)
    );
}

#[test]
fn failed_latest_attempt_cannot_use_a_retained_successful_attempt_identity() {
    let directory = Directory::new();
    let target = directory.target("cpp", "");
    let probe = Tools::new(&directory);
    let source = directory.path("inputs/src/main.cpp");
    fs::write(&source, "first source").unwrap();
    let first = directory.pending(&target, "first", 100, &probe);
    write_executable(&file_artifact(&first, &target));
    let first = finish_build(first, success()).unwrap();
    fs::write(source, "changed source").unwrap();
    let second = directory.pending(&target, "second", 100, &probe);
    let latest_attempt = second.id().clone();
    assert_ne!(first.output_root(), second.output_root());
    let mut outcome = success();
    outcome.termination = Termination::Exited { code: 1 };
    let failure = finish_build(second, outcome).unwrap_err();
    assert_eq!(failure.id, latest_attempt);
    assert_ne!(first.id(), &latest_attempt);
    assert!(first.output_root().is_dir());
    // The caller's latest-attempt check refuses this still-valid older value.
    // Neither failures nor pending attempts expose run(); ids cannot be reset.
}
