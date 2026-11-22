use super::*;
use crate::config::{
    ConfigRequest, ProgramValue, Sourced, StageCommand, TextValue, load_config, tokenize,
};
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "guard-{}",
                crate::build::BuildId::generate().unwrap()
            ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("config.toml"), "schema = 1\n").unwrap();
        Self(path)
    }
    fn stage(&self) -> StagePlan {
        load_config(&ConfigRequest {
            current_dir: self.0.clone(),
            config_path: Some(self.0.join("config.toml")),
            language: Some("cpp".into()),
            ..ConfigRequest::default()
        })
        .target()
        .unwrap()
        .build
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn paths(platform: Platform) -> (PathBuf, PathBuf, PathBuf) {
    let workspace = if platform == Platform::Windows {
        "C:/abs/ws"
    } else {
        "/abs/ws"
    };
    (
        workspace.into(),
        format!("{workspace}/.cpforge/runs/J").into(),
        format!("{workspace}/.cpforge/runs/J/cwd").into(),
    )
}

#[test]
fn contract_operand_vectors_on_every_platform() {
    let fixture = Fixture::new();
    let stage = fixture.stage();
    let shell_stage = shell_stage(&stage, Vec::new());
    // $W is the workspace spelling, $J the job spelling. Windows rows are
    // exercised on every host, independent of std::path's native interpretation.
    let rows = [
        ("g++", true),
        ("$W/compiler", false),
        ("$J/inputs/src/main.cpp", true),
        ("$J/builds/B/main", true),
        ("-I$W/lib", false),
        ("-L$W/lib", false),
        ("-o$W/output", false),
        ("-I$W/lib=/external", false),
        ("--include-dir=$W/lib", false),
        ("-I../../../lib", false),
        ("/backup/abs/ws/a.cpp", true),
        ("$W/../ext/a.cpp", true),
        ("$Wx/a.cpp", true),
        ("-I/usr/include", true),
        ("-Wl,-rpath,$W/lib", true),
        ("-Wl,-rpath,../../../../lib", false),
        ("-I../../../../../ext", false),
        ("$J/inputs/./src/../src/main.cpp", true),
        ("$J/../../../problem/main.cpp", false),
        ("--one=two=$W/lib", true), // Only the first equals is a delimiter.
        ("-I$J/inputs=../../../../lib", false), // Every candidate is checked.
        ("/external:$W/lib", true), // No PATH-list splitting.
        ("bare-name", true),
        ("..", true), // No separator: not a path operand.
    ];
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let (workspace, job, cwd) = paths(platform);
        for (element, accepted) in rows {
            let element = element
                .replace("$J", job.to_str().unwrap())
                .replace("$W", workspace.to_str().unwrap());
            for shell in [false, true] {
                let args = vec![element.clone().into()];
                let arguments = if shell {
                    ExpandedArguments::ShellArgs(args)
                } else {
                    ExpandedArguments::Argv(args)
                };
                let raw_stage = if shell { &shell_stage } else { &stage };
                let result = check_capture(raw_stage, &arguments, &cwd, &workspace, &job, platform);
                assert_eq!(
                    result.is_ok(),
                    accepted,
                    "{platform:?} {element:?}: {result:?}"
                );
                if accepted {
                    assert_eq!(
                        result.unwrap(),
                        if shell {
                            CaptureCheck::Unchecked {
                                reasons: vec![CaptureCheckReason::ShellStage],
                            }
                        } else {
                            CaptureCheck::Checked
                        }
                    );
                } else {
                    let error = result.unwrap_err();
                    assert!(error.to_string().contains("phase 3"));
                    assert!(
                        matches!(error, GuardError::Operand { element: actual, candidate } if actual.as_os_str() == std::ffi::OsStr::new(&element) && !candidate.is_empty())
                    );
                }
            }
        }
    }
}

