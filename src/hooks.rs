//! Hook authorization and preparation, using only the triggering snapshot.
use std::path::{Path, PathBuf};

use crate::config::{HookPlan, HookSettings, Platform};
use crate::error::Error;
use crate::exec::{
    self, Baseline, Context, InvocationDirs, NativeProbe, ProblemBinding, Resolution,
};
use crate::jobs::engine::{Iteration, Recipe, Role};

mod policy;
use policy::decision;

pub(crate) fn authorize(
    settings: &HookSettings,
    root: &Path,
    baseline: &Baseline,
) -> Result<(), &'static str> {
    // Same spelling rules as other path comparisons: verbatim prefixes are
    // stripped on both sides, then Windows names fold case-insensitively.
    let canonical = |path: &Path| {
        std::fs::canonicalize(path).ok().map(|p| {
            let path = crate::storage::paths::comparable(&p);
            if cfg!(windows) {
                PathBuf::from(crate::config::env_name_fold_os(
                    Platform::Windows,
                    path.as_os_str(),
                ))
            } else {
                path
            }
        })
    };
    let root = canonical(root);
    let trusted = settings
        .trusted_workspaces
        .iter()
        .flatten()
        // An entry that cannot be canonicalised (e.g. a deleted directory)
        // trusts nothing, but does not revoke the other entries.
        .filter_map(|p| canonical(&p.value))
        .collect::<Vec<_>>();
    let depth = baseline
        .entries()
        .iter()
        .rev()
        .find(|(name, _)| {
            if cfg!(windows) {
                name.as_encoded_bytes()
                    .eq_ignore_ascii_case(b"CPF_HOOK_DEPTH")
            } else {
                name == "CPF_HOOK_DEPTH"
            }
        })
        .map(|(_, value)| value.as_os_str());
    decision(
        settings.user_file_replaced,
        settings.enabled.as_ref().is_none_or(|v| v.value),
        depth,
        root.as_deref(),
        &trusted,
    )
}

pub(crate) fn prepare(
    plans: &[HookPlan],
    trigger: &str,
    job: &Path,
    workspace: &Path,
    problem: ProblemBinding,
    baseline: &Baseline,
) -> Result<Vec<Iteration>, Error> {
    plans
        .iter()
        .map(|plan| {
            let context = Context::hook(
                plan,
                InvocationDirs {
                    run_cwd: job.join("run/cwd"),
                    workspace: Some(workspace.to_owned()),
                    problem: Some(problem.clone()),
                },
            )
            .map_err(|e| Error::Config(e.to_string()))?;
            let prepared = exec::prepare(&plan.stage, &context, baseline, Platform::host(), &[])
                .map_err(|e| Error::Config(e.to_string()))?;
            let resolution = exec::resolve(prepared, &NativeProbe, Platform::host())
                .map_err(|e| Error::Config(e.to_string()))?;
            let recipe = match resolution {
                Resolution::Resolved(stage) => {
                    stage
                        .launch_command(Platform::host())
                        .map_err(|e| Error::Config(e.to_string()))?;
                    Recipe::Resolved(Box::new(stage))
                }
                Resolution::AwaitingProducer(_) => {
                    return Err(Error::Config("hook requires a generated program".into()));
                }
            };
            Ok(Iteration {
                role: Role::Hook {
                    name: plan.name.value.clone(),
                    trigger: trigger.into(),
                },
                recipe,
                stdin: None,
                depends_on_build: false,
            })
        })
        .collect()
}
