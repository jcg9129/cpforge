use std::num::NonZeroU64;
use std::path::PathBuf;

use crate::config::{
    Artifacts, CommandPlan, HookPlan, Placeholder, Sourced, StageKind, StagePlan, TargetPlan,
};

use super::{PrepareError, PrepareErrorKind as Reason, PrepareErrors};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemBinding {
    pub dir: PathBuf,
    pub id: String,
}

/// Selector directories captured by the caller, without reading current state.
/// All paths must be absolute for preparation's platform. `run_cwd` is the fresh
/// directory allocated for this invocation by the job/storage caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationDirs {
    pub run_cwd: PathBuf,
    pub problem: Option<ProblemBinding>,
    pub workspace: Option<PathBuf>,
}

/// Absolute paths allocated by the caller, not checked for existence here.
/// Sources are captured paths when capture was performed; no original-source
/// fallback exists. `artifact` is present iff the target has a file artifact;
/// `artifact_dir` iff it has a build; `entry` iff the language declares one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetBindings {
    pub sources: Vec<PathBuf>,
    pub artifact: Option<PathBuf>,
    pub artifact_dir: Option<PathBuf>,
    pub entry: Option<String>,
}

/// Frozen absolute test paths supplied by job admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestBinding {
    pub input: PathBuf,
    pub answer: Option<PathBuf>,
}

/// Language declarations available independently of a build dependency.
/// Constructed from a selected target so supplying an arbitrary entry value
/// cannot make an undeclared language entry available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageBinding {
    entry: Option<String>,
}

impl LanguageBinding {
    pub fn from_target(plan: &TargetPlan) -> Self {
        Self {
            entry: plan.entry.as_ref().map(|entry| entry.value.clone()),
        }
    }
}

/// Bindings for a command invocation. `language` permits `{entry}` independently
/// of `needs_build`: §5.2 gives entry no build declaration gate. Without this
/// binding, the entry in declared target bindings is used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandBindings {
    pub target: Option<TargetBindings>,
    pub test: Option<TestBinding>,
    pub iteration: Option<NonZeroU64>,
    pub language: Option<LanguageBinding>,
}

/// An invocation's declarations and bindings. Private fields ensure that a
/// supplied value cannot grant an undeclared placeholder. Construction errors
/// accumulate just like expansion errors. Paths are validated when used in
/// preparation, using its explicit platform (including cross-platform tests).
#[derive(Clone, Debug, PartialEq)]
pub struct Context {
    pub(super) plan: StagePlan,
    pub(super) dirs: InvocationDirs,
    target: Option<TargetBindings>,
    entry: Option<String>,
    test: Option<TestBinding>,
    iteration: Option<NonZeroU64>,
    target_declared: bool,
    test_declared: bool,
    repeat_declared: bool,
    pub(super) identity_requirement: Option<Sourced<bool>>,
}

impl Context {
    pub fn target_build(
        plan: &TargetPlan,
        dirs: InvocationDirs,
        bindings: TargetBindings,
    ) -> Result<Self, PrepareErrors> {
        let Some(stage) = &plan.build else {
            let mut error = binding_error(&plan.run, &plan.run.cwd, "build", Reason::NoBuild);
            error.stage = StageKind::Build;
            return Err(PrepareErrors {
                errors: vec![error],
            });
        };
        Self::for_target(stage, plan, dirs, bindings)
    }

    pub fn target_run(
        plan: &TargetPlan,
        dirs: InvocationDirs,
        bindings: TargetBindings,
    ) -> Result<Self, PrepareErrors> {
        Self::for_target(&plan.run, plan, dirs, bindings)
    }

