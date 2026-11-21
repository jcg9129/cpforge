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

#[cfg(all(test, windows))]
mod tests {
    use super::authorize;
    use crate::config::{HookSettings, Origin, Provenance, Sourced};
    use crate::exec::Baseline;
    use std::fs;
    use std::path::{Path, PathBuf};

    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn windows_trust_path_spellings() {
        let directory = Directory(Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(
            format!("hooks-trust-{}", crate::build::BuildId::generate().unwrap()),
        ));
        let root = directory.0.join("WorkSpace");
        let sibling = directory.0.join("WorkSpace-other");
        let child = root.join("child");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        let verbatim = fs::canonicalize(&root).unwrap();
        assert!(verbatim.to_str().unwrap().starts_with(r"\\?\"));
        let plain = crate::storage::paths::plain(&verbatim);
        let text = plain.to_str().unwrap();
        let variants = [
            PathBuf::from(text.to_ascii_uppercase()),
            PathBuf::from(text.to_ascii_lowercase()),
            PathBuf::from(text.replace('\\', "/")),
            verbatim.clone(),
            PathBuf::from(verbatim.to_str().unwrap().to_ascii_uppercase()),
        ];
        let baseline = Baseline::default();
        for trusted in variants {
            let settings = HookSettings {
                trusted_workspaces: Some(vec![Sourced {
                    value: trusted.clone(),
                    origin: Origin {
                        file: directory.0.join("user.toml"),
                        line: Some(3),
                        col: Some(1),
                        key_path: "hooks.trusted_workspaces[0]".into(),
                        span: None,
                    },
                    provenance: Provenance::File {
                        path: directory.0.join("user.toml"),
                        line: Some(3),
                        col: Some(1),
                    },
                    overlay: None,
                }]),
                ..HookSettings::default()
            };
            for spelling in [&plain, &verbatim] {
                assert_eq!(
                    authorize(&settings, spelling, &baseline),
                    Ok(()),
                    "{trusted:?}"
                );
            }
            for different in [&sibling, &child] {
                assert_eq!(
                    authorize(&settings, different, &baseline),
                    Err("untrusted"),
                    "{trusted:?} vs {different:?}"
                );
            }
        }
    }
}