#[test]
fn platform_separators_case_roots_and_component_boundaries() {
    let fixture = Fixture::new();
    let stage = fixture.stage();
    for (platform, workspace, job, cwd, element, accepted) in [
        (
            Platform::Windows,
            r"C:\Ws",
            r"C:\Ws\.cpforge\runs\J",
            r"C:\Ws\.cpforge\runs\J\cwd",
            r"/IC:\ws\lib",
            false,
        ),
        (
            Platform::Windows,
            r"C:\Ws",
            r"C:\Ws\J",
            r"C:\Ws\J\cwd",
            r"\Ws\lib",
            false,
        ),
        (
            Platform::Windows,
            r"C:\Ws",
            r"C:\Ws\.cpforge\runs\J",
            r"C:\Ws\.cpforge\runs\J\cwd",
            r"..\..\..\lib",
            false,
        ),
        (
            Platform::Windows,
            r"\\?\C:\Ws",
            r"\\?\C:\Ws\.cpforge\runs\J",
            r"\\?\C:\Ws\.cpforge\runs\J\cwd",
            r"C:\ws\lib",
            false,
        ),
        (
            Platform::Windows,
            r"\\host\share\Ws",
            r"\\host\share\Ws\J",
            r"\\host\share\Ws\J\cwd",
            r"\\HOST\SHARE\ws\lib",
            false,
        ),
        (
            Platform::Windows,
            r"\\?\UNC\host\share\Ws",
            r"\\?\UNC\host\share\Ws\J",
            r"\\?\UNC\host\share\Ws\J\cwd",
            r"\\host\share\Ws\lib",
            false,
        ),
        (
            Platform::Windows,
            r"C:\Ws",
            r"C:\Ws\J",
            r"C:\Ws\J\cwd",
            r"D:\Ws\lib",
            true,
        ),
        (
            Platform::Linux,
            "/abs/ws",
            "/abs/ws/J",
            "/abs/ws/J/cwd",
            "/ABS/WS/lib",
            true,
        ),
        (
            Platform::Macos,
            "/abs/ws",
            "/abs/ws/J",
            "/abs/ws/J/cwd",
            "/ABS/WS/lib",
            false,
        ),
        (
            Platform::Macos,
            "/abs/É",
            "/abs/É/J",
            "/abs/É/J/cwd",
            "/abs/é/lib",
            true,
        ),
        (
            Platform::Linux,
            "/abs/ws",
            "/abs/ws/J",
            "/abs/ws/J/cwd",
            r"..\..\lib",
            true,
        ),
        (
            Platform::Linux,
            "/abs/ws",
            "/abs/ws/J",
            "/abs/ws/J/cwd",
            "/abs/ws/J-other/main",
            false,
        ),
    ] {
        let result = check_capture(
            &stage,
            &ExpandedArguments::Argv(vec![element.into()]),
            Path::new(cwd),
            Path::new(workspace),
            Path::new(job),
            platform,
        );
        assert_eq!(
            result.is_ok(),
            accepted,
            "{platform:?} {element}: {result:?}"
        );
    }
}

fn text(stage: &StagePlan, value: &str) -> TextValue {
    let StageCommand::Argv(argv) = &stage.command else {
        panic!()
    };
    let mut text = argv[0].clone();
    text.tokens = tokenize(value).unwrap();
    text.path = None;
    text
}

fn shell_stage(base: &StagePlan, args: Vec<TextValue>) -> StagePlan {
    let mut stage = base.clone();
    stage.command = StageCommand::Shell {
        interpreter: ProgramValue {
            program: text(base, "sh"),
            args: vec![Sourced {
                value: "-c".into(),
                origin: base.cwd.origin.clone(),
                provenance: base.cwd.provenance.clone(),
                overlay: None,
            }],
        },
        script: Sourced {
            value: "cat {problem_dir}/main.cpp {workspace}/lib".into(),
            origin: base.cwd.origin.clone(),
            provenance: base.cwd.provenance.clone(),
            overlay: None,
        },
        args,
    };
    stage
}

