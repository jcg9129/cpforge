use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::config::{
    Cwd, EnvEdit, Origin, Placeholder, Platform, Provenance, StageCommand, StageKind, StagePlan,
    TextValue, Token, env_name_fold_os,
    syntax::{join, native_absolute},
};

use super::{
    Baseline, Context, PrepareError, PrepareErrorKind as Reason, PrepareErrors, PreparedCommand,
    PreparedStage, ProgramRequest, contains_nul,
};

/// Expand one stage without filesystem access, program resolution or shell
/// lowering. Every independent value is visited even after another value fails.
/// CLI extra arguments are appended verbatim and never tokenized or rescanned.
pub fn prepare(
    stage: &StagePlan,
    ctx: &Context,
    baseline: &Baseline,
    platform: Platform,
    extra_args: &[OsString],
) -> Result<PreparedStage, PrepareErrors> {
    let mut expansion = Expansion {
        ctx,
        platform,
        stage: stage.kind,
        errors: Vec::new(),
    };
    if stage != &ctx.plan {
        expansion.error(
            "context",
            None,
            Reason::ContextMismatch,
            &stage.cwd.origin,
            &stage.cwd.provenance,
        );
    }
    let command = match &stage.command {
        StageCommand::Argv(argv) => {
            let program = if let Some(program) = argv.first() {
                expansion.program(program)
            } else {
                expansion.error(
                    "argv",
                    None,
                    Reason::EmptyCommand,
                    &stage.cwd.origin,
                    &stage.cwd.provenance,
                );
                None
            };
            let mut args = expansion.arguments(argv.get(1..).unwrap_or_default());
            expansion.extra(&mut args, extra_args, &stage.cwd.origin);
            program.map(|program| PreparedCommand::Argv { program, args })
        }
        StageCommand::Shell {
            interpreter,
            script,
            args,
        } => {
            let mut args = expansion.arguments(args);
            expansion.extra(&mut args, extra_args, &script.origin);
            Some(PreparedCommand::Shell {
                interpreter: interpreter.clone(),
                script: script.clone(),
                args,
            })
        }
    };
    let cwd = match &stage.cwd.value {
        Cwd::Run => Some(ctx.dirs.run_cwd.clone()),
        Cwd::Problem => match &ctx.dirs.problem {
            Some(problem) => Some(problem.dir.clone()),
            None => {
                expansion.error(
                    "cwd.selector",
                    None,
                    Reason::NoProblem,
                    &stage.cwd.origin,
                    &stage.cwd.provenance,
                );
                None
            }
        },
        Cwd::Workspace => match &ctx.dirs.workspace {
            Some(workspace) => Some(workspace.clone()),
            None => {
                expansion.error(
                    "cwd.selector",
                    None,
                    Reason::NoWorkspace,
                    &stage.cwd.origin,
                    &stage.cwd.provenance,
                );
                None
            }
        },
        Cwd::Path(text) => expansion.scalar(text).map(PathBuf::from),
    };
    if let Some(cwd) = &cwd {
        if contains_nul(cwd.as_os_str()) {
            expansion.error(
                "cwd",
                None,
                Reason::Nul,
                &stage.cwd.origin,
                &stage.cwd.provenance,
            );
        }
        if !native_absolute(cwd.as_os_str(), platform) {
            expansion.error(
                "cwd",
                None,
                Reason::NotAbsolute,
                &stage.cwd.origin,
                &stage.cwd.provenance,
            );
        }
    }
    // Declarations can require identity even without any placeholder (for
    // example needs_build = true with argv = ["echo"]). Existing value errors
    // already explain missing identity, so avoid a redundant context error.
    for (missing, kind, field) in [
        (
            (stage.context.problem || ctx.identity_requirement.is_some())
                && ctx.dirs.problem.is_none(),
            Reason::NoProblem,
            "context.problem",
        ),
        (
            (stage.context.workspace || ctx.identity_requirement.is_some())
                && ctx.dirs.workspace.is_none(),
            Reason::NoWorkspace,
            "context.workspace",
        ),
    ] {
        if missing && !expansion.errors.iter().any(|error| error.kind == kind) {
            let (origin, provenance) = if let Some(declaration) = &ctx.identity_requirement {
                (&declaration.origin, &declaration.provenance)
            } else if let Some(value) = stage.deferred.first() {
                (&value.origin, &value.provenance)
            } else {
                (&stage.cwd.origin, &stage.cwd.provenance)
            };
            expansion.error(field, None, kind, origin, provenance);
        }
    }
    let environment = environment(baseline, &stage.environment, platform);
    let artifact_dir = ctx.artifact_dir().cloned();
    if let Some(path) = &artifact_dir {
        for (invalid, reason) in [
            (contains_nul(path.as_os_str()), Reason::Nul),
            (
                !native_absolute(path.as_os_str(), platform),
                Reason::NotAbsolute,
            ),
        ] {
            if invalid {
                expansion.error(
                    "artifact_dir",
                    None,
                    reason,
                    &stage.cwd.origin,
                    &stage.cwd.provenance,
                );
            }
        }
    }
    if expansion.errors.is_empty() {
        Ok(PreparedStage {
            kind: stage.kind,
            command: command.expect("no program error"),
            artifact_dir,
            cwd: cwd.expect("no cwd error"),
            shared_cwd: stage.shared_cwd(),
            environment,
            deadline: stage.deadline.clone(),
            stdout: stage.stdout.clone(),
            stderr: stage.stderr.clone(),
            limits: stage.limits.clone(),
            plan: stage.clone(),
        })
    } else {
        Err(PrepareErrors {
            errors: expansion.errors,
        })
    }
}

