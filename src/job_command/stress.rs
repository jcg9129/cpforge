//! Search command policy and frozen batch templates.
use super::*;
use crate::jobs::engine::Recipe;
use crate::jobs::engine::stress::{
    BatchTemplates, RunTemplate, Schedule, SearchBudget, StressRequest,
};
use crate::report::RoleName;
use std::cell::Cell;
use std::time::Duration;

pub(crate) struct StressSearch {
    seed: Cell<Option<u64>>,
    iterations: Option<u64>,
    time: Option<String>,
    keep_going: bool,
    deadline: Option<String>,
}
impl StressSearch {
    pub(crate) fn new(args: &crate::cli::StressArgs) -> Self {
        Self {
            seed: Cell::new(args.seed),
            iterations: args.iterations,
            time: args.time.clone(),
            keep_going: args.keep_going,
            deadline: args.deadline.clone(),
        }
    }
}
impl JobCommand for StressSearch {
    fn kind(&self) -> JobKind {
        JobKind::Stress
    }
    fn operation(&self) -> Operation {
        Operation::Stress
    }
    fn deadline_ms(&self) -> Result<Option<u64>, Error> {
        self.deadline
            .as_deref()
            .map(crate::config::parse_deadline)
            .transpose()
            .map_err(|e| Error::Config(format!("--deadline: {e}")))
    }
    fn stress_schedule(&self, config: &LoadedConfig) -> Result<Option<Schedule>, Error> {
        let stress = config
            .stress()
            .map_err(|e| Error::Config(e.to_string()))?
            .expect("validated stress");
        let start_seed = match self.seed.get() {
            Some(seed) => seed,
            None => {
                let mut bytes = [0; 8];
                getrandom::fill(&mut bytes)
                    .map_err(|e| Error::Infrastructure(format!("start seed: {e}")))?;
                let seed = u64::from_le_bytes(bytes) & (u64::MAX >> 1);
                self.seed.set(Some(seed));
                seed
            }
        };
        let iterations = self.iterations.unwrap_or(stress.iterations.value);
        if iterations == 0 || start_seed.checked_add(iterations - 1).is_none() {
            return Err(Error::Config(format!(
                "--seed {start_seed} with --iterations {iterations}: generator seeds would wrap"
            )));
        }
        let time = self
            .time
            .as_deref()
            .map(crate::config::parse_deadline)
            .transpose()
            .map_err(|e| Error::Config(format!("--time: {e}")))?
            .unwrap_or(stress.time_ms.value);
        Ok(Some(Schedule::Search {
            start_seed,
            budget: SearchBudget {
                iterations,
                time: Duration::from_millis(time),
            },
            keep_going: self.keep_going,
        }))
    }
    fn select_tests(
        &self,
        _: &Selected,
        _: &LoadedConfig,
        _: &TargetPlan,
    ) -> Result<Vec<Identifier>, Error> {
        Ok(Vec::new())
    }
    fn request(&self, _: &TargetPlan) -> JobRequest {
        JobRequest::Build
    }
    fn summary(&self, report: &FinalizedReport) -> Vec<String> {
        let data = report.data();
        let mut lines = if data.result.verdict == JobVerdict::BuildFailed {
            build_summary(report)
        } else {
            Vec::new()
        };
        if data
            .details
            .stress
            .as_ref()
            .is_some_and(|s| s.counters.not_saved != 0 && s.failures.is_empty())
        {
            lines.push(
                "failures kept in job iterations; saving failures is not available yet".into(),
            );
        }
        lines.push(format!(
            "stress: {}",
            crate::report::render_label(&label(
                LabelSubject::Job(data.result.verdict),
                &DisplayContext::default()
            ))
        ));
        lines
    }
}

fn recipe(resolution: &Resolution) -> Recipe {
    match resolution {
        Resolution::Resolved(stage) => Recipe::Resolved(Box::new(stage.clone())),
        Resolution::AwaitingProducer(stage) => Recipe::Deferred(Box::new(stage.clone())),
    }
}

