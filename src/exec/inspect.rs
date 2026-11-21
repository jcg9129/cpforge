//! Program-only inspection: never invent job paths or expand job arguments.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::config::{
    Placeholder, Platform, StageCommand, StageKind, StagePlan, TextValue, Token,
    syntax::{join, native_absolute},
};

use super::{
    Baseline, PreparedCommand, PreparedStage, Probe, ProgramRequest, Resolution, ResolvedCommand,
};

#[derive(Debug)]
pub(crate) enum ToolStatus {
    Found(PathBuf),
    Generated,
    Unavailable,
    NotFound(String),
}

/// Resolve just the external program, retaining the stage's environment and
/// interpreter policy. Argument placeholders do not determine tool availability.
#[allow(clippy::too_many_arguments)]
pub(crate) fn inspect_program(
    stage: &StagePlan,
    cwd: &Path,
    workspace: Option<&Path>,
    problem: Option<(&Path, &str)>,
    entry: Option<&str>,
    baseline: &Baseline,
    probe: &dyn Probe,
    platform: Platform,
) -> ToolStatus {
    if let StageCommand::Argv(argv) = &stage.command {
        let text = &argv[0];
        if text.tokens.iter().any(|token| {
            matches!(
                token,
                Token::Placeholder(Placeholder::Artifact | Placeholder::ArtifactDir)
            )
        }) {
            return ToolStatus::Generated;
        }
    }
    // A target compiler remains probeable when only its *arguments* need
    // captured sources. Command declarations can require a problem regardless
    // of their program (e.g. for_each_test outside a problem).
    if matches!(stage.kind, StageKind::Command | StageKind::Hook)
        && ((stage.context.problem && problem.is_none())
            || (stage.context.workspace && workspace.is_none()))
    {
        return ToolStatus::Unavailable;
    }
    let command = match &stage.command {
        StageCommand::Argv(argv) => {
            let text = &argv[0];
            let program = match program(text, workspace, problem, entry, platform) {
                Ok(program) => program,
                Err(status) => return status,
            };
            PreparedCommand::Argv {
                program,
                args: Vec::new(),
            }
        }
        StageCommand::Shell {
            interpreter,
            script,
            ..
        } => PreparedCommand::Shell {
            interpreter: interpreter.clone(),
            script: script.clone(),
            args: Vec::new(),
        },
    };
    let prepared = PreparedStage {
        kind: stage.kind,
        command,
        artifact_dir: None,
        // Resolver lookup ignores cwd: relative PATH entries are skipped by C2.
        cwd: cwd.to_owned(),
        shared_cwd: stage.shared_cwd(),
        environment: super::prepare::environment(baseline, &stage.environment, platform),
        deadline: stage.deadline.clone(),
        stdout: stage.stdout.clone(),
        stderr: stage.stderr.clone(),
        limits: stage.limits.clone(),
        plan: stage.clone(),
    };
    match super::resolve(prepared, probe, platform) {
        Ok(Resolution::Resolved(stage)) => {
            let program = match stage.command {
                ResolvedCommand::Argv { program, .. } => program,
                ResolvedCommand::Shell { interpreter, .. } => interpreter,
            };
            ToolStatus::Found(program.path)
        }
        Ok(Resolution::AwaitingProducer(_)) => ToolStatus::Generated,
        Err(error) => ToolStatus::NotFound(error.to_string()),
    }
}

/// Mirrors the program expansion of `prepare` for the placeholders known
/// outside a job; any other placeholder makes the program not checkable here.
fn program(
    text: &TextValue,
    workspace: Option<&Path>,
    problem: Option<(&Path, &str)>,
    entry: Option<&str>,
    platform: Platform,
) -> Result<ProgramRequest, ToolStatus> {
    if let Some(path) = &text.path {
        return Ok(ProgramRequest::Path(path.clone()));
    }
    let mut value = OsString::new();
    for (index, token) in text.tokens.iter().enumerate() {
        match token {
            Token::Literal(literal) => match text.expanded_home.as_ref().filter(|_| index == 0) {
                // Phase 1 captured the native home but left the leading ~/
                // (or ~\) in this literal. Never round-trip it through UTF-8.
                Some(home)
                    if literal.starts_with("~/")
                        || (platform == Platform::Windows && literal.starts_with("~\\")) =>
                {
                    value.push(join(home, OsStr::new(&literal[2..]), platform));
                }
                _ => value.push(literal),
            },
            Token::Placeholder(Placeholder::Workspace) => {
                value.push(workspace.ok_or(ToolStatus::Unavailable)?);
            }
            Token::Placeholder(Placeholder::ProblemDir) => {
                value.push(problem.ok_or(ToolStatus::Unavailable)?.0);
            }
            Token::Placeholder(Placeholder::ProblemId) => {
                value.push(problem.ok_or(ToolStatus::Unavailable)?.1);
            }
            Token::Placeholder(Placeholder::Entry) => {
                value.push(entry.ok_or(ToolStatus::Unavailable)?);
            }
            Token::Placeholder(_) => return Err(ToolStatus::Unavailable),
        }
    }
    if value.is_empty() {
        return Err(ToolStatus::NotFound("empty program".into()));
    }
    let bytes = value.as_encoded_bytes();
    let drive_relative = platform == Platform::Windows
        && bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && !native_absolute(&value, platform);
    if drive_relative {
        return Err(ToolStatus::NotFound(
            "drive-relative program paths are not allowed".into(),
        ));
    }
    let separator =
        bytes.contains(&b'/') || (platform == Platform::Windows && bytes.contains(&b'\\'));
    Ok(if !text.lookup && separator {
        ProgramRequest::Path(value.into())
    } else {
        ProgramRequest::Lookup(value)
    })
}
