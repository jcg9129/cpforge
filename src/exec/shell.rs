//! Shell adapter (§5.3), using the selected path without filesystem access.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;

use crate::config::{Origin, Platform, Provenance, StageCommand, StageKind};

use super::{LaunchKind, ResolvedCommand, ResolvedStage, resolve::final_component};

/// Final program and argv, excluding argv[0]. Admission still supplies the
/// environment's CPF_* variables, stdin, sinks and normalized limits. This is
/// neither a supervisor spec nor a persisted execution spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: PathBuf,
    pub launch_kind: LaunchKind,
    pub args: Vec<OsString>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowerErrorKind {
    InterpreterName,
    BatchInterpreter,
}

/// Phase-3 shell policy failure, tied to the interpreter's C1 program leaf.
#[derive(Clone, Debug, PartialEq)]
pub struct LowerError {
    pub stage: StageKind,
    pub path: PathBuf,
    pub kind: LowerErrorKind,
    pub origin: Origin,
    pub provenance: Provenance,
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.kind {
            LowerErrorKind::InterpreterName => {
                "shell interpreter basename must be sh, bash, dash, zsh or ksh (Windows also permits .exe)"
            }
            LowerErrorKind::BatchInterpreter => "shell interpreter must have native launch kind",
        };
        write!(
            f,
            "{}: {:?} stage, {}: {reason}",
            self.origin,
            self.stage,
            self.path.display()
        )
    }
}

impl std::error::Error for LowerError {}

impl ResolvedStage {
    /// Obtain final argv using the same target platform passed to resolution.
    /// Argv stages pass through unchanged. Shell scripts remain literal and
    /// values enter only as positional parameters, with `$0` fixed to `cpf`.
    /// No canonicalisation, filesystem probing or program search occurs here.
    ///
    /// # Panics
    /// Panics if a shell stage violates C1's `[<p>, "-c"]` invariant or has
    /// lost its originating shell plan.
    pub fn launch_command(&self, platform: Platform) -> Result<LaunchCommand, Box<LowerError>> {
        let (program, args) = match &self.command {
            ResolvedCommand::Argv { program, args } => (program, args.clone()),
            ResolvedCommand::Shell {
                interpreter,
                interpreter_args,
                script,
                args,
            } => {
                assert!(
                    matches!(interpreter_args.as_slice(), [flag] if flag == "-c"),
                    "C1 shell interpreter must be exactly [<p>, -c]"
                );
                let kind = if interpreter.launch_kind != LaunchKind::Native {
                    Some(LowerErrorKind::BatchInterpreter)
                } else if !interpreter_name(interpreter.path.as_os_str(), platform) {
                    Some(LowerErrorKind::InterpreterName)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    let StageCommand::Shell {
                        interpreter: configured,
                        ..
                    } = &self.plan.command
                    else {
                        unreachable!("resolved shell retains its originating shell plan")
                    };
                    return Err(Box::new(LowerError {
                        stage: self.kind,
                        path: interpreter.path.clone(),
                        kind,
                        origin: configured.program.origin.clone(),
                        provenance: configured.program.provenance.clone(),
                    }));
                }
                let mut lowered = Vec::with_capacity(3 + args.len());
                lowered.extend_from_slice(interpreter_args);
                lowered.push(OsString::from(&script.value));
                lowered.push(OsString::from("cpf"));
                lowered.extend_from_slice(args);
                (interpreter, lowered)
            }
        };
        Ok(LaunchCommand {
            program: program.path.clone(),
            launch_kind: program.launch_kind,
            args,
        })
    }
}

fn interpreter_name(path: &OsStr, platform: Platform) -> bool {
    let mut name = final_component(path, platform);
    if platform == Platform::Windows {
        if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(b".exe") {
            name = &name[..name.len() - 4];
        }
        [b"sh".as_slice(), b"bash", b"dash", b"zsh", b"ksh"]
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed))
    } else {
        matches!(name, b"sh" | b"bash" | b"dash" | b"zsh" | b"ksh")
    }
}

#[cfg(test)]
mod tests;