    fn for_target(
        stage: &StagePlan,
        plan: &TargetPlan,
        dirs: InvocationDirs,
        bindings: TargetBindings,
    ) -> Result<Self, PrepareErrors> {
        let mut errors: Vec<PrepareError> = Vec::new();
        if bindings.artifact_dir.is_some() != plan.build.is_some() {
            errors.push(binding_error(
                stage,
                &stage.cwd,
                "artifact_dir",
                Reason::InvalidBinding("artifact_dir must be supplied iff the target has a build"),
            ));
        }
        if bindings.artifact.is_some() != matches!(plan.artifacts, Some(Artifacts::File { .. })) {
            errors.push(binding_error(
                stage,
                &stage.cwd,
                "artifact",
                Reason::InvalidBinding(
                    "artifact must be supplied iff the target has a file artifact",
                ),
            ));
        }
        if bindings.entry.as_deref() != plan.entry.as_ref().map(|entry| entry.value.as_str()) {
            errors.push(binding_error(
                stage,
                &stage.cwd,
                "entry",
                Reason::InvalidBinding("entry binding must match the language declaration"),
            ));
        }
        if errors.is_empty() {
            Ok(Self {
                plan: stage.clone(),
                dirs,
                entry: bindings.entry.clone(),
                target: Some(bindings),
                test: None,
                iteration: None,
                target_declared: true,
                test_declared: false,
                repeat_declared: false,
                identity_requirement: None,
            })
        } else {
            Err(PrepareErrors { errors })
        }
    }

    /// A target binding is accepted only with `needs_build = true`; test and
    /// iteration bindings likewise require `for_each_test = true` and an
    /// explicitly present `repeat` (including `repeat = 1`). Declared bindings
    /// are required even if the stage happens not to reference them.
    pub fn command(
        plan: &CommandPlan,
        dirs: InvocationDirs,
        bindings: CommandBindings,
    ) -> Result<Self, PrepareErrors> {
        let CommandBindings {
            target,
            test,
            iteration,
            language,
        } = bindings;
        let entry = language.map_or_else(
            || target.as_ref().and_then(|t| t.entry.clone()),
            |language| language.entry,
        );
        let mut errors: Vec<PrepareError> = Vec::new();
        let repeat_declared = plan.stage.explicit_fields.contains("repeat");
        for (declared, supplied, field, declaration, origin, provenance) in [
            (
                plan.needs_build.value,
                target.is_some(),
                "target",
                "needs_build = true",
                &plan.needs_build.origin,
                &plan.needs_build.provenance,
            ),
            (
                plan.for_each_test.value,
                test.is_some(),
                "test",
                "for_each_test = true",
                &plan.for_each_test.origin,
                &plan.for_each_test.provenance,
            ),
            (
                repeat_declared,
                iteration.is_some(),
                "iteration",
                "repeat",
                &plan.repeat.origin,
                &plan.repeat.provenance,
            ),
        ] {
            let kind = if supplied && !declared {
                Some(Reason::Undeclared { declaration })
            } else if declared && !supplied {
                Some(Reason::MissingBinding(field))
            } else {
                None
            };
            if let Some(kind) = kind {
                errors.push(PrepareError {
                    stage: plan.stage.kind,
                    field: field.into(),
                    placeholder: None,
                    kind,
                    origin: origin.clone(),
                    provenance: provenance.clone(),
                });
            }
        }
        if repeat_declared && iteration.is_some_and(|i| i.get() > plan.repeat.value) {
            errors.push(binding_error(
                &plan.stage,
                &plan.repeat,
                "iteration",
                Reason::InvalidBinding("iteration exceeds the declared repeat count"),
            ));
        }
        if plan.stage.kind != StageKind::Command {
            errors.push(binding_error(
                &plan.stage,
                &plan.stage.cwd,
                "context",
                Reason::ContextMismatch,
            ));
        }
        if errors.is_empty() {
            Ok(Self {
                plan: plan.stage.clone(),
                dirs,
                target,
                entry,
                test,
                iteration,
                target_declared: plan.needs_build.value,
                test_declared: plan.for_each_test.value,
                repeat_declared,
                identity_requirement: if plan.needs_build.value {
                    Some(plan.needs_build.clone())
                } else if plan.for_each_test.value {
                    Some(plan.for_each_test.clone())
                } else {
                    None
                },
            })
        } else {
            Err(PrepareErrors { errors })
        }
    }