#[test]
fn raw_tokens_cwd_and_shell_script_boundary() {
    let fixture = Fixture::new();
    for platform in [Platform::Linux, Platform::Windows, Platform::Macos] {
        let (workspace, job, cwd) = paths(platform);
        let base = fixture.stage();
        let args = ExpandedArguments::Argv(vec!["tool".into()]);
        for placeholder in ["{problem_dir}", "{workspace}"] {
            let shell = shell_stage(&base, vec![text(&base, placeholder)]);
            assert!(matches!(
                check_capture(
                    &shell,
                    &ExpandedArguments::ShellArgs(Vec::new()),
                    &cwd,
                    &workspace,
                    &job,
                    platform
                ),
                Err(GuardError::Placeholder(_))
            ));
            for in_cwd in [false, true] {
                let mut stage = base.clone();
                let value = text(&base, placeholder);
                if in_cwd {
                    stage.cwd.value = Cwd::Path(value);
                } else {
                    stage.command = StageCommand::Argv(vec![value]);
                }
                assert!(matches!(
                    check_capture(&stage, &args, &cwd, &workspace, &job, platform),
                    Err(GuardError::Placeholder(_))
                ));
            }
        }
        for selector in [Cwd::Problem, Cwd::Workspace] {
            let mut stage = base.clone();
            stage.cwd.value = selector;
            assert_eq!(
                check_capture(&stage, &args, &cwd, &workspace, &job, platform),
                Err(GuardError::CwdSelector)
            );
        }
        let mut stage = base.clone();
        stage.cwd.value = Cwd::Path(text(&base, "literal"));
        assert_eq!(
            check_capture(&stage, &args, &workspace, &workspace, &job, platform),
            Err(GuardError::CwdPath)
        );
        // The job must be spelled below the given workspace root.
        assert_eq!(
            check_capture(&base, &args, &cwd, &job, &workspace, platform),
            Err(GuardError::JobOutsideWorkspace)
        );
        // Escaped braces are literal tokens, even when they resemble a placeholder.
        stage.command = StageCommand::Argv(vec![text(&base, "{{workspace}}")]);
        assert_eq!(
            check_capture(&stage, &args, &cwd, &workspace, &job, platform).unwrap(),
            CaptureCheck::Checked
        );
    }
}

#[cfg(unix)]
#[test]
fn native_bytes_are_not_lossily_compared() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let stage = fixture.stage();
    let workspace = PathBuf::from(OsString::from_vec(b"/abs/ws-\xff".to_vec()));
    let job = workspace.join("J");
    let cwd = job.join("cwd");
    let inside = OsString::from_vec(b"--include=/abs/ws-\xff/lib".to_vec());
    let external = OsString::from_vec(b"/abs/ws-\xfe/lib".to_vec());
    assert!(
        check_capture(
            &stage,
            &ExpandedArguments::Argv(vec![inside]),
            &cwd,
            &workspace,
            &job,
            Platform::Linux
        )
        .is_err()
    );
    assert_eq!(
        check_capture(
            &stage,
            &ExpandedArguments::Argv(vec![external]),
            &cwd,
            &workspace,
            &job,
            Platform::Linux
        )
        .unwrap(),
        CaptureCheck::Checked
    );
}

#[test]
fn shell_build_sources_require_run_cwd() {
    let fixture = Fixture::new();
    let base = fixture.stage();
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let (workspace, job, cwd) = paths(platform);
        // Use the emulated platform's lexical spelling even on Windows hosts.
        let arguments = ExpandedArguments::ShellArgs(vec![
            format!("{}/inputs/src/a.cpp", job.display()).into(),
        ]);
        for selector in [Cwd::Run, Cwd::Workspace, Cwd::Problem] {
            let mut stage = shell_stage(&base, vec![text(&base, "{sources}")]);
            stage.cwd.value = selector.clone();
            let result = check_capture(&stage, &arguments, &cwd, &workspace, &job, platform);
            if selector == Cwd::Run {
                assert_eq!(
                    result.unwrap(),
                    CaptureCheck::Unchecked {
                        reasons: vec![CaptureCheckReason::ShellStage],
                    }
                );
            } else {
                let error = result.unwrap_err();
                assert_eq!(error, GuardError::CwdSelector);
                assert!(error.to_string().contains("phase 3"));
            }
        }
    }
}