pub(super) fn preflight_roles(
    config: &LoadedConfig,
    selected: &Selected,
    candidate: &TargetPlan,
) -> Result<(), Error> {
    let stress = config
        .stress()
        .map_err(|e| Error::Config(e.to_string()))?
        .expect("stress");
    let check = config
        .judging()
        .map_err(|e| Error::Config(e.to_string()))?
        .expect("judging defaults");
    let baseline = Baseline::capture();
    let retention = config
        .retention()
        .map_err(|e| Error::Config(e.to_string()))?;
    if !matches!(candidate.run.cwd.value, crate::config::Cwd::Run) {
        return Err(Error::Config(
            "stress candidate run cwd must select the private run directory".into(),
        ));
    }
    crate::storage::inputs::sources::current_source_files(candidate, &selected.dir)
        .map_err(|e| Error::Config(e.to_string()))?;
    let extra = [
        (RoleName::Generator, Some(stress.generator.as_ref())),
        (RoleName::Brute, stress.brute.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, role)| role.map(|role| (name, role)));
    for (name, spec) in crate::storage::inputs::roles::declarations(check).chain(extra) {
        let target = TargetPlan::for_role(spec);
        crate::storage::inputs::sources::validate_declarations(
            &crate::storage::inputs::roles::files(spec),
            &selected.dir,
        )
        .map_err(|e| Error::Config(format!("{} role: {e}", name.wire_token())))?;
        for stage in target.build.iter().chain(std::iter::once(&target.run)) {
            let errors = exec::limit_errors(&stage.limits);
            if !errors.is_empty() {
                return Err(Error::Config(format!(
                    "{} role: {}",
                    name.wire_token(),
                    errors.join("\n")
                )));
            }
        }
        // A prebuilt role executes its exclusive captured copy, made executable
        // during freezing. Its fixed argv has no external program to probe;
        // the declarations above are checked before admission.
        if matches!(spec.input, crate::config::RoleInput::Prebuilt(_)) {
            continue;
        }
        preflight(
            selected,
            &target,
            &baseline,
            &problem_binding(selected),
            retention.build_bytes.value,
            Some(name.wire_token()),
        )
        .map_err(|e| Error::Config(format!("{} role: {e}", name.wire_token())))?;
    }
    Ok(())
}

pub(super) fn prepare_request(
    ready: &crate::jobs::ReadyExecutionJob<'_>,
    config: &LoadedConfig,
    schedule: Schedule,
    baseline: &Baseline,
) -> Result<StressRequest, Error> {
    let target = ready.target.as_ref().expect("stress target");
    let judging = ready.judging.as_ref().expect("stress roles");
    let candidate_ctx = Context::target_run(
        target,
        InvocationDirs {
            run_cwd: ready.dir.join("iterations").join("0.candidate").join("cwd"),
            problem: ready.problem.clone(),
            workspace: Some(ready.workspace.clone()),
        },
        ready
            .planned_target
            .as_ref()
            .expect("target plan")
            .command_bindings(),
    )
    .map_err(|e| Error::Config(e.to_string()))?;
    let candidate = exec::resolve(
        exec::prepare(&target.run, &candidate_ctx, baseline, ready.platform, &[])
            .map_err(|e| Error::Config(e.to_string()))?,
        &NativeProbe,
        ready.platform,
    )
    .map_err(|e| Error::Config(e.to_string()))?;
    let template = |stage: Recipe, target: &TargetPlan| {
        RunTemplate::new(
            stage,
            target.run.stdout.bytes.value,
            target.run.stderr.bytes.value,
        )
        .map_err(|e| Error::Config(e.to_string()))
    };
    let role = |name| -> Result<Option<RunTemplate<Recipe>>, Error> {
        judging
            .roles
            .iter()
            .find(|r| r.captured.role == name)
            .map(|r| {
                template(
                    recipe(r.template.as_deref().expect("stress role template")),
                    &r.target,
                )
            })
            .transpose()
    };
    let templates = BatchTemplates::new(
        role(RoleName::Generator)?.expect("generator"),
        role(RoleName::Validator)?,
        role(RoleName::Brute)?,
        template(recipe(&candidate), target)?,
        role(RoleName::Checker)?,
    )
    .map_err(|e| Error::Config(e.to_string()))?;
    let mut request = StressRequest::planned(templates, schedule, target.compare.value.clone());
    request.validator_profile = judging
        .check
        .validator
        .as_ref()
        .map_or(crate::report::JudgeProfile::Simple, |r| r.profile.value);
    request.checker_profile = judging
        .check
        .checker
        .as_ref()
        .map_or(crate::report::JudgeProfile::Simple, |r| r.profile.value);
    request.scoring = judging.check.report_config().scoring;
    request.min_score = config
        .stress()
        .map_err(|e| Error::Config(e.to_string()))?
        .and_then(|s| s.min_score.as_ref())
        .map(|s| s.value);
    for (index, name) in [
        Some(RoleName::Generator),
        Some(RoleName::Validator),
        Some(RoleName::Brute),
        None,
        Some(RoleName::Checker),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(role) = judging.roles.iter().find(|r| Some(r.captured.role) == name) {
            request.assets[index] = role
                .captured
                .files
                .files()
                .iter()
                .filter(|f| f.role == crate::storage::inputs::SourceRole::Asset)
                .map(|file| crate::jobs::roles::StagedFile {
                    source: Some(file.copy_path.clone()),
                    bytes: None,
                    destination: file.problem_path.components().iter().collect(),
                })
                .collect();
        }
    }
    Ok(request)
}