    /// Hooks expose only problem/workspace identity; there are no parameters
    /// through which target, test or repeat values could be supplied.
    pub fn hook(plan: &HookPlan, dirs: InvocationDirs) -> Result<Self, PrepareErrors> {
        if plan.stage.kind != StageKind::Hook {
            return Err(PrepareErrors {
                errors: vec![binding_error(
                    &plan.stage,
                    &plan.stage.cwd,
                    "context",
                    Reason::ContextMismatch,
                )],
            });
        }
        Ok(Self {
            plan: plan.stage.clone(),
            dirs,
            target: None,
            entry: None,
            test: None,
            iteration: None,
            target_declared: false,
            test_declared: false,
            repeat_declared: false,
            identity_requirement: None,
        })
    }

    pub(super) fn sources(&self) -> Result<&[PathBuf], Reason> {
        self.target().map(|target| target.sources.as_slice())
    }

    fn target(&self) -> Result<&TargetBindings, Reason> {
        if self.plan.kind == StageKind::Hook {
            return Err(Reason::HookPlaceholder);
        }
        if !self.target_declared {
            return Err(Reason::Undeclared {
                declaration: "needs_build = true",
            });
        }
        self.target.as_ref().ok_or(Reason::MissingBinding("target"))
    }

    pub(super) fn scalar(&self, placeholder: Placeholder) -> Result<std::ffi::OsString, Reason> {
        use Placeholder as P;
        if self.plan.kind == StageKind::Hook
            && !matches!(placeholder, P::ProblemDir | P::ProblemId | P::Workspace)
        {
            return Err(Reason::HookPlaceholder);
        }
        match placeholder {
            P::Sources => Err(Reason::MultiInScalar),
            P::Source => match self.sources()? {
                [source] => Ok(source.as_os_str().to_owned()),
                sources => Err(Reason::SourceCount(sources.len())),
            },
            P::Artifact => self
                .target()?
                .artifact
                .as_ref()
                .map(|p| p.as_os_str().to_owned())
                .ok_or(Reason::NoFileArtifact),
            P::ArtifactDir => self
                .target()?
                .artifact_dir
                .as_ref()
                .map(|p| p.as_os_str().to_owned())
                .ok_or(Reason::NoBuild),
            P::Entry => self
                .entry
                .as_ref()
                .map(std::ffi::OsString::from)
                .ok_or(Reason::NoEntry),
            P::ProblemDir => self
                .dirs
                .problem
                .as_ref()
                .map(|p| p.dir.as_os_str().to_owned())
                .ok_or(Reason::NoProblem),
            P::ProblemId => self
                .dirs
                .problem
                .as_ref()
                .map(|p| std::ffi::OsString::from(&p.id))
                .ok_or(Reason::NoProblem),
            P::Workspace => self
                .dirs
                .workspace
                .as_ref()
                .map(|p| p.as_os_str().to_owned())
                .ok_or(Reason::NoWorkspace),
            P::TestInput | P::TestAnswer | P::Iteration if self.plan.kind != StageKind::Command => {
                Err(Reason::CommandOnly)
            }
            P::TestInput | P::TestAnswer => {
                if !self.test_declared {
                    return Err(Reason::Undeclared {
                        declaration: "for_each_test = true",
                    });
                }
                let test = self.test.as_ref().ok_or(Reason::MissingBinding("test"))?;
                if placeholder == P::TestInput {
                    Ok(test.input.as_os_str().to_owned())
                } else {
                    test.answer
                        .as_ref()
                        .map(|p| p.as_os_str().to_owned())
                        .ok_or(Reason::NoTestAnswer)
                }
            }
            P::Iteration => {
                if !self.repeat_declared {
                    return Err(Reason::Undeclared {
                        declaration: "repeat",
                    });
                }
                self.iteration
                    .map(|i| i.to_string().into())
                    .ok_or(Reason::MissingBinding("iteration"))
            }
        }
    }
}

fn binding_error<T>(
    stage: &StagePlan,
    value: &Sourced<T>,
    field: &str,
    kind: Reason,
) -> PrepareError {
    PrepareError {
        stage: stage.kind,
        field: field.into(),
        placeholder: None,
        kind,
        origin: value.origin.clone(),
        provenance: value.provenance.clone(),
    }
}
