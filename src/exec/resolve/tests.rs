use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;

use super::*;
use crate::exec::tests::{Directory, command_context, prepared};
use crate::exec::{Baseline, Context, NativeProbe, TargetBindings, prepare};

#[derive(Debug, Default)]
struct FakeProbe {
    entries: BTreeMap<Vec<u8>, Result<ProbeResult, io::ErrorKind>>,
    calls: RefCell<Vec<Vec<u8>>>,
}

impl FakeProbe {
    fn with(entries: &[(&str, ProbeResult)]) -> Self {
        Self {
            entries: entries
                .iter()
                .map(|(path, result)| (path.as_bytes().to_vec(), Ok(*result)))
                .collect(),
            calls: RefCell::default(),
        }
    }

    fn paths(&self) -> Vec<OsString> {
        self.calls
            .borrow()
            .iter()
            .map(|bytes| {
                // SAFETY: every entry was copied from an OsStr in probe(), with no
                // modification, on this host in this process.
                unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }.to_owned()
            })
            .collect()
    }
}

impl Probe for FakeProbe {
    fn probe(&self, path: &Path) -> io::Result<ProbeResult> {
        let bytes = path.as_os_str().as_encoded_bytes().to_vec();
        self.calls.borrow_mut().push(bytes.clone());
        match self
            .entries
            .get(&bytes)
            .copied()
            .unwrap_or(Ok(ProbeResult::NotFound))
        {
            Ok(result) => Ok(result),
            Err(kind) => Err(io::Error::new(kind, "candidate observation failed")),
        }
    }
}

const YES: ProbeResult = ProbeResult::Regular(Executable::Yes);
const NO: ProbeResult = ProbeResult::Regular(Executable::No);

