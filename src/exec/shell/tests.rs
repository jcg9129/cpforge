use std::io;
use std::path::Path;

use super::*;
use crate::config::StageCommand;
use crate::exec::tests::{Directory, command_context};
use crate::exec::{Baseline, Executable, Probe, ProbeResult, Resolution, prepare, resolve};

struct Present;

impl Probe for Present {
    fn probe(&self, _: &Path) -> io::Result<ProbeResult> {
        Ok(ProbeResult::Regular(Executable::Yes))
    }
}

fn shell(dir: &Directory, path: &str, platform: Platform) -> ResolvedStage {
    let plan = dir.command(r#"shell = { interpreter = ['sh', '-c'], script = 'printf "{x}" "$@"', args = ['a b', '', '{{x}}'] }"#);
    let ctx = command_context(&plan, dir.dirs());
    let mut prepared = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &["{sources}".into(), "--x=$y".into()],
    )
    .unwrap();
    // Set the selected candidate through the request, never the host's Path
    // parsing, so Windows paths exercise the target platform on every host.
    let crate::exec::PreparedCommand::Shell { interpreter, .. } = &mut prepared.command else {
        unreachable!()
    };
    interpreter.program.path = Some(path.into());
    interpreter.program.lookup = false;
    let Resolution::Resolved(stage) = resolve(prepared, &Present, platform).unwrap() else {
        panic!("external interpreter must resolve immediately")
    };
    stage
}

#[test]
fn lowering_preserves_script_and_argument_order() {
    let dir = Directory::new();
    let stage = shell(&dir, "/bin/sh", Platform::Linux);
    let command = stage.launch_command(Platform::Linux).unwrap();
    assert_eq!(command.program, Path::new("/bin/sh"));
    assert_eq!(command.launch_kind, LaunchKind::Native);
    let expected: Vec<OsString> = [
        "-c",
        "printf \"{x}\" \"$@\"",
        "cpf",
        "a b",
        "",
        "{x}",
        "{sources}",
        "--x=$y",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    assert_eq!(command.args, expected);
}

#[test]
fn basename_uses_selected_path_and_target_platform() {
    for platform in [Platform::Linux, Platform::Macos] {
        for path in [
            "/bin/sh",
            "/usr/bin/bash",
            "/usr/local/bin/zsh",
            "ksh",
            "dash",
        ] {
            assert!(interpreter_name(OsStr::new(path), platform), "{path}");
        }
        for path in [
            "/bin/fish",
            "/usr/bin/python3",
            "/bin/shx",
            "sh.exe",
            "/bin/SH",
            r"/bin/other\sh",
        ] {
            assert!(!interpreter_name(OsStr::new(path), platform), "{path}");
        }
    }
    for path in [r"C:\Git\bin\SH.EXE", "bash.exe", r"C:/bin/zsh.ExE", "KSH"] {
        assert!(
            interpreter_name(OsStr::new(path), Platform::Windows),
            "{path}"
        );
    }
    for path in ["sh.cmd", "sh.bat", "sh.com", "sh.exe.exe", "fish.exe"] {
        assert!(
            !interpreter_name(OsStr::new(path), Platform::Windows),
            "{path}"
        );
    }
}

#[test]
fn shell_policy_errors_name_path_and_interpreter_origin() {
    let dir = Directory::new();
    for (path, platform, kind) in [
        (
            "/bin/fish",
            Platform::Linux,
            LowerErrorKind::InterpreterName,
        ),
        (
            "/bin/sh.exe",
            Platform::Linux,
            LowerErrorKind::InterpreterName,
        ),
        (
            r"C:\bin\sh.cmd",
            Platform::Windows,
            LowerErrorKind::BatchInterpreter,
        ),
        (
            r"C:\bin\sh.bat",
            Platform::Windows,
            LowerErrorKind::BatchInterpreter,
        ),
    ] {
        let stage = shell(&dir, path, platform);
        let error = stage.launch_command(platform).unwrap_err();
        let StageCommand::Shell { interpreter, .. } = &stage.plan.command else {
            unreachable!()
        };
        assert_eq!(error.path, Path::new(path));
        assert_eq!(error.kind, kind);
        assert_eq!(error.stage, stage.kind);
        assert_eq!(error.origin, interpreter.program.origin);
        assert_eq!(error.provenance, interpreter.program.provenance);
        assert!(error.to_string().contains(path));
        assert!(error.to_string().contains(&error.origin.to_string()));
    }
    for path in [r"C:\Git\bin\SH.EXE", r"C:\bin\bash.exe"] {
        let command = shell(&dir, path, Platform::Windows)
            .launch_command(Platform::Windows)
            .unwrap();
        assert_eq!(command.program, Path::new(path));
    }
}

#[test]
fn argv_view_preserves_program_launch_kind_and_arguments() {
    let dir = Directory::new();
    let plan = dir.command("argv = ['tool', 'a b', '', '{{x}}']");
    let ctx = command_context(&plan, dir.dirs());
    let mut prepared = prepare(
        &plan.stage,
        &ctx,
        &Baseline::default(),
        Platform::host(),
        &[],
    )
    .unwrap();
    prepared.command = crate::exec::PreparedCommand::Argv {
        program: crate::exec::ProgramRequest::Path(r"C:\bin\tool.cmd".into()),
        args: vec!["a b".into(), "".into(), "{x}".into()],
    };
    let Resolution::Resolved(stage) = resolve(prepared, &Present, Platform::Windows).unwrap()
    else {
        unreachable!()
    };
    let command = stage.launch_command(Platform::Windows).unwrap();
    assert_eq!(command.program, Path::new(r"C:\bin\tool.cmd"));
    assert_eq!(command.launch_kind, LaunchKind::Batch);
    let ResolvedCommand::Argv { args, .. } = &stage.command else {
        unreachable!()
    };
    assert_eq!(&command.args, args);
}

#[test]
#[should_panic(expected = "C1 shell interpreter must be exactly")]
fn lowering_asserts_c1_interpreter_shape() {
    let dir = Directory::new();
    let mut stage = shell(&dir, "/bin/sh", Platform::Linux);
    let ResolvedCommand::Shell {
        interpreter_args, ..
    } = &mut stage.command
    else {
        unreachable!()
    };
    interpreter_args.push("-x".into());
    let _ = stage.launch_command(Platform::Linux);
}

#[cfg(unix)]
#[test]
fn symlink_named_sh_keeps_its_selected_name() {
    let dir = Directory::new();
    let target = dir.path("dash");
    std::fs::write(&target, "target").unwrap();
    let link = dir.path("sh");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(&target).unwrap()
    );
    let stage = shell(&dir, link.to_str().unwrap(), Platform::host());
    let command = stage.launch_command(Platform::host()).unwrap();
    assert_eq!(command.program, link);
}