struct Expansion<'a> {
    ctx: &'a Context,
    platform: Platform,
    stage: StageKind,
    errors: Vec<PrepareError>,
}

impl Expansion<'_> {
    fn error(
        &mut self,
        field: &str,
        placeholder: Option<Placeholder>,
        kind: Reason,
        origin: &Origin,
        provenance: &Provenance,
    ) {
        self.errors.push(PrepareError {
            stage: self.stage,
            field: field.into(),
            placeholder,
            kind,
            origin: origin.clone(),
            provenance: provenance.clone(),
        });
    }

    fn text_error(&mut self, text: &TextValue, placeholder: Option<Placeholder>, kind: Reason) {
        self.error(
            &text.origin.key_path,
            placeholder,
            kind,
            &text.origin,
            &text.provenance,
        );
    }

    fn check_value(&mut self, text: &TextValue, placeholder: Option<Placeholder>, value: &OsStr) {
        if contains_nul(value) {
            self.text_error(text, placeholder, Reason::Nul);
        }
        if matches!(
            placeholder,
            Some(
                Placeholder::Source
                    | Placeholder::Sources
                    | Placeholder::Artifact
                    | Placeholder::ArtifactDir
                    | Placeholder::ProblemDir
                    | Placeholder::Workspace
                    | Placeholder::TestInput
                    | Placeholder::TestAnswer
            )
        ) && !native_absolute(value, self.platform)
        {
            self.text_error(text, placeholder, Reason::NotAbsolute);
        }
    }

    fn scalar(&mut self, text: &TextValue) -> Option<OsString> {
        let before = self.errors.len();
        if let Some(path) = &text.path {
            self.check_value(text, None, path.as_os_str());
            return (before == self.errors.len()).then(|| path.as_os_str().to_owned());
        }
        let mut result = OsString::new();
        for (index, token) in text.tokens.iter().enumerate() {
            match token {
                Token::Literal(literal) => {
                    if let Some(home) = text.expanded_home.as_ref().filter(|_| index == 0) {
                        // Phase 1 captured the native home but left the leading
                        // ~/ (or ~\) in this literal. Never tokenize the home.
                        if literal.starts_with("~/")
                            || (self.platform == Platform::Windows && literal.starts_with("~\\"))
                        {
                            result.push(join(home, OsStr::new(&literal[2..]), self.platform));
                        } else {
                            result.push(literal);
                        }
                    } else {
                        result.push(literal);
                    }
                }
                Token::Placeholder(placeholder) => match self.ctx.scalar(*placeholder) {
                    Ok(value) => {
                        self.check_value(text, Some(*placeholder), &value);
                        result.push(value);
                    }
                    Err(kind) => self.text_error(text, Some(*placeholder), kind),
                },
            }
        }
        // Individual placeholders already received precise errors; this check
        // covers literal text and a captured native home without duplicating them.
        if contains_nul(&result)
            && !self.errors[before..]
                .iter()
                .any(|error| error.kind == Reason::Nul)
        {
            self.text_error(text, None, Reason::Nul);
        }
        (before == self.errors.len()).then_some(result)
    }

    fn arguments(&mut self, texts: &[TextValue]) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        for text in texts {
            if text.tokens == [Token::Placeholder(Placeholder::Sources)] {
                let ctx = self.ctx;
                match ctx.sources() {
                    Ok(sources) => {
                        for source in sources {
                            self.check_value(text, Some(Placeholder::Sources), source.as_os_str());
                            args.push(source.as_os_str().to_owned());
                        }
                    }
                    Err(kind) => self.text_error(text, Some(Placeholder::Sources), kind),
                }
            } else if let Some(value) = self.scalar(text) {
                args.push(value);
            }
        }
        args
    }

    fn program(&mut self, text: &TextValue) -> Option<ProgramRequest> {
        let value = self.scalar(text)?;
        if value.is_empty() {
            self.text_error(text, None, Reason::EmptyProgram);
            return None;
        }
        let bytes = value.as_encoded_bytes();
        let drive_relative = self.platform == Platform::Windows
            && bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && !native_absolute(&value, self.platform);
        // Even C:tool (with no separator) is an invalid drive-relative result.
        if drive_relative {
            self.text_error(text, None, Reason::NotAbsolute);
            return None;
        }
        let separator =
            bytes.contains(&b'/') || (self.platform == Platform::Windows && bytes.contains(&b'\\'));
        if text.path.is_some() || (!text.lookup && separator) {
            if !native_absolute(&value, self.platform) {
                self.text_error(text, None, Reason::NotAbsolute);
                None
            } else {
                Some(ProgramRequest::Path(value.into()))
            }
        } else {
            Some(ProgramRequest::Lookup(value))
        }
    }

    fn extra(&mut self, args: &mut Vec<OsString>, extra: &[OsString], origin: &Origin) {
        for (index, value) in extra.iter().enumerate() {
            if contains_nul(value) {
                self.error(
                    &format!("extra_args[{index}]"),
                    None,
                    Reason::Nul,
                    origin,
                    &Provenance::Cli { flag: "--".into() },
                );
            }
            args.push(value.clone());
        }
    }
}

fn environment(
    baseline: &Baseline,
    edits: &[EnvEdit],
    platform: Platform,
) -> Vec<(OsString, OsString)> {
    let mut entries: Vec<(OsString, OsString)> = Vec::new();
    for (name, value) in baseline.entries() {
        install(&mut entries, name.clone(), value.clone(), platform);
    }
    for edit in edits {
        match edit {
            EnvEdit::Remove(name) | EnvEdit::Unset(name) => {
                let folded = env_name_fold_os(platform, OsStr::new(&name.value));
                entries.retain(|(key, _): &(OsString, OsString)| {
                    env_name_fold_os(platform, key) != folded
                });
            }
            EnvEdit::Set { name, value } => install(
                &mut entries,
                name.value.clone().into(),
                value.value.clone().into(),
                platform,
            ),
        }
    }
    entries
}

fn install(
    entries: &mut Vec<(OsString, OsString)>,
    name: OsString,
    value: OsString,
    platform: Platform,
) {
    let folded = env_name_fold_os(platform, &name);
    if let Some(index) = entries
        .iter()
        .position(|(key, _): &(OsString, OsString)| env_name_fold_os(platform, key) == folded)
    {
        entries[index] = (name, value);
    } else {
        entries.push((name, value));
    }
}