fn env(entries: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    entries
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

fn lookup(name: &str) -> ProgramRequest {
    ProgramRequest::Lookup(name.into())
}

fn explicit(path: &str) -> ProgramRequest {
    ProgramRequest::Path(path.into())
}

fn chosen(resolution: Resolution) -> ResolvedStage {
    let Resolution::Resolved(stage) = resolution else {
        panic!("resolved stage expected")
    };
    stage
}

fn program(stage: &ResolvedStage) -> &ResolvedProgram {
    match &stage.command {
        ResolvedCommand::Argv { program, .. } => program,
        ResolvedCommand::Shell { interpreter, .. } => interpreter,
    }
}

#[test]
fn missing_and_empty_path_permit_only_explicit_paths_on_both_platforms() {
    for (platform, path) in [
        (Platform::Linux, "/bin/tool"),
        (Platform::Windows, r"C:\bin\tool.exe"),
        (Platform::Windows, r"\\server\share\tool.exe"),
        (Platform::Windows, r"\\?\C:\bin\tool.exe"),
        (Platform::Windows, r"\\?\UNC\server\share\tool.exe"),
    ] {
        for environment in [vec![], env(&[("PATH", "")])] {
            let probe = FakeProbe::with(&[(path, YES)]);
            let trace = select(&lookup("tool"), &environment, &probe, platform).unwrap_err();
            assert!(trace.skipped_candidates.is_empty());
            assert!(probe.paths().is_empty());
            let result = select(&explicit(path), &environment, &probe, platform).unwrap();
            assert_eq!(result.path.as_os_str(), OsStr::new(path));
        }
    }
}

#[test]
fn path_entries_follow_the_target_platform_and_never_search_cwd() {
    for (platform, path, expected) in [
        (Platform::Linux, ":relative:/one::/two", "/two/tool"),
        (
            Platform::Windows,
            ";relative;\"C:\\one\";;\"C:\\two\"",
            r"C:\two\tool.EXE",
        ),
    ] {
        let probe = FakeProbe::with(&[(expected, YES)]);
        let result = select(
            &lookup("tool"),
            &env(&[("Path", "ignored"), ("PATH", path), ("PATHEXT", ".EXE")]),
            &probe,
            platform,
        )
        .unwrap();
        assert_eq!(result.path.as_os_str(), OsStr::new(expected));
        let skipped = &result.record.trace.skipped_path_entries;
        assert_eq!(skipped.len(), 3);
        assert_eq!(skipped[0].reason, EntryReason::Empty);
        assert_eq!(skipped[1].reason, EntryReason::Relative);
        assert_eq!(skipped[2].reason, EntryReason::Empty);
        assert_eq!(skipped[1].entry, "relative");
        assert_eq!(probe.paths().len(), 2);
    }
    // Unix names are byte-exact, and backslashes do not make a PATH entry absolute.
    let probe = FakeProbe::default();
    assert!(
        select(
            &lookup("tool"),
            &env(&[("Path", "/one")]),
            &probe,
            Platform::Linux
        )
        .is_err()
    );
    let trace = select(
        &lookup("tool"),
        &env(&[("PATH", r"\one;C:two")]),
        &probe,
        Platform::Windows,
    )
    .unwrap_err();
    assert_eq!(trace.skipped_path_entries.len(), 2);
    assert!(probe.paths().is_empty());
}

#[test]
fn windows_path_quotes_protect_separators_and_are_removed() {
    let probe = FakeProbe::with(&[(r"C:\a;b\c\tool.EXE", YES)]);
    let result = select(
        &lookup("tool"),
        &env(&[("PATH", r#""";"C:\a;b"\c;C:\d"#), ("PATHEXT", ".EXE")]),
        &probe,
        Platform::Windows,
    )
    .unwrap();
    assert_eq!(result.path.as_os_str(), OsStr::new(r"C:\a;b\c\tool.EXE"));
    let skipped = &result.record.trace.skipped_path_entries;
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].entry, r#""""#);
    assert_eq!(skipped[0].reason, EntryReason::Empty);
    assert_eq!(probe.paths(), [OsString::from(r"C:\a;b\c\tool.EXE")]);
    // Unix PATH has no quoting.
    let trace = select(
        &lookup("tool"),
        &env(&[("PATH", r#""/a""#)]),
        &probe,
        Platform::Linux,
    )
    .unwrap_err();
    assert_eq!(trace.skipped_path_entries[0].reason, EntryReason::Relative);
}

#[test]
fn unix_rejections_continue_in_path_order_and_keep_io_errors_and_approximation() {
    let mut probe = FakeProbe::with(&[
        ("/a/tool", ProbeResult::Directory),
        ("/b/tool", NO),
        ("/c/tool", ProbeResult::Other),
        (
            "/e/tool",
            ProbeResult::Regular(Executable::Approximate(false)),
        ),
        (
            "/f/tool",
            ProbeResult::Regular(Executable::Approximate(true)),
        ),
        ("/g/tool", YES),
    ]);
    probe
        .entries
        .insert(b"/d/tool".to_vec(), Err(io::ErrorKind::PermissionDenied));
    let result = select(
        &lookup("tool"),
        &env(&[("PATH", "/a:/b:/c:/d:/e:/f:/g")]),
        &probe,
        Platform::Linux,
    )
    .unwrap();
    assert_eq!(result.path, PathBuf::from("/f/tool"));
    let trace = &result.record.trace;
    assert!(trace.approximate_check);
    assert_eq!(trace.skipped_candidates.len(), 5);
    assert!(matches!(
        trace.skipped_candidates[0].reason,
        CandidateReason::Directory
    ));
    assert!(matches!(
        trace.skipped_candidates[1].reason,
        CandidateReason::NotExecutable
    ));
    assert!(matches!(
        trace.skipped_candidates[2].reason,
        CandidateReason::Other
    ));
    let CandidateReason::Io(error) = &trace.skipped_candidates[3].reason else {
        panic!("I/O error expected")
    };
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(error.to_string(), "candidate observation failed");
    assert_eq!(probe.paths().len(), 6);
    assert_eq!(result.launch_kind, LaunchKind::Native);
}

#[test]
fn first_launchable_path_wins_on_each_platform() {
    for (platform, path, first, second) in [
        (
            Platform::Linux,
            "/first:/second",
            "/first/tool",
            "/second/tool",
        ),
        (
            Platform::Windows,
            r"C:\first;C:\second",
            r"C:\first\tool.EXE",
            r"C:\second\tool.EXE",
        ),
    ] {
        let probe = FakeProbe::with(&[(first, YES), (second, YES)]);
        let result = select(
            &lookup("tool"),
            &env(&[("PATH", path), ("PATHEXT", ".EXE")]),
            &probe,
            platform,
        )
        .unwrap();
        assert_eq!(result.path.as_os_str(), OsStr::new(first));
        assert_eq!(probe.paths(), [OsString::from(first)]);
        assert_eq!(result.record.request, lookup("tool"));
        assert_eq!(result.record.chosen_path, result.path);
        assert_eq!(result.record.launch_kind, result.launch_kind);
    }
}

#[test]
fn windows_default_pathext_order_does_not_try_bare_lookup() {
    for environment in [
        env(&[("PATH", r"C:\bin")]),
        env(&[("Path", r"C:\bin"), ("Pathext", "")]),
    ] {
        let probe = FakeProbe::with(&[(r"C:\bin\tool", YES), (r"C:\bin\tool.CMD", NO)]);
        let result = select(&lookup("tool"), &environment, &probe, Platform::Windows).unwrap();
        assert_eq!(result.path.as_os_str(), OsStr::new(r"C:\bin\tool.CMD"));
        assert_eq!(result.launch_kind, LaunchKind::Batch);
        assert_eq!(
            probe.paths(),
            [
                r"C:\bin\tool.COM",
                r"C:\bin\tool.EXE",
                r"C:\bin\tool.BAT",
                r"C:\bin\tool.CMD"
            ]
            .map(OsString::from)
        );
    }
}

#[test]
fn windows_explicit_extensionless_path_tries_as_is_then_pathext() {
    let probe = FakeProbe::with(&[(r"C:\bin\tool", YES), (r"C:\bin\tool.cMd", NO)]);
    let result = select(
        &explicit(r"C:\bin\tool"),
        &env(&[("PATHEXT", ".cMd;.eXe")]),
        &probe,
        Platform::Windows,
    )
    .unwrap();
    assert_eq!(result.launch_kind, LaunchKind::Batch);
    assert_eq!(
        probe.paths(),
        [r"C:\bin\tool", r"C:\bin\tool.cMd"].map(OsString::from)
    );
    assert!(matches!(
        result.record.trace.skipped_candidates[0].reason,
        CandidateReason::UnsupportedExtension
    ));
}

#[test]
fn windows_any_extension_is_tried_as_is_only_and_compared_without_case() {
    for (name, launch_kind) in [
        ("x.eXe", Some(LaunchKind::Native)),
        ("x.CoM", Some(LaunchKind::Native)),
        ("x.CMD", Some(LaunchKind::Batch)),
        ("x.bAt", Some(LaunchKind::Batch)),
        ("x.ps1", None),
        ("x.tar.gz", None),
    ] {
        let path = format!(r"C:\bin\{name}");
        for request in [lookup(name), explicit(&path)] {
            let probe = FakeProbe::with(&[(path.as_str(), YES)]);
            let result = select(
                &request,
                &env(&[("PATH", r"C:\bin"), ("PATHEXT", ".EXE;.CMD")]),
                &probe,
                Platform::Windows,
            );
            match launch_kind {
                Some(kind) => assert_eq!(result.unwrap().launch_kind, kind),
                None => assert!(matches!(
                    result.unwrap_err().skipped_candidates[0].reason,
                    CandidateReason::UnsupportedExtension
                )),
            }
            assert_eq!(probe.paths(), [OsString::from(&path)]);
        }
    }
}

#[test]
fn windows_malformed_pathext_entries_are_skipped_and_reported() {
    let probe = FakeProbe::with(&[(r"C:\bin\x.PS1", YES), (r"C:\later\x.EXE", YES)]);
    let result = select(
        &lookup("x"),
        &env(&[
            ("PATH", r"C:\bin;C:\later"),
            ("PATHEXT", r";EXE;.;.bad/name;.bad\name;.PS1;.EXE;"),
        ]),
        &probe,
        Platform::Windows,
    )
    .unwrap();
    assert_eq!(result.path.as_os_str(), OsStr::new(r"C:\later\x.EXE"));
    let trace = result.record.trace;
    assert_eq!(trace.skipped_pathext_entries.len(), 6);
    assert!(
        trace
            .skipped_pathext_entries
            .iter()
            .all(|entry| entry.reason == EntryReason::MalformedExtension)
    );
    assert!(matches!(
        trace.skipped_candidates[0].reason,
        CandidateReason::UnsupportedExtension
    ));
    assert_eq!(probe.paths().len(), 4);
    let trace = select(
        &lookup("x"),
        &env(&[("PATH", r"C:\bin"), ("PATHEXT", ";bad;.")]),
        &probe,
        Platform::Windows,
    )
    .unwrap_err();
    assert_eq!(trace.skipped_pathext_entries.len(), 3);
    assert!(trace.skipped_candidates.is_empty());
}

#[test]
fn windows_directories_other_files_and_io_errors_do_not_end_search() {
    let mut probe = FakeProbe::with(&[
        (r"C:\a\x.EXE", ProbeResult::Directory),
        (r"C:\b\x.EXE", ProbeResult::Other),
        (r"C:\d\x.EXE", YES),
    ]);
    probe.entries.insert(
        br"C:\c\x.EXE".to_vec(),
        Err(io::ErrorKind::PermissionDenied),
    );
    let result = select(
        &lookup("x"),
        &env(&[("PATH", r"C:\a;C:\b;C:\c;C:\d"), ("PATHEXT", ".EXE")]),
        &probe,
        Platform::Windows,
    )
    .unwrap();
    assert_eq!(result.record.trace.skipped_candidates.len(), 3);
    assert_eq!(result.path.as_os_str(), OsStr::new(r"C:\d\x.EXE"));
}

#[test]
fn stage_environment_edits_control_resolution_and_winning_windows_spelling() {
    let dir = Directory::new();
    for (platform, baseline_path, edited_path, executable) in [
        (Platform::Linux, "/baseline", "/edited", "/edited/tool"),
        (
            Platform::Windows,
            r"C:\baseline",
            r"C:\edited",
            r"C:\edited\tool.EXE",
        ),
    ] {
        let plan = dir.command(&format!(
            "argv = ['tool']\nenv = {{ Path = '{edited_path}' }}"
        ));
        // Unix requires PATH exactly, whereas Windows finds the winning Path spelling.
        let mut stage = plan.stage.clone();
        if platform != Platform::Windows {
            let crate::config::EnvEdit::Set { name, .. } = &mut stage.environment[0] else {
                panic!("set expected")
            };
            name.value = "PATH".into();
        }
        let mut command = plan.clone();
        command.stage = stage;
        let mut dirs = dir.dirs();
        dirs.run_cwd = if platform == Platform::Windows {
            r"C:\job\cwd".into()
        } else {
            "/job/cwd".into()
        };
        let ctx = command_context(&command, dirs);
        let prepared = prepare(
            &command.stage,
            &ctx,
            &Baseline::from_entries(env(&[("PATH", baseline_path), ("PATHEXT", ".EXE")])),
            platform,
            &[],
        )
        .unwrap();
        let probe = FakeProbe::with(&[(executable, YES)]);
        let stage = chosen(resolve(prepared, &probe, platform).unwrap());
        assert_eq!(program(&stage).path.as_os_str(), OsStr::new(executable));
        assert_eq!(probe.paths(), [OsString::from(executable)]);
    }
}

fn fixture(dir: &Directory, request: ProgramRequest, platform: Platform) -> PreparedStage {
    let plan = dir.command("argv = ['tool', 'one']");
    let ctx = command_context(&plan, dir.dirs());
    let mut stage = prepared(&plan.stage, &ctx);
    stage.command = PreparedCommand::Argv {
        program: request,
        args: vec!["one".into()],
    };
    stage.cwd = if platform == Platform::Windows {
        r"C:\job\cwd".into()
    } else {
        "/job/cwd".into()
    };
    stage
}

#[test]
fn errors_name_the_request_all_skips_and_c1_origin() {
    let dir = Directory::new();
    for (platform, path, candidate) in [
        (Platform::Linux, "/bin:/absent", "/bin/tool"),
        (Platform::Windows, r"C:\bin;C:\absent", r"C:\bin\tool.EXE"),
    ] {
        let mut stage = fixture(&dir, lookup("tool"), platform);
        stage.environment = env(&[("PATH", path), ("PATHEXT", ".EXE")]);
        let StageCommand::Argv(argv) = &stage.plan.command else {
            panic!("argv expected")
        };
        let origin = argv[0].origin.clone();
        let provenance = argv[0].provenance.clone();
        let probe = FakeProbe::with(&[(candidate, ProbeResult::Directory)]);
        let errors = resolve(stage, &probe, platform).unwrap_err();
        let failure = &errors.errors[0];
        assert_eq!(failure.stage, StageKind::Command);
        assert_eq!(failure.field, "commands.c.argv[0]");
        assert_eq!(failure.origin, origin);
        assert_eq!(failure.provenance, provenance);
        assert_eq!(failure.request, lookup("tool"));
        assert_eq!(
            failure.trace.skipped_candidates[0].path.as_os_str(),
            OsStr::new(candidate)
        );
        let display = errors.to_string();
        assert!(display.contains("tool"));
        assert!(display.contains(candidate));
        assert!(display.contains("directory"));
        assert!(display.contains("absent"));
        assert!(display.contains("not found"));
        assert_eq!(failure.trace.skipped_candidates.len(), 2);
    }
}

#[test]
fn windows_trailing_dot_and_space_are_rejected_without_probing() {
    let dir = Directory::new();
    for request in [
        lookup("tool."),
        lookup("tool.cmd "),
        explicit(r"C:\out\tool."),
        explicit(r"C:\out\tool.cmd "),
    ] {
        let stage = fixture(&dir, request, Platform::Windows);
        let probe = FakeProbe::default();
        let errors = resolve(stage, &probe, Platform::Windows).unwrap_err();
        assert_eq!(errors.errors[0].kind, ResolveErrorKind::TrailingDotOrSpace);
        assert!(probe.paths().is_empty());
    }
}

#[test]
fn generated_run_program_is_deferred_until_producer_and_then_checked() {
    let dir = Directory::new();
    let target = dir.target("cpp", "");
    let mut bindings = dir.bindings(&target, 1);
    if cfg!(windows) {
        bindings.artifact = Some(dir.path("job/out/main.exe"));
    }
    let artifact = bindings.artifact.as_ref().unwrap().clone();
    let ctx = Context::target_run(&target, dir.dirs(), bindings.clone()).unwrap();
    let prepared = prepare(
        &target.run,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap();
    assert_eq!(prepared.artifact_dir, bindings.artifact_dir);
    let missing = FakeProbe::default();
    let Resolution::AwaitingProducer(deferred) =
        resolve(prepared.clone(), &missing, Platform::host()).unwrap()
    else {
        panic!("deferred stage expected")
    };
    assert!(missing.paths().is_empty());
    let errors = deferred.complete(&missing).unwrap_err();
    assert_eq!(errors.errors[0].kind, ResolveErrorKind::NotFound);
    let Resolution::AwaitingProducer(deferred) =
        resolve(prepared, &missing, Platform::host()).unwrap()
    else {
        panic!("deferred stage expected")
    };
    let mut available = FakeProbe::default();
    available
        .entries
        .insert(artifact.as_os_str().as_encoded_bytes().to_vec(), Ok(YES));
    let stage = deferred.complete(&available).unwrap();
    assert_eq!(program(&stage).path.as_os_str(), artifact.as_os_str());
    assert_eq!(stage.plan, target.run);
    assert_eq!(stage.kind, StageKind::Run);
}

#[test]
fn generated_containment_is_component_wise_case_aware_and_rejects_dots() {
    let dir = Directory::new();
    for (platform, root, path, generated, dots) in [
        (Platform::Linux, "/out/art", "/out/art/tool", true, false),
        (Platform::Linux, "/out/art", "/out/artifact/x", false, false),
        (Platform::Linux, "/out/art", "/out/ART/tool", false, false),
        (Platform::Linux, "/out/art", "/out/art/../tool", true, true),
        (Platform::Linux, "/out/art", "/out/art/./tool", true, true),
        (Platform::Linux, "/out/art", "/out/./art/tool", true, true),
        (
            Platform::Windows,
            r"C:\out\art",
            r"c:/OUT/ART/tool.CMD",
            true,
            false,
        ),
        (
            Platform::Windows,
            r"C:\out\art",
            r"C:\out\artifact\x.exe",
            false,
            false,
        ),
        (
            Platform::Windows,
            r"C:\out\art",
            r"C:\out\art\..\x.exe",
            true,
            true,
        ),
        (
            Platform::Windows,
            r"\\server\share\art",
            r"//SERVER/share/ART/x.exe",
            true,
            false,
        ),
    ] {
        let mut stage = fixture(&dir, explicit(path), platform);
        stage.artifact_dir = Some(root.into());
        stage.kind = StageKind::Run;
        let probe = FakeProbe::with(&[(path, YES)]);
        let result = resolve(stage, &probe, platform);
        if dots {
            assert_eq!(
                result.unwrap_err().errors[0].kind,
                ResolveErrorKind::GeneratedDotComponent,
                "{path}"
            );
        } else if generated {
            let Resolution::AwaitingProducer(deferred) = result.unwrap() else {
                panic!("deferred expected for {path}")
            };
            assert!(probe.paths().is_empty());
            let stage = deferred.complete(&probe).unwrap();
            if path.ends_with(".CMD") {
                assert_eq!(program(&stage).launch_kind, LaunchKind::Batch);
            }
        } else {
            assert_eq!(
                program(&chosen(result.unwrap())).path.as_os_str(),
                OsStr::new(path)
            );
        }
    }
}

#[test]
fn build_cannot_be_its_own_producer_and_policy_errors_accumulate() {
    let dir = Directory::new();
    let mut stage = fixture(&dir, explicit(r"C:\out\art\..\tool."), Platform::Windows);
    stage.kind = StageKind::Build;
    stage.artifact_dir = Some(r"C:\out\art".into());
    let probe = FakeProbe::default();
    let errors = resolve(stage, &probe, Platform::Windows).unwrap_err();
    assert_eq!(
        errors.errors.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [
            ResolveErrorKind::TrailingDotOrSpace,
            ResolveErrorKind::GeneratedDotComponent,
            ResolveErrorKind::OwnProducer
        ]
    );
    assert!(probe.paths().is_empty());
}

#[test]
fn lookup_is_never_generated_even_when_selected_file_is_under_artifacts() {
    let dir = Directory::new();
    let mut stage = fixture(&dir, lookup("tool"), Platform::Linux);
    stage.artifact_dir = Some("/out/art".into());
    stage.environment = env(&[("PATH", "/out/art")]);
    let probe = FakeProbe::with(&[("/out/art/tool", YES)]);
    assert!(matches!(
        resolve(stage, &probe, Platform::Linux).unwrap(),
        Resolution::Resolved(_)
    ));
}

#[test]
fn shell_interpreter_resolves_by_lookup_and_preserves_all_shell_parts() {
    let dir = Directory::new();
    let plan = dir.command(
        "shell = { interpreter = ['sh', '-c'], script = 'echo {literal} \"$1\"', args = ['a b'] }",
    );
    for (platform, path, executable, cwd) in [
        (Platform::Linux, "/bin", "/bin/sh", "/job/cwd"),
        (
            Platform::Windows,
            r"C:\bin",
            r"C:\bin\sh.EXE",
            r"C:\job\cwd",
        ),
    ] {
        let mut dirs = dir.dirs();
        dirs.run_cwd = cwd.into();
        let ctx = command_context(&plan, dirs);
        let prepared = prepare(
            &plan.stage,
            &ctx,
            &Baseline::from_entries(env(&[("PATH", path), ("PATHEXT", ".EXE")])),
            platform,
            &[],
        )
        .unwrap();
        let expected = prepared.clone();
        let probe = FakeProbe::with(&[(executable, YES)]);
        let resolved = chosen(resolve(prepared, &probe, platform).unwrap());
        let ResolvedCommand::Shell {
            interpreter,
            interpreter_args,
            script,
            args,
        } = &resolved.command
        else {
            panic!("shell expected")
        };
        assert_eq!(interpreter.path.as_os_str(), OsStr::new(executable));
        assert_eq!(interpreter_args, &[OsString::from("-c")]);
        assert_eq!(script.value, "echo {literal} \"$1\"");
        assert_eq!(args, &[OsString::from("a b")]);
        assert_eq!(resolved.cwd, expected.cwd);
        assert_eq!(resolved.environment, expected.environment);
        assert_eq!(resolved.deadline, expected.deadline);
        assert_eq!(resolved.stdout, expected.stdout);
        assert_eq!(resolved.stderr, expected.stderr);
        assert_eq!(resolved.limits, expected.limits);
        assert_eq!(resolved.shared_cwd, expected.shared_cwd);
        assert_eq!(resolved.plan, expected.plan);
    }
}

#[test]
fn explicit_shell_interpreter_under_artifact_directory_is_an_error() {
    let dir = Directory::new();
    let plan = dir.command("shell = { interpreter = ['bin/sh', '-c'], script = 'true' }");
    let ctx = command_context(&plan, dir.dirs());
    let mut stage = prepared(&plan.stage, &ctx);
    stage.artifact_dir = Some(dir.path("bin"));
    let probe = FakeProbe::default();
    let errors = resolve(stage, &probe, Platform::host()).unwrap_err();
    assert_eq!(
        errors.errors[0].kind,
        ResolveErrorKind::GeneratedInterpreter
    );
    assert_eq!(errors.errors[0].field, "commands.c.shell.interpreter[0]");
    assert!(probe.paths().is_empty());
    // Even in a build stage the only error is the generated interpreter.
    let mut stage = prepared(&plan.stage, &ctx);
    stage.artifact_dir = Some(dir.path("bin"));
    stage.kind = StageKind::Build;
    let errors = resolve(stage, &probe, Platform::host()).unwrap_err();
    assert_eq!(
        errors.errors.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [ResolveErrorKind::GeneratedInterpreter]
    );
}

#[test]
fn c2a_retains_and_validates_artifact_directory_even_when_unused() {
    let dir = Directory::new();
    let target = dir.target("cpp", "");
    let mut bindings: TargetBindings = dir.bindings(&target, 1);
    bindings.artifact_dir = Some("relative".into());
    let ctx = Context::target_build(&target, dir.dirs(), bindings).unwrap();
    let errors = prepare(
        target.build.as_ref().unwrap(),
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap_err();
    assert!(
        errors
            .errors
            .iter()
            .any(|error| error.field == "artifact_dir"
                && error.kind == crate::exec::PrepareErrorKind::NotAbsolute)
    );
}

#[cfg(unix)]
#[test]
fn native_unix_probe_follows_links_and_checks_effective_user_permission() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = Directory::new();
    for (name, mode, launchable) in [
        ("yes", 0o755, true),
        ("no", 0o644, false),
        ("owner", 0o601, false),
    ] {
        // SAFETY: geteuid has no arguments or failure case.
        if name == "owner" && unsafe { libc::geteuid() } == 0 {
            continue;
        }
        let path = dir.path(name);
        fs::write(&path, b"contents are never launched").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let outcome = NativeProbe.probe(&path).unwrap();
        assert_eq!(
            outcome,
            ProbeResult::Regular(if launchable {
                Executable::Yes
            } else {
                Executable::No
            })
        );
        let result = select(
            &ProgramRequest::Path(path),
            &[],
            &NativeProbe,
            Platform::host(),
        );
        assert_eq!(result.is_ok(), launchable);
        if !launchable {
            assert!(matches!(
                result.unwrap_err().skipped_candidates[0].reason,
                CandidateReason::NotExecutable
            ));
        }
    }
    let directory = dir.path("directory");
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        NativeProbe.probe(&directory).unwrap(),
        ProbeResult::Directory
    );
    let trace = select(
        &ProgramRequest::Path(directory),
        &[],
        &NativeProbe,
        Platform::host(),
    )
    .unwrap_err();
    assert!(matches!(
        trace.skipped_candidates[0].reason,
        CandidateReason::Directory
    ));
    let link = dir.path("link");
    symlink(dir.path("yes"), &link).unwrap();
    let resolved = select(
        &ProgramRequest::Path(link.clone()),
        &[],
        &NativeProbe,
        Platform::host(),
    )
    .unwrap();
    assert_eq!(resolved.path, link);
    assert!(matches!(
        NativeProbe.probe(&dir.path("missing")).unwrap(),
        ProbeResult::NotFound
    ));
}

#[cfg(unix)]
#[test]
fn native_unix_path_order_and_non_unicode_paths() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    let dir = Directory::new();
    let first = dir.path("first");
    let mut second_name = OsString::from("second-");
    // APFS on macOS refuses names that are not UTF-8.
    if cfg!(target_os = "linux") {
        second_name.push(OsString::from_vec(vec![0xff]));
    }
    let second = dir.path("").join(second_name);
    for directory in [&first, &second] {
        fs::create_dir(directory).unwrap();
        fs::write(directory.join("tool"), b"").unwrap();
        fs::set_permissions(directory.join("tool"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (a, b) in [(&first, &second), (&second, &first)] {
        let mut path = a.as_os_str().to_owned();
        path.push(":");
        path.push(b);
        let result = select(
            &lookup("tool"),
            &[("PATH".into(), path)],
            &NativeProbe,
            Platform::host(),
        )
        .unwrap();
        assert_eq!(result.path, a.join("tool"));
    }
}

#[cfg(windows)]
#[test]
fn native_windows_pathext_order_explicit_paths_and_directory_rejection() {
    let dir = Directory::new();
    for name in [
        "tool.exe", "tool.cMd", "tool.ps1", "bare", "bare.cmd", "x.cmd",
    ] {
        fs::write(dir.path(name), b"contents are never launched").unwrap();
    }
    fs::create_dir(dir.path("x.exe")).unwrap();
    for (extensions, expected, kind) in [
        (".PS1;.CMD;.EXE", "tool.CMD", LaunchKind::Batch),
        (".EXE;.CMD", "tool.EXE", LaunchKind::Native),
    ] {
        let environment: Vec<(OsString, OsString)> = vec![
            ("Path".into(), dir.path("").into_os_string()),
            ("PATHEXT".into(), extensions.into()),
        ];
        let result = select(
            &lookup("tool"),
            &environment,
            &NativeProbe,
            Platform::Windows,
        )
        .unwrap();
        assert_eq!(result.path, dir.path(expected));
        assert_eq!(result.launch_kind, kind);
        if extensions.starts_with(".PS1") {
            assert!(matches!(
                result.record.trace.skipped_candidates[0].reason,
                CandidateReason::UnsupportedExtension
            ));
        }
    }
    let environment: Vec<(OsString, OsString)> = vec![
        ("PATH".into(), dir.path("").into_os_string()),
        ("PATHEXT".into(), ".EXE;.CMD".into()),
    ];
    let result = select(
        &ProgramRequest::Path(dir.path("bare")),
        &environment,
        &NativeProbe,
        Platform::Windows,
    )
    .unwrap();
    assert_eq!(result.path, dir.path("bare.CMD"));
    assert!(matches!(
        result.record.trace.skipped_candidates[0].reason,
        CandidateReason::UnsupportedExtension
    ));
    let result = select(&lookup("x"), &environment, &NativeProbe, Platform::Windows).unwrap();
    assert_eq!(result.path, dir.path("x.CMD"));
    assert!(matches!(
        result.record.trace.skipped_candidates[0].reason,
        CandidateReason::Directory
    ));
    let trace = select(
        &ProgramRequest::Path(dir.path("tool.ps1")),
        &environment,
        &NativeProbe,
        Platform::Windows,
    )
    .unwrap_err();
    assert!(matches!(
        trace.skipped_candidates[0].reason,
        CandidateReason::UnsupportedExtension
    ));
}
