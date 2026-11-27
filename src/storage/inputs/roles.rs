//! Role capture uses exactly the solution capture path and accounting rules.
use super::*;
use crate::config::{CheckConfig, RoleInput, RoleSpec};
use crate::report::RoleName;

#[derive(Debug)]
pub struct CapturedRole {
    pub role: RoleName,
    pub spec: Box<RoleSpec>,
    pub files: CapturedSources,
}

#[derive(Debug)]
pub struct FrozenJudging {
    pub check: Box<CheckConfig>,
    pub roles: Vec<CapturedRole>,
}

/// Stable role order, also used for builds and fingerprint slots.
pub(crate) fn declarations(check: &CheckConfig) -> impl Iterator<Item = (RoleName, &RoleSpec)> {
    [
        (RoleName::Checker, check.checker.as_deref()),
        (RoleName::Validator, check.validator.as_deref()),
        (
            RoleName::Interactor,
            check.interactive.as_ref().map(|i| &i.interactor),
        ),
    ]
    .into_iter()
    .filter_map(|(name, spec)| spec.map(|spec| (name, spec)))
}

pub(crate) fn files(spec: &RoleSpec) -> Vec<CapturedSource> {
    let mut files: Vec<_> = match &spec.input {
        RoleInput::Built(built) => built
            .sources
            .iter()
            .map(|f| (SourceRole::Source, f))
            .chain(built.support_files.iter().map(|f| (SourceRole::Support, f)))
            .collect(),
        RoleInput::Prebuilt(prebuilt) => vec![(SourceRole::Program, &prebuilt.program)],
    };
    files.extend(spec.assets.iter().map(|f| (SourceRole::Asset, f)));
    files
        .into_iter()
        .map(|(role, file)| CapturedSource {
            role,
            problem_path: ProblemPath::from_role(file),
        })
        .collect()
}

impl JobInputs<'_> {
    /// Freeze tests, solution files, then every declared role. No process starts.
    pub fn freeze_judged(
        &self,
        store: &TestStore,
        ids: &[TestId],
        target: &crate::config::TargetPlan,
        check: &CheckConfig,
        problem_dir: &Path,
    ) -> Result<FrozenInputs, FreezeInputsError> {
        self.freeze_roles(store, ids, target, check, None, problem_dir)
    }

    pub(crate) fn freeze_stress(
        &self,
        store: &TestStore,
        target: &crate::config::TargetPlan,
        check: &CheckConfig,
        stress: &crate::config::StressConfig,
        problem_dir: &Path,
    ) -> Result<FrozenInputs, FreezeInputsError> {
        self.freeze_roles(store, &[], target, check, Some(stress), problem_dir)
    }

    #[allow(clippy::too_many_arguments)] // Shared frozen declarations for test and stress jobs.
    fn freeze_roles(
        &self,
        store: &TestStore,
        ids: &[TestId],
        target: &crate::config::TargetPlan,
        check: &CheckConfig,
        stress: Option<&crate::config::StressConfig>,
        problem_dir: &Path,
    ) -> Result<FrozenInputs, FreezeInputsError> {
        let mut frozen = self.freeze(
            store,
            ids,
            &CapturedSource::from_target(target),
            problem_dir,
        )?;
        if !check.has_roles() && stress.is_none() {
            return Ok(frozen);
        }
        let result = (|| {
            let mut roles = Vec::new();
            let extra = stress
                .into_iter()
                .flat_map(|s| {
                    [
                        (RoleName::Generator, Some(s.generator.as_ref())),
                        (RoleName::Brute, s.brute.as_deref()),
                    ]
                })
                .filter_map(|(name, spec)| spec.map(|s| (name, s)));
            for (role, spec) in declarations(check).chain(extra) {
                let prefix = Path::new("roles").join(role.wire_token());
                let captured = sources::capture_at(&files(spec), problem_dir, self, &prefix)?;
                #[cfg(unix)]
                if matches!(spec.input, RoleInput::Prebuilt(_)) {
                    use std::os::unix::fs::PermissionsExt;
                    let path = &captured.files()[0].copy_path;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                        .map_err(|e| StorageError::io(path, e))?;
                }
                roles.push(CapturedRole {
                    role,
                    spec: Box::new(spec.clone()),
                    files: captured,
                });
            }
            Ok::<_, CaptureError>(FrozenJudging {
                check: Box::new(check.clone()),
                roles,
            })
        })();
        frozen.judging = Some(Box::new(
            self.finish(result).map_err(FreezeInputsError::Sources)?,
        ));
        Ok(frozen)
    }
}
