//! Shared problem job pipeline: preflight, admission, capture, execution and delivery.
//!
//! Implement [`JobCommand`] to supply the kind, configuration operation, selected
//! tests, engine request and human summary. [`run`] owns the invocation lifecycle;
//! commands supply phase hooks (build uses `NoHooks`, test can publish facts/hooks).
//! No executor can see original source bindings or an incomplete capture.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::build::{self, PlanInputs};
use crate::cancel_signals::CancelSignals;
use crate::config::{
    CommandPlan, ConfigRequest, Environment, LoadedConfig, Operation, Platform, Stdin, TargetPlan,
    load_config,
};
use crate::error::Error;
use crate::exec::{
    self, Baseline, CommandBindings, Context, InvocationDirs, LanguageBinding, NativeProbe,
    ProblemBinding, Resolution, TargetBindings, TestBinding,
};
use crate::exit::{ExitCode, ExitInputs, exit_status};
use crate::jobs::engine::{
    CancelState, Engine, ExecutionRecord, FinalizedReport, JobIdentity, JobRequest, NoHooks,
    PhaseHooks, ReportPersistence, ReportPresentation,
};
use crate::jobs::persist::JobPersistence;
use crate::jobs::{PrepareJobInputs, ReadyError, prepare_build_job, prepare_job};
use crate::names::Identifier;
use crate::progress::ProgressWriter;
use crate::report::{
    BuildCause, BuildResult, DisplayContext, JobKind, JobVerdict, LabelSubject, Qualifier,
    RunClass, label, wire,
};
use crate::selection::{self, Selected};
use crate::storage::WorkspaceLock;
use crate::storage::admission::register_job;
use crate::storage::inputs::CapturedSource;
use crate::storage::prune::PrunePolicy;
use crate::tests_store::TestStore;
use crate::transport::{self, Delivery, MachineArgs, MachineMode};

mod stress;
mod warnings;
pub(crate) use stress::StressSearch;

/// Command-specific policy, evaluated before job registration. Test selection
/// must return stable catalog ids; the shared pipeline freezes that exact list.
pub trait JobCommand {
    fn kind(&self) -> JobKind;
    fn operation(&self) -> Operation;
    fn stress_schedule(
        &self,
        _config: &LoadedConfig,
    ) -> Result<Option<crate::jobs::engine::stress::Schedule>, Error> {
        Ok(None)
    }
    fn named(&self) -> Option<(&str, &[OsString])> {
        None
    }
    fn deadline_ms(&self) -> Result<Option<u64>, Error> {
        Ok(None)
    }
    fn select_tests(
        &self,
        selected: &Selected,
        config: &LoadedConfig,
        target: &TargetPlan,
    ) -> Result<Vec<Identifier>, Error>;
    fn request(&self, target: &TargetPlan) -> JobRequest;
    /// Construct callbacks once capture is complete, with the job identity and
    /// frozen manifest available for fact publication and after-test hooks.
    fn phase_hooks(
        &self,
        _ready: &crate::jobs::ReadyJob<'_>,
        _config: &LoadedConfig,
    ) -> Result<Box<dyn PhaseHooks>, Error> {
        Ok(Box::new(NoHooks))
    }
    /// Bounded by the stderr writer, one rendered line per entry.
    fn summary(&self, report: &FinalizedReport) -> Vec<String>;
}

#[derive(Debug)]
pub struct NamedCommand {
    pub name: String,
    pub extra: Vec<OsString>,
}
impl JobCommand for NamedCommand {
    fn kind(&self) -> JobKind {
        JobKind::Cmd
    }
    fn operation(&self) -> Operation {
        Operation::Other
    }
    fn named(&self) -> Option<(&str, &[OsString])> {
        Some((&self.name, &self.extra))
    }
    fn select_tests(
        &self,
        _selected: &Selected,
        _config: &LoadedConfig,
        _target: &TargetPlan,
    ) -> Result<Vec<Identifier>, Error> {
        unreachable!("named commands select from their command plan")
    }
    fn request(&self, _target: &TargetPlan) -> JobRequest {
        unreachable!("named commands bind iterations after freezing")
    }
    fn summary(&self, report: &FinalizedReport) -> Vec<String> {
        if report.data().result.verdict == JobVerdict::BuildFailed {
            return build_summary(report);
        }
        vec![crate::report::render_label(&label(
            LabelSubject::Job(report.data().result.verdict),
            &DisplayContext::default(),
        ))]
    }
}

#[derive(Debug)]
pub struct BuildCommand;
impl JobCommand for BuildCommand {
    fn kind(&self) -> JobKind {
        JobKind::Build
    }
    fn operation(&self) -> Operation {
        Operation::Other
    }
    fn select_tests(
        &self,
        _selected: &Selected,
        _config: &LoadedConfig,
        _target: &TargetPlan,
    ) -> Result<Vec<Identifier>, Error> {
        Ok(Vec::new())
    }
    fn request(&self, _target: &TargetPlan) -> JobRequest {
        JobRequest::Build
    }
    fn summary(&self, report: &FinalizedReport) -> Vec<String> {
        build_summary(report)
    }
}

/// Raw mode occupies the entire invocation; it never admits a comparison.
#[derive(Debug)]
pub struct RunCommand {
    pub deadline: Option<String>,
}
impl JobCommand for RunCommand {
    fn kind(&self) -> JobKind {
        JobKind::Run
    }
    fn operation(&self) -> Operation {
        Operation::Run
    }
    fn deadline_ms(&self) -> Result<Option<u64>, Error> {
        self.deadline
            .as_deref()
            .map(crate::config::parse_deadline)
            .transpose()
            .map_err(|e| Error::Config(format!("--deadline: {e}")))
    }
    fn select_tests(
        &self,
        _selected: &Selected,
        _config: &LoadedConfig,
        _target: &TargetPlan,
    ) -> Result<Vec<Identifier>, Error> {
        Ok(Vec::new())
    }
    fn request(&self, _target: &TargetPlan) -> JobRequest {
        use crate::jobs::engine::{CommandKind, Iteration, Recipe, Role};
        JobRequest::Iterations {
            kind: CommandKind::Run,
            iterations: vec![Iteration {
                role: Role::Raw,
                recipe: Recipe::TargetRun,
                stdin: None,
                depends_on_build: true,
            }],
            needs_build: true,
            keep_going: true,
        }
    }
    fn summary(&self, report: &FinalizedReport) -> Vec<String> {
        let data = report.data();
        let raw = data
            .executions
            .iter()
            .find(|e| matches!(e.plan.role, crate::jobs::engine::Role::Raw));
        let class = raw.and_then(|e| e.result.class.as_ref());
        if matches!(class, Some(RunClass::ExitedZero)) {
            return Vec::new();
        }
        // Only the raw execution's termination is "the child's" (D2); a
        // build or a run that never started is named by the job verdict.
        let execution = raw.filter(|e| e.result.class.is_some());
        let subject = if data.result.verdict == JobVerdict::InternalError {
            LabelSubject::Job(data.result.verdict)
        } else if let Some(build) = data
            .build
            .as_ref()
            .filter(|_| data.result.verdict == JobVerdict::BuildFailed)
        {
            LabelSubject::Build(&build.result)
        } else {
            execution.map_or(LabelSubject::Job(data.result.verdict), |e| {
                LabelSubject::Run(&e.result)
            })
        };
        let display = label(
            subject,
            &DisplayContext {
                wall_deadline: execution
                    .and_then(|e| e.summary.as_ref())
                    .map(|s| s.deadline),
                ..DisplayContext::default()
            },
        );
        let mut line = crate::report::render_label(&display);
        if let Some(ExecutionRecord::Started(root)) = execution.and_then(|e| e.evidence.as_deref())
        {
            line.push_str(&match root.status {
                crate::supervisor::Termination::Exited { code } => {
                    format!(" | child exited with code {code}")
                }
                crate::supervisor::Termination::Signalled {
                    signal,
                    core_dumped,
                } => format!(
                    " | child killed by signal {signal}{}",
                    if core_dumped { " (core dumped)" } else { "" }
                ),
                crate::supervisor::Termination::Unknown => " | child status unknown".into(),
            });
        }
        vec![line]
    }
}

#[derive(Debug)]
pub struct TestRunCommand {
    cases: Vec<String>,
    deadline: Option<String>,
}
impl TestRunCommand {
    pub fn new(cases: Vec<String>, deadline: Option<String>) -> Result<Self, Error> {
        let mut seen = std::collections::BTreeSet::new();
        for id in &cases {
            if !seen.insert(id) {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!("duplicate --case {id:?}"),
                )
                .into());
            }
        }
        Ok(Self { cases, deadline })
    }
}
impl JobCommand for TestRunCommand {
    fn phase_hooks(
        &self,
        ready: &crate::jobs::ReadyJob<'_>,
        _config: &LoadedConfig,
    ) -> Result<Box<dyn PhaseHooks>, Error> {
        Ok(Box::new(crate::jobs::facts::TestFacts::new(ready)))
    }
    fn kind(&self) -> JobKind {
        JobKind::Test
    }
    fn operation(&self) -> Operation {
        Operation::Test
    }
    fn deadline_ms(&self) -> Result<Option<u64>, Error> {
        self.deadline
            .as_deref()
            .map(crate::config::parse_deadline)
            .transpose()
            .map_err(|e| Error::Config(format!("--deadline: {e}")))
    }
    fn select_tests(
        &self,
        selected: &Selected,
        _config: &LoadedConfig,
        _target: &TargetPlan,
    ) -> Result<Vec<Identifier>, Error> {
        let catalog = TestStore::new(selected.dir.clone())
            .catalog()
            .map_err(Error::Workspace)?;
        let entries = if self.cases.is_empty() {
            catalog
                .entries
                .iter()
                .filter(|e| e.enabled)
                .collect::<Vec<_>>()
        } else {
            let find = |id: &String| {
                catalog
                    .entries
                    .iter()
                    .find(|entry| entry.id.as_str() == id)
                    .ok_or_else(|| {
                        Error::Workspace(format!(
                            "unknown registered test id {id:?}; register candidates with cpf test register first"
                        ))
                    })
            };
            self.cases.iter().map(find).collect::<Result<Vec<_>, _>>()?
        };
        refuse_test_overrides(entries.iter().copied())?;
        Ok(entries.into_iter().map(|entry| entry.id.clone()).collect())
    }
    fn request(&self, target: &TargetPlan) -> JobRequest {
        JobRequest::Test {
            comparator: target.compare.value.clone(),
        }
    }
    fn summary(&self, report: &FinalizedReport) -> Vec<String> {
        let data = report.data();
        let mut lines = if data.result.verdict == JobVerdict::BuildFailed {
            build_summary(report)
        } else {
            Vec::new()
        };
        let passed = data
            .tests
            .iter()
            .filter(|t| matches!(t.verdict, Some(crate::report::TestVerdict::Passed)))
            .count() as u32;
        let unchecked = data
            .tests
            .iter()
            .filter(|t| {
                matches!(t.verdict, Some(crate::report::TestVerdict::Unchecked))
                    && t.check
                        .as_ref()
                        .is_some_and(|c| matches!(c.kind, crate::report::CheckKind::Unchecked))
            })
            .count() as u32;
        let comparator = data
            .comparator
            .as_ref()
            .map_or("tokens", crate::report::comparator_token);
        let template = if data.result.verdict == JobVerdict::NoTests {
            crate::report::SummaryTemplate::NoTests
        } else if let Some(crate::report::AggregateScore::Points {
            value,
            max,
            aggregation,
            ..
        }) = &data.details.score
        {
            crate::report::SummaryTemplate::Points {
                value: *value,
                max: *max,
                aggregation: *aggregation,
                selected: data.tests.len() as u32,
            }
        } else if let Some(crate::report::AggregateScore::Incomplete { missing, .. }) =
            &data.details.score
        {
            crate::report::SummaryTemplate::IncompleteScore { missing: *missing }
        } else if let Some(profile) = data
            .details
            .roles
            .iter()
            .find(|r| r.role == crate::report::RoleName::Checker)
            .and_then(|r| r.profile)
        {
            crate::report::SummaryTemplate::Checker {
                passed,
                selected: data.tests.len() as u32,
                profile,
            }
        } else {
            crate::report::SummaryTemplate::Passed {
                passed,
                selected: data.tests.len() as u32,
                comparator,
            }
        };
        let completed = !matches!(
            data.result.verdict,
            JobVerdict::Cancelled | JobVerdict::Infrastructure | JobVerdict::InternalError
        );
        if !matches!(
            data.result.verdict,
            JobVerdict::Passed
                | JobVerdict::Unchecked
                | JobVerdict::NoTests
                | JobVerdict::BuildFailed
        ) {
            lines.push(crate::report::render_label(&label(
                LabelSubject::Job(data.result.verdict),
                &DisplayContext {
                    invariant: data.result.internal.as_ref().map(|i| i.invariant),
                    job_score: data.details.score.clone(),
                    ..DisplayContext::default()
                },
            )));
        }
        if unchecked > 0 {
            lines.push(crate::report::SummaryTemplate::Unchecked { count: unchecked }.render());
        }
        let mut summary =
            crate::report::summary(template, completed, data.result.cancellation.is_some());
        if data
            .details
            .roles
            .iter()
            .any(|r| r.role == crate::report::RoleName::Validator)
        {
            summary = summary.with_validated_inputs();
        }
        if data
            .details
            .target
            .as_ref()
            .is_some_and(|target| target.io.is_some())
        {
            summary = summary.with_file_io();
        }
        lines.push(summary.render());
        lines
    }
}

/// Configuration contract §8.2: builds + tests × repeats + hooks, at most
/// 10 000. Test jobs have one execution per test, plus eligible hooks.
fn exceeds_run_cap(tests: usize, build: bool) -> bool {
    tests
        .checked_add(usize::from(build))
        .is_none_or(|planned| planned > 10_000)
}

/// Run one invocation. Completed jobs return their selected status without
/// becoming application errors. Refusals are delivered here, then returned for
/// the CLI's stderr diagnostic. Machine preflight always precedes admission.
pub fn run(
    command: &impl JobCommand,
    problem: Option<&str>,
    profile: Option<String>,
    machine: MachineArgs,
) -> Result<ExitCode, Error> {
    run_with_config(command, problem, profile, machine, None)
}

pub fn run_with_config(
    command: &impl JobCommand,
    problem: Option<&str>,
    profile: Option<String>,
    machine: MachineArgs,
    config_path: Option<PathBuf>,
) -> Result<ExitCode, Error> {
    let kind = command.kind();
    let mode = machine.mode();
    let mut delivery = Some(machine.prepare(kind)?);
    let mut refusal = transport::early_refusal(kind, &Error::Internal("preparing job".into()));
    let cancel = CancelState::new();
    let mut signals = None;
    let mut progress = None;
    let mut events = None;
    let outcome = (|| {
        let supported = cfg!(any(target_os = "linux", target_os = "macos"));
        let cwd = std::env::current_dir()
            .map_err(|e| Error::Workspace(format!("cannot read current directory: {e}")))?;
        // Outside a workspace no problem configuration can be loaded, so the
        // platform refusal comes first. Inside one, planning reports
        // configuration errors first and then refuses (`plan_invocation`).
        if !supported && crate::workspace::find_optional_root(&cwd)?.is_none() {
            return Err(Error::Infrastructure(
                "unsupported platform: build/test/run/cmd execution requires Linux or macOS".into(),
            ));
        }
        // An unsupported platform always refuses during planning; it installs
        // no signal listener.
        if supported {
            signals = Some(
                CancelSignals::install(cancel.clone())
                    .map_err(|e| Error::Infrastructure(format!("signal listener: {e}")))?,
            );
        }
        let root = crate::workspace::find_root(&cwd)?;
        refusal.workspace = Some(root.clone());
        let plan = plan_invocation(
            command,
            &root,
            &cwd,
            problem,
            profile,
            config_path,
            &mut refusal,
        );
        // Planning can fail concurrently with a signal. Cancellation selects
        // the refusal before any workspace admission or execution is attempted.
        check_cancel(&cancel)?;
        let plan = plan?;
        let InvocationPlan {
            config,
            selected,
            presentation,
            target,
            named,
            tests,
            baseline,
        } = plan;
        let hook_plans = if kind == JobKind::Test {
            config
                .hooks()
                .map_err(|e| Error::Config(e.to_string()))?
                .after_test
        } else {
            Vec::new()
        };
        let hook_reason = crate::hooks::authorize(config.hook_settings(), &root, &baseline).err();
        if !hook_plans.is_empty() && hook_reason.is_none() {
            let selected = selected.as_ref().expect("test selection");
            let job = root.join(".cpforge/runs/00000000000000000-0000000000000000");
            crate::hooks::prepare(
                &hook_plans,
                "after_test",
                &job,
                &root,
                problem_binding(selected),
                &baseline,
            )?;
            let build = usize::from(target.as_ref().is_some_and(|t| t.build.is_some()));
            if tests
                .len()
                .saturating_add(build)
                .saturating_add(hook_plans.len())
                > 10_000
            {
                return Err(Error::Config(
                    "planned runs exceed the 10 000 execution cap".into(),
                ));
            }
        }
        // Config §8.2: a judged plan counts every role build and role run and
        // is capped before admission; the same bounds are recomputed after freezing.
        let file_io = if kind == JobKind::Test {
            config
                .file_io()
                .map_err(|e| Error::Config(e.to_string()))?
                .map(|io| io.value)
        } else {
            None
        };
        let judge_options = crate::jobs::plan::JudgeOptions {
            hooks: if hook_reason.is_none() {
                hook_plans.len()
            } else {
                0
            },
            output_file: file_io.as_ref().is_some_and(|io| io.output.is_some()),
        };
        if kind == JobKind::Test {
            let check = config
                .judging()
                .map_err(|e| Error::Config(e.to_string()))?
                .filter(|c| c.has_roles());
            if let (Some(target), Some(check)) = (target.as_ref(), check) {
                crate::jobs::plan::validate_test_plan(
                    target,
                    Some(check),
                    tests.len(),
                    judge_options.hooks,
                    judge_options.output_file,
                )
                .map_err(|e| Error::Config(e.to_string()))?;
            }
        }
        let retention = config
            .retention()
            .map_err(|e| Error::Config(e.to_string()))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Infrastructure(format!("job runtime: {e}")))?;
        let writer = ProgressWriter::new(io::stderr(), tests.len())
            .map_err(|e| Error::Infrastructure(format!("stderr worker: {e}")))?;
        let diagnostics = config.diagnostics();
        for diagnostic in diagnostics
            .iter()
            .take(if diagnostics.len() > 64 { 63 } else { 64 })
        {
            writer.queue().result(&diagnostic.to_string());
        }
        if diagnostics.len() > 64 {
            writer.queue().result(&format!(
                "cpf: {} further configuration diagnostics",
                diagnostics.len() - 63
            ));
        }
        if let Some(crate::jobs::engine::stress::Schedule::Search { start_seed, .. }) =
            command.stress_schedule(&config)?
        {
            writer
                .queue()
                .result(&format!("stress start seed: {start_seed}"));
        }
        progress = Some(writer);
        check_cancel(&cancel)?;
        let lock = WorkspaceLock::new(&root)?;
        let registered = register_job(
            lock.acquire("job command")?,
            PrunePolicy::new(retention.keep_jobs.value, retention.workspace_bytes.value),
            retention.job_bytes.value,
            SystemTime::now(),
        )
        .map_err(|e| {
            admission_observations(&mut refusal, &e.report);
            Error::Workspace(e.to_string())
        })?;
        admission_observations(&mut refusal, &registered.prune_report);
        refusal.job = Some(JobIdentity {
            id: registered.id.clone(),
            dir: registered.dir.clone(),
            sequence: registered.sequence.clone(),
            kind,
        });
        refusal.timing.started_at = registered.started_at;
        check_cancel(&cancel)?;
        let mut hooks: Box<dyn PhaseHooks> = Box::new(NoHooks);
        let mut ready: crate::jobs::ReadyExecutionJob<'_> = if let Some(target) = target
            .as_ref()
            .filter(|_| named.as_ref().is_none_or(|p| p.needs_build.value))
        {
            let selected = selected.as_ref().expect("target selection");
            let inputs = PrepareJobInputs {
                workspace_root: root.clone(),
                problem: problem_binding(selected),
                report: presentation.clone().expect("problem presentation"),
                baseline: baseline.clone(),
                platform: Platform::host(),
                build_bytes: retention.build_bytes.value,
                judge_options,
                probe: &NativeProbe,
            };
            // Test jobs freeze even an empty selection, as before; build, raw
            // run and commands without tests only capture the target.
            let ready = if matches!(kind, JobKind::Build | JobKind::Run)
                || (kind == JobKind::Cmd && tests.is_empty())
            {
                prepare_build_job(registered, target, inputs)
            } else {
                let store = TestStore::new(inputs.problem.dir.clone());
                let check = config
                    .judging()
                    .map_err(|e| Error::Config(e.to_string()))?
                    .filter(|c| kind == JobKind::Test && c.has_roles());
                let frozen = if kind == JobKind::Stress {
                    registered.inputs().freeze_stress(
                        &store,
                        target,
                        config
                            .judging()
                            .map_err(|e| Error::Config(e.to_string()))?
                            .expect("stress judging defaults"),
                        config
                            .stress()
                            .map_err(|e| Error::Config(e.to_string()))?
                            .expect("validated stress"),
                        &inputs.problem.dir,
                    )
                } else if let Some(check) = check {
                    registered.inputs().freeze_judged(
                        &store,
                        &tests,
                        target,
                        check,
                        &inputs.problem.dir,
                    )
                } else {
                    registered.inputs().freeze(
                        &store,
                        &tests,
                        &CapturedSource::from_target(target),
                        &inputs.problem.dir,
                    )
                }
                .map_err(|e| Error::Workspace(e.to_string()))?;
                prepare_job(registered, frozen, target, inputs)
            }
            .map_err(ready_error)?;
            hooks = command.phase_hooks(&ready, &config)?;
            ready.into()
        } else {
            crate::jobs::prepare_command_job(
                registered,
                root.clone(),
                selected
                    .as_ref()
                    .zip(presentation)
                    .map(|(p, r)| (problem_binding(p), r)),
                &tests,
            )
            .map_err(ready_error)?
        };
        // Judged jobs remove staged scratch before measuring each private cwd.
        ready.cwd_bytes = (kind == JobKind::Cmd || ready.judging.is_some() || file_io.is_some())
            .then_some(retention.cwd_bytes.value);
        let stress_request = command
            .stress_schedule(&config)?
            .map(|schedule| stress::prepare_request(&ready, &config, schedule, &baseline))
            .transpose()?;
        let request = if let Some(named) = &named {
            let bindings = ready.planned_target.as_ref().map(|p| p.command_bindings());
            let frozen: Vec<_> = ready
                .manifest
                .entries()
                .iter()
                .map(|t| {
                    (
                        t.id.to_string(),
                        TestBinding {
                            input: t.input.path.clone(),
                            answer: match &t.answer {
                                crate::tests_store::FrozenAnswer::Absent => None,
                                crate::tests_store::FrozenAnswer::Present { path, .. } => {
                                    Some(path.clone())
                                }
                            },
                        },
                    )
                })
                .collect();
            command_request(
                named,
                command.named().expect("named command").1,
                CommandInputs {
                    job: &ready.dir,
                    workspace: &root,
                    problem: ready.problem.clone(),
                    target: target.as_ref(),
                    bindings,
                    tests: &frozen,
                    baseline: &baseline,
                },
            )?
        } else {
            command.request(target.as_ref().expect("target job"))
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let (mut executor, persistence_credit) =
            crate::jobs::engine::SupervisorExecutor::start_job()
                .map(|(executor, credit)| (executor, Some(credit)))
                .map_err(|e| Error::Infrastructure(format!("creation worker: {e:?}")))?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let persistence_credit = None;
        let mut persistence = JobPersistence::for_execution(&ready, persistence_credit)
            .map_err(|e| Error::Infrastructure(format!("persistence worker: {e}")))?;
        let after_test = if hook_reason.is_none() && !hook_plans.is_empty() {
            crate::hooks::prepare(
                &hook_plans,
                "after_test",
                &ready.dir,
                &root,
                ready.problem.clone().expect("test problem"),
                &baseline,
            )?
        } else {
            Vec::new()
        };
        let mut engine = if let Some(request) = stress_request {
            Engine::stress_with_cancellation(ready, request, cancel.clone())
        } else {
            Engine::with_cancellation(ready, request, cancel.clone())
        }
        .map_err(|e| Error::Config(e.to_string()))?
        .file_io(file_io)
        .map_err(|e| Error::Config(e.to_string()))?
        .progress(progress.as_ref().expect("stderr writer").queue())
        .output_events(mode.output_chunks());
        if kind == JobKind::Test {
            if let Some(reason) = hook_reason {
                engine = engine.disabled_hooks(
                    hook_plans.iter().map(|p| p.name.value.clone()).collect(),
                    "after_test",
                    reason,
                );
            } else {
                engine = engine
                    .after_test(after_test)
                    .map_err(|e| Error::Config(e.to_string()))?;
            }
        }
        if let MachineMode::Events { output } = mode {
            let receiver = engine.events().subscribe().expect("first event subscriber");
            events = Some(
                delivery
                    .take()
                    .expect("stdout delivery")
                    .start_events(receiver, kind, output)?,
            );
        }
        execute(
            engine,
            &runtime,
            &mut persistence,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            &mut executor,
            hooks.as_mut(),
        )
    })();
    match outcome {
        Ok(report) => {
            crate::process::freeze_report();
            let delivered = if let Some(events) = events {
                events.finish()
            } else if mode == MachineMode::Json {
                delivery.as_mut().expect("stdout delivery").report(&report)
            } else {
                Ok(())
            };
            let mut status = transport::delivered_status(&report, mode, &delivered);
            let mut lines = final_warnings(&report);
            if let Err(error) = &delivered {
                lines.push(format!("cpf: error: {error}"));
            }
            lines.extend(command.summary(&report));
            let writer = progress.expect("stderr writer");
            let finished = if kind == JobKind::Run {
                writer.finish_raw_lines(&lines)
            } else {
                writer.finish_lines(&lines)
            };
            if finished.is_err() {
                status = ExitCode::Internal;
            }
            // The report retains the lease through delivery and the summary.
            crate::storage::crash::barrier("job.after_delivery");
            Ok(signals.expect("signal listener").finish(status))
        }
        Err(error) => {
            let base = transport::early_refusal(kind, &error);
            refusal.class = base.class;
            refusal.error = base.error;
            refusal.cancellation = cancel.cancellation();
            refusal.timing.finished_at = SystemTime::now();
            refusal.timing.duration = refusal
                .timing
                .finished_at
                .duration_since(refusal.timing.started_at)
                .unwrap_or_default();
            let status = exit_status(&ExitInputs {
                failure: Some(error.class()),
                cancellation: refusal.cancellation.is_some(),
                ..ExitInputs::default()
            });
            refusal.pre_delivery_exit = if status == ExitCode::Cancelled {
                crate::jobs::engine::pre_delivery_exit(
                    kind,
                    JobVerdict::Cancelled,
                    refusal.cancellation,
                    None,
                )
            } else {
                crate::jobs::engine::PreDeliveryExit {
                    code: status as i64,
                    reraise: None,
                }
            };
            let delivered = delivery
                .get_or_insert_with(Delivery::stdout)
                .refusal(&refusal, mode);
            let status = exit_status(&ExitInputs {
                failure: if error.class() == crate::error::FailureClass::Internal {
                    Some(error.class())
                } else {
                    delivered
                        .as_ref()
                        .err()
                        .map(Error::class)
                        .or(Some(error.class()))
                },
                cancellation: refusal.cancellation.is_some(),
                machine_output: mode.requested(),
                delivery_failed: delivered
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.class() == crate::error::FailureClass::Delivery),
                ..ExitInputs::default()
            });
            let mut lines: Vec<_> = refusal
                .warnings
                .iter()
                .map(|w| format!("cpf: warning: {}", w.message))
                .collect();
            lines.push(format!("cpf: error: {error}"));
            if let Err(delivery_error) = &delivered {
                lines.push(format!("cpf: error: {delivery_error}"));
            }
            if let Some(progress) = progress {
                progress.finish_lines(&lines)?;
            } else {
                // No stderr worker exists yet; write directly, ignoring errors.
                let mut stderr = io::stderr().lock();
                for line in &lines {
                    let _ = writeln!(stderr, "{line}");
                }
            }
            Ok(signals.map_or(status, |signals| signals.finish(status)))
        }
    }
}

struct InvocationPlan {
    config: LoadedConfig,
    selected: Option<Selected>,
    presentation: Option<ReportPresentation>,
    target: Option<TargetPlan>,
    named: Option<CommandPlan>,
    tests: Vec<Identifier>,
    baseline: Baseline,
}

fn problem_binding(selected: &Selected) -> ProblemBinding {
    ProblemBinding {
        dir: selected.dir.clone(),
        id: selected.identity.to_string(),
    }
}

fn plan_invocation(
    command: &impl JobCommand,
    root: &Path,
    cwd: &Path,
    problem: Option<&str>,
    profile: Option<String>,
    config_path: Option<PathBuf>,
    refusal: &mut wire::Refusal,
) -> Result<InvocationPlan, Error> {
    let mut request = ConfigRequest {
        environment: Environment {
            home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            appdata: std::env::var_os("APPDATA"),
        },
        workspace_root: Some(root.to_owned()),
        config_path,
        current_dir: cwd.to_owned(),
        platform: Platform::host(),
        operation: command.operation(),
        profile,
        deadline_ms: command.deadline_ms()?,
        ..ConfigRequest::default()
    };
    // Commands are declared at user/workspace level. Resolve their requirements
    // before attempting selection, so no saved selection or target is required
    // by an independent command.
    let initial = command.named().map(|_| {
        load_config(&ConfigRequest {
            profile: None,
            ..request.clone()
        })
    });
    let mut named = initial
        .as_ref()
        .zip(command.named())
        .map(|(initial, (name, _))| initial.command(name))
        .transpose()
        .map_err(|e| {
            refusal.diagnostics = diagnostics(&e.diagnostics);
            Error::Config(e.to_string())
        })?;
    let selected = if named.as_ref().is_none_or(|p| p.context.problem) || problem.is_some() {
        match selection::resolve(root, cwd, problem) {
            // A command whose fields require a problem, run without one, is
            // missing context: a plan error (C §5.2, §8.1; report §5.1 row 7).
            Err(selection::SelectionError::NoSelection) if named.is_some() => {
                return Err(Error::Config(format!(
                    "command {:?} requires a selected problem: {}",
                    command.named().map_or("", |(name, _)| name),
                    selection::SelectionError::NoSelection
                )));
            }
            result => Some(result?),
        }
    } else {
        None
    };
    let metadata = selected.as_ref().map(problem_metadata).transpose()?;
    let presentation = metadata.as_ref().map(|m| m.presentation.clone());
    refusal.problem =
        selected
            .as_ref()
            .zip(presentation.as_ref())
            .map(|(p, r)| crate::jobs::engine::Problem {
                identity: p.identity.to_string(),
                name: r.problem_name.clone(),
                kind: r.problem_kind.clone(),
                dir: p.dir.clone(),
            });
    if let Some(selected) = &selected {
        request = selected.config_request(request);
    }
    let config = match initial {
        Some(initial) if selected.is_none() => initial,
        _ => load_config(&request),
    };
    // Surface all loader errors even when another target rule (for example
    // an unsupported problem kind) would also refuse this invocation.
    if config.has_errors() {
        let error = crate::config::ConfigErrors {
            diagnostics: config.diagnostics().to_vec(),
        };
        refusal.diagnostics = diagnostics(&error.diagnostics);
        return Err(Error::Config(error.to_string()));
    }
    if let Some((name, _)) = command.named() {
        named = Some(config.command(name).map_err(|e| {
            refusal.diagnostics = diagnostics(&e.diagnostics);
            Error::Config(e.to_string())
        })?);
    }
    if named
        .as_ref()
        .is_none_or(|p| p.needs_build.value || p.for_each_test.value)
    {
        if let Some(presentation) = presentation.as_ref().filter(|r| r.problem_kind != "batch") {
            return Err(Error::Config(format!(
                "problem kind {:?}: build/test/run unsupported in this version",
                presentation.problem_kind
            )));
        }
        if !crate::jobs::plan::judging_supported(
            config.judging().map_err(|e| Error::Config(e.to_string()))?,
        ) {
            let key = metadata
                .as_ref()
                .and_then(|m| m.judging_key)
                .unwrap_or("check");
            return Err(Error::Config(format!(
                "{}: {key}: build/test/run unsupported in this version",
                selected
                    .as_ref()
                    .expect("problem metadata")
                    .dir
                    .join("problem.toml")
                    .display()
            )));
        }
    }
    let needs_target = named.as_ref().is_none_or(|p| {
        p.needs_build.value
            || p.stage
                .deferred
                .iter()
                .any(|v| v.placeholder == crate::config::Placeholder::Entry)
    });
    let target = needs_target
        .then(|| config.target())
        .transpose()
        .map_err(|e| {
            refusal.diagnostics = diagnostics(&e.diagnostics);
            Error::Config(e.to_string())
        })?;
    // A reserved per-test declaration selected by a judged job (`cpf test`;
    // storage §8.1) is an early configuration refusal, also on unsupported
    // platforms. Other catalog/selection failures keep their existing
    // precedence: Windows does not enter phase 3 or open test data.
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        if named.is_none() && command.kind() == JobKind::Test {
            if let Err(Error::Config(message)) = command.select_tests(
                selected.as_ref().expect("selection"),
                &config,
                target.as_ref().expect("target"),
            ) {
                return Err(Error::Config(message));
            }
        }
        return Err(Error::Infrastructure(
            "unsupported platform: build/test/run/cmd execution requires Linux or macOS".into(),
        ));
    }
    command.stress_schedule(&config)?;
    if command.kind() == JobKind::Stress {
        stress::preflight_roles(
            &config,
            selected.as_ref().expect("stress selection"),
            target.as_ref().expect("stress target"),
        )?;
    }
    let (tests, command_tests) = if let Some(named) = &named {
        let entries = if named.for_each_test.value {
            let selected = selected.as_ref().expect("test context");
            TestStore::new(selected.dir.clone())
                .catalog()
                .map_err(Error::Workspace)?
                .entries
                .into_iter()
                .filter(|t| t.enabled)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        (
            entries.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            entries,
        )
    } else {
        (
            command.select_tests(
                selected.as_ref().expect("selection"),
                &config,
                target.as_ref().expect("target"),
            )?,
            Vec::new(),
        )
    };
    // Config §9.1.4 under `answer = "file"`, before admission: a refusal
    // allocates nothing. Preparation repeats the check on the frozen data.
    if command.kind() == JobKind::Test {
        if let Some(check) = config
            .judging()
            .map_err(|e| Error::Config(e.to_string()))?
            .filter(|check| {
                check.kind.value != crate::report::CheckMode::Compare
                    && check.answer.value == crate::report::AnswerPolicy::File
            })
        {
            let catalog = TestStore::new(selected.as_ref().expect("test selection").dir.clone())
                .catalog()
                .map_err(Error::Workspace)?;
            crate::jobs::plan::validate_answer_presence(
                check,
                catalog
                    .entries
                    .iter()
                    .filter(|entry| tests.contains(&entry.id))
                    .map(|entry| {
                        (
                            entry.id.to_string(),
                            !matches!(entry.answer, crate::tests_store::Answer::Absent),
                        )
                    }),
            )
            .map_err(Error::Config)?;
        }
    }
    let build = named.as_ref().is_none_or(|p| p.needs_build.value)
        && target.as_ref().is_some_and(|t| t.build.is_some());
    let primary = if let Some(named) = &named {
        planned_command_runs(named, tests.len(), build)?
    } else {
        tests
            .len()
            .checked_add(usize::from(command.kind() == JobKind::Run))
    };
    if primary.is_none_or(|count| exceeds_run_cap(count, build)) {
        return Err(Error::Config(
            "planned runs exceed the 10 000 execution cap".into(),
        ));
    }
    if command.kind() == JobKind::Test {
        if let Some(check) = config.judging().map_err(|e| Error::Config(e.to_string()))? {
            check
                .validate_test_count(tests.len())
                .map_err(|e| Error::Config(e.into()))?;
        }
    }
    let baseline = Baseline::capture();
    let retention = config
        .retention()
        .map_err(|e| Error::Config(e.to_string()))?;
    let bindings = if let Some(target) = target
        .as_ref()
        .filter(|_| named.as_ref().is_none_or(|p| p.needs_build.value))
    {
        let selected = selected.as_ref().expect("target selection");
        Some(preflight(
            selected,
            target,
            &baseline,
            &problem_binding(selected),
            retention.build_bytes.value,
            (command.kind() == JobKind::Stress).then_some("candidate"),
        )?)
    } else {
        None
    };
    if let Some(named) = &named {
        let job = root.join(".cpforge/runs/00000000000000000-0000000000000000");
        let frozen = command_tests
            .iter()
            .map(|t| {
                (
                    t.id.to_string(),
                    TestBinding {
                        input: job.join("inputs").join(format!("{}.in", t.id)),
                        answer: matches!(t.answer, crate::tests_store::Answer::Named(_))
                            .then(|| job.join("inputs").join(format!("{}.ans", t.id))),
                    },
                )
            })
            .collect::<Vec<_>>();
        command_request(
            named,
            command.named().expect("named").1,
            CommandInputs {
                job: &job,
                workspace: root,
                problem: selected.as_ref().map(problem_binding),
                target: target.as_ref(),
                bindings,
                tests: &frozen,
                baseline: &baseline,
            },
        )?;
    }
    Ok(InvocationPlan {
        config,
        selected,
        presentation,
        target,
        named,
        tests,
        baseline,
    })
}

fn refuse_test_overrides<'a>(
    entries: impl IntoIterator<Item = &'a crate::tests_store::TestEntry>,
) -> Result<(), Error> {
    let errors = entries
        .into_iter()
        .filter_map(|entry| {
            entry.check.as_ref().map(|location| {
                format!(
                    "{location}: test {:?}: check: unsupported in this version",
                    entry.id.as_str()
                )
            })
        })
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::Config(errors.join("\n")))
    }
}

fn planned_command_runs(
    plan: &CommandPlan,
    tests: usize,
    build: bool,
) -> Result<Option<usize>, Error> {
    if plan.for_each_test.value && tests == 0 {
        return Err(Error::Config(
            "for_each_test requires at least one enabled test".into(),
        ));
    }
    let count = if plan.for_each_test.value { tests } else { 1 };
    let primary = usize::try_from(plan.repeat.value)
        .ok()
        .and_then(|repeat| count.checked_mul(repeat));
    if primary.is_none_or(|count| exceeds_run_cap(count, build)) {
        return Err(Error::Config(
            "planned runs exceed the 10 000 execution cap".into(),
        ));
    }
    Ok(primary)
}

struct CommandInputs<'a> {
    job: &'a Path,
    workspace: &'a Path,
    problem: Option<ProblemBinding>,
    target: Option<&'a TargetPlan>,
    bindings: Option<TargetBindings>,
    tests: &'a [(String, TestBinding)],
    baseline: &'a Baseline,
}

fn command_request(
    plan: &CommandPlan,
    extra: &[OsString],
    inputs: CommandInputs<'_>,
) -> Result<JobRequest, Error> {
    use crate::jobs::engine::{CommandKind, Iteration, Recipe, Role};
    let mut iterations = Vec::new();
    let mut errors = crate::exec::limit_errors(&plan.stage.limits);
    let tests: Vec<_> = if plan.for_each_test.value {
        inputs.tests.iter().map(Some).collect()
    } else {
        vec![None]
    };
    for test in tests {
        for iteration in 1..=plan.repeat.value {
            let result = (|| {
                let context = Context::command(
                    plan,
                    InvocationDirs {
                        run_cwd: inputs.job.join("run/cwd"),
                        problem: inputs.problem.clone(),
                        workspace: Some(inputs.workspace.to_owned()),
                    },
                    CommandBindings {
                        target: inputs.bindings.clone(),
                        test: test.map(|(_, t)| t.clone()),
                        iteration: plan
                            .stage
                            .explicit_fields
                            .contains("repeat")
                            .then(|| NonZeroU64::new(iteration).expect("positive iteration")),
                        language: inputs.target.map(LanguageBinding::from_target),
                    },
                )
                .map_err(|e| e.to_string())?;
                let prepared = exec::prepare(
                    &plan.stage,
                    &context,
                    inputs.baseline,
                    Platform::host(),
                    extra,
                )
                .map_err(|e| e.to_string())?;
                let resolution = exec::resolve(prepared, &NativeProbe, Platform::host())
                    .map_err(|e| e.to_string())?;
                let recipe = match resolution {
                    Resolution::Resolved(stage) => {
                        stage
                            .launch_command(Platform::host())
                            .map_err(|e| e.to_string())?;
                        Recipe::Resolved(Box::new(stage))
                    }
                    Resolution::AwaitingProducer(stage) => Recipe::Deferred(Box::new(stage)),
                };
                Ok::<_, String>(Iteration {
                    role: Role::Command {
                        test: test.map(|(id, _)| id.clone()),
                        iteration: iteration as u32,
                    },
                    recipe,
                    stdin: test
                        .filter(|_| plan.stdin.value == Stdin::Test)
                        .map(|(_, t)| t.input.clone()),
                    depends_on_build: plan.needs_build.value,
                })
            })();
            match result {
                Ok(iteration) => iterations.push(iteration),
                Err(error) => errors.push(format!(
                    "test {} iteration {iteration}: {error}",
                    test.map_or("(none)", |(id, _)| id.as_str())
                )),
            }
        }
    }
    if !errors.is_empty() {
        return Err(Error::Config(errors.join("\n")));
    }
    Ok(JobRequest::Iterations {
        kind: CommandKind::Cmd,
        iterations,
        needs_build: plan.needs_build.value,
        keep_going: plan.keep_going.value,
    })
}

struct ProblemMetadata {
    presentation: ReportPresentation,
    judging_key: Option<&'static str>,
}

fn problem_metadata(selected: &Selected) -> Result<ProblemMetadata, Error> {
    let path = selected.dir.join("problem.toml");
    let record = crate::storage::read_user_record(&path, 1, |document| {
        let kind = document.get("problem").and_then(|p| p.get("kind"));
        let kind = kind
            .map(|v| v.as_str().ok_or("problem.kind must be a string"))
            .transpose()?
            .unwrap_or("batch");
        // Diagnostic location only; jobs::plan::judging_supported owns the gate.
        let judging_key = document.get("check").and_then(|check| {
            if !check.is_table_like() {
                return Some("check");
            }
            if check
                .get("kind")
                .is_some_and(|v| v.as_str() != Some("compare"))
            {
                return Some("check.kind");
            }
            [
                ("checker", "check.checker"),
                ("validator", "check.validator"),
                ("interactor", "check.interactor"),
                ("interactive", "check.interactive"),
                ("interactor_test", "check.interactor_test"),
            ]
            .into_iter()
            .find_map(|(key, path)| check.get(key).map(|_| path))
        });
        Ok::<_, String>(ProblemMetadata {
            presentation: ReportPresentation {
                problem_name: selected.identity.name().as_str().into(),
                problem_kind: kind.into(),
                target_name: None,
                published_limits: None,
            },
            judging_key,
        })
    });
    selection::record_result(record)
        .map_err(|e| Error::Workspace(format!("{}: {e}", path.display())))
}

fn admission_observations(
    refusal: &mut wire::Refusal,
    report: &crate::storage::admission::AdmissionReport,
) {
    refusal.warnings = crate::jobs::engine::warnings::admission(report)
        .iter()
        .filter_map(|w| wire::engine_warning(w, None).ok())
        .collect();
    if let (Some(dir), Some(sequence)) = (&report.created_dir, &report.allocated) {
        if let Some(id) = dir
            .file_name()
            .and_then(|v| v.to_str())
            .and_then(|v| crate::storage::job::JobId::parse(v).ok())
        {
            refusal.job = Some(JobIdentity {
                id,
                dir: dir.clone(),
                sequence: sequence.clone(),
                kind: refusal.command,
            });
        }
    }
}

fn diagnostics(values: &[crate::config::Diagnostic]) -> Vec<wire::Diagnostic> {
    values
        .iter()
        .map(|d| wire::Diagnostic {
            file: Some(d.origin.file.clone()),
            key: (!d.origin.key_path.is_empty()).then(|| d.origin.key_path.clone()),
            line: d.origin.line.and_then(|n| u32::try_from(n).ok()),
            col: d.origin.col.and_then(|n| u32::try_from(n).ok()),
            message: d.message.clone(),
        })
        .collect()
}

fn check_cancel(cancel: &CancelState) -> Result<(), Error> {
    if cancel.cancellation().is_some() {
        Err(Error::Cancelled("job cancelled before readiness".into()))
    } else {
        Ok(())
    }
}

fn ready_error(error: ReadyError) -> Error {
    match error {
        ReadyError::Plan(_)
        | ReadyError::Guard(_)
        | ReadyError::JudgingPlan(_)
        | ReadyError::Judging(_) => Error::Config(error.to_string()),
        ReadyError::WrongJob { .. } | ReadyError::WrongTarget | ReadyError::FailedInputs => {
            Error::Internal(error.to_string())
        }
        ReadyError::Freeze(_) | ReadyError::Storage(_) => Error::Workspace(error.to_string()),
    }
}

fn preflight(
    selected: &Selected,
    target: &TargetPlan,
    baseline: &Baseline,
    problem: &ProblemBinding,
    build_bytes: u64,
    iteration_role: Option<&str>,
) -> Result<TargetBindings, Error> {
    // Same depth and spelling as a real job, without creating any storage.
    // The actual plan is checked again after capture; this is not a capability.
    let job = selected
        .root
        .join(".cpforge/runs/00000000000000000-0000000000000000");
    let cwd = if target.build.is_some() {
        job.join("build").join("cwd")
    } else if let Some(role) = iteration_role {
        job.join("iterations").join(format!("0.{role}")).join("cwd")
    } else {
        job.join("run").join("cwd")
    };
    let outputs = job.join("builds").join("r00000");
    let plan = build::preflight_target(
        target,
        PlanInputs {
            sources: target
                .sources
                .iter()
                .map(|file| {
                    file.value
                        .components()
                        .iter()
                        .fold(job.join("inputs/src"), |p, c| p.join(c))
                })
                .collect(),
            dirs: InvocationDirs {
                run_cwd: cwd,
                problem: Some(problem.clone()),
                workspace: Some(selected.root.clone()),
            },
            baseline: baseline.clone(),
            platform: Platform::host(),
            build_bytes,
            probe: &NativeProbe,
        },
        target.build.as_ref().map(|_| outputs.as_path()),
    )
    .map_err(|e| Error::Config(e.to_string()))?;
    let (stage, arguments) = plan.source_stage();
    crate::jobs::guard::check_capture(
        target.build.as_ref().unwrap_or(&target.run),
        arguments,
        &stage.cwd,
        &selected.root,
        &job,
        Platform::host(),
    )
    .map_err(|e| Error::Config(e.to_string()))?;
    Ok(plan.command_bindings())
}

fn execute(
    engine: Engine<'_>,
    runtime: &tokio::runtime::Runtime,
    persistence: &mut JobPersistence,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    executor: &mut crate::jobs::engine::SupervisorExecutor,
    hooks: &mut dyn PhaseHooks,
) -> Result<Arc<FinalizedReport>, Error> {
    let mut retirement = crate::jobs::engine::stress::KeepFailures;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let report = if engine.kind() == JobKind::Stress {
            runtime.block_on(Box::pin(engine.run_stress(
                executor,
                persistence,
                &mut retirement,
            )))
        } else {
            runtime.block_on(Box::pin(engine.run(executor, persistence, hooks)))
        };
        #[cfg(feature = "storage-test-hooks")]
        if let Some(path) = std::env::var_os("CPF_STORAGE_PERSISTENCE_STATS") {
            let stats = persistence.stats();
            let samples: Vec<_> = stats
                .service_cycles
                .iter()
                .map(|d| d.as_micros().to_string())
                .collect();
            let bytes = serde_json::to_vec(&serde_json::json!({
                "peak_credits": stats.peak_credits,
                "credit_wait_us": stats.credit_wait.as_micros().to_string(),
                "service_cycles_us": samples,
                "last_write": stats.last_write,
                "settlement_to_finished_us": stats.settlement_elapsed.map(|d| d.as_micros().to_string()),
            })).expect("test statistics encode");
            std::fs::write(path, bytes).expect("test statistics publish");
        }
        Ok(report)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (engine, runtime, persistence, hooks, &mut retirement);
        Err(Error::Infrastructure(
            "unsupported execution platform".into(),
        ))
    }
}

fn final_warnings(report: &FinalizedReport) -> Vec<String> {
    use crate::jobs::engine::Warning;
    // The report's `warnings` list (§4.10): engine warnings, then the job's
    // own, then a late cancellation, at most 24, the rest counted. The final
    // allowance of report contract §8 holds exactly these lines.
    let data = report.data();
    let render = |w: &Warning| {
        wire::engine_warning(w, data.result.cancellation)
            .map_or_else(|e| format!("cpf: warning: {e}"), |w| warnings::line(&w))
    };
    let late = |w: &&Warning| matches!(w, Warning::LateCancellation);
    let mut lines: Vec<_> = data
        .warnings
        .iter()
        .filter(|w| !late(w))
        .map(render)
        .chain(
            data.details
                .warnings
                .iter()
                .map(|w| format!("cpf: warning: {}", w.message)),
        )
        .chain(data.warnings.iter().filter(late).map(render))
        .take(24)
        .collect();
    let overflow = (data.warnings.len() + data.details.warnings.len()).saturating_sub(24);
    let dropped = data
        .details
        .warnings_dropped
        .saturating_add(overflow.min(u32::MAX as usize) as u32);
    if dropped != 0 {
        lines.push(format!("cpf: warning: {dropped} further warnings dropped"));
    }
    match report.persistence() {
        ReportPersistence::Failed { message, .. } => lines.push(format!(
            "cpf: warning: report persistence failed: {message}"
        )),
        ReportPersistence::Published {
            uncertain,
            cleanup_pending,
            ..
        } => {
            if *uncertain {
                lines.push("cpf: warning: report durability uncertain".into());
            }
            if *cleanup_pending {
                lines.push("cpf: warning: report temporary cleanup pending".into());
            }
        }
        ReportPersistence::Refusal => {}
    }
    lines
}

fn build_summary(report: &FinalizedReport) -> Vec<String> {
    let data = report.data();
    let context = DisplayContext {
        cleanup_unverified: data
            .warnings
            .iter()
            .any(|w| matches!(w, crate::jobs::engine::Warning::CleanupUnverified { .. })),
        invariant: data.result.internal.as_ref().map(|i| i.invariant),
        ..DisplayContext::default()
    };
    let subject = if data.result.verdict != JobVerdict::InternalError {
        data.build
            .as_ref()
            .map_or(LabelSubject::Job(data.result.verdict), |build| {
                LabelSubject::Build(&build.result)
            })
    } else {
        LabelSubject::Job(data.result.verdict)
    };
    let display = label(subject, &context);
    let mut parts = vec![
        display
            .token
            .map_or("", |token| token.wire_token())
            .to_owned(),
    ];
    if data
        .target
        .as_ref()
        .is_some_and(|target| target.build.is_none())
    {
        parts.push("nothing was compiled (build-free target)".into());
    }
    for qualifier in display.qualifiers {
        parts.push(match qualifier {
            Qualifier::BuildCause(BuildCause::Run(RunClass::NonzeroExit { code })) => {
                format!("compiler exited with code {code}")
            }
            Qualifier::BuildCause(cause) => cause.wire_token().replace('_', " "),
            other => other
                .fixed_text()
                .map_or_else(|| format!("{other:?}"), str::to_owned),
        });
    }
    let mut lines = Vec::new();
    if let Some(build) = &data.build {
        if build.result == BuildResult::Succeeded {
            parts.push(format!(
                "artifacts: {}",
                build
                    .artifacts
                    .iter()
                    .map(|name| name.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(execution) = data.executions.get(build.execution as usize) {
            if let Some(ExecutionRecord::Started(root)) = execution.evidence.as_deref() {
                if let Some(capture) = &root.stderr {
                    // A bounded read from our own capture, never a compiler pipe.
                    let bytes = if let Some(path) = &capture.sink {
                        let mut bytes = Vec::new();
                        if let Ok(file) = std::fs::File::open(path) {
                            let _ = file.take(2048).read_to_end(&mut bytes);
                        }
                        bytes
                    } else {
                        capture.retained.iter().copied().take(2048).collect()
                    };
                    if data.result.verdict == JobVerdict::BuildFailed {
                        lines.extend(
                            String::from_utf8_lossy(&bytes)
                                .lines()
                                .take(3)
                                .map(str::to_owned),
                        );
                    }
                }
            }
        }
    }
    if data.result.cancellation.is_some() && data.result.verdict != JobVerdict::Cancelled {
        parts.push("(interrupted after completion)".into());
    }
    lines.push(parts.join(" | "));
    lines
}

/// Execute the captured on-switch plan after the selection transaction ended.
/// Selection is already committed: ordinary admission/execution failures warn.
pub(crate) fn switch_hooks(selected: &Selected, config: LoadedConfig) -> ExitCode {
    let cancel = CancelState::new();
    let mut signals = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Option<Arc<FinalizedReport>>, Error> {
            let plans = config
                .hooks()
                .map_err(|e| Error::Config(e.to_string()))?
                .on_switch;
            if plans.is_empty() {
                return Ok(None);
            }
            let baseline = Baseline::capture();
            if let Err(reason) =
                crate::hooks::authorize(config.hook_settings(), &selected.root, &baseline)
            {
                eprintln!("cpf: warning: hooks_disabled: {reason}");
                return Ok(None);
            }
            if !cfg!(any(target_os = "linux", target_os = "macos")) {
                eprintln!("cpf: warning: hooks are unsupported on this platform");
                return Ok(None);
            }
            let dummy = selected
                .root
                .join(".cpforge/runs/00000000000000000-0000000000000000");
            crate::hooks::prepare(
                &plans,
                "on_switch",
                &dummy,
                &selected.root,
                problem_binding(selected),
                &baseline,
            )?;
            let retention = config
                .retention()
                .map_err(|e| Error::Config(e.to_string()))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| Error::Infrastructure(format!("hook runtime: {e}")))?;
            signals = Some(
                CancelSignals::install(cancel.clone())
                    .map_err(|e| Error::Infrastructure(format!("signal listener: {e}")))?,
            );
            check_cancel(&cancel)?;
            let lock = WorkspaceLock::new(&selected.root)?;
            let registered = register_job(
                lock.acquire("switch hooks")?,
                PrunePolicy::new(retention.keep_jobs.value, retention.workspace_bytes.value),
                retention.job_bytes.value,
                SystemTime::now(),
            )
            .map_err(|e| Error::Workspace(e.to_string()))?;
            let mut ready = crate::jobs::prepare_command_job(
                registered,
                selected.root.clone(),
                Some((
                    problem_binding(selected),
                    problem_metadata(selected)?.presentation,
                )),
                &[],
            )
            .map_err(ready_error)?;
            ready.cwd_bytes = Some(retention.cwd_bytes.value);
            let iterations = crate::hooks::prepare(
                &plans,
                "on_switch",
                &ready.dir,
                &selected.root,
                problem_binding(selected),
                &baseline,
            )?;
            let request = JobRequest::Iterations {
                kind: crate::jobs::engine::CommandKind::Hook,
                iterations,
                needs_build: false,
                keep_going: true,
            };
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let (mut executor, persistence_credit) =
                crate::jobs::engine::SupervisorExecutor::start_job()
                    .map(|(executor, credit)| (executor, Some(credit)))
                    .map_err(|e| Error::Infrastructure(format!("creation worker: {e:?}")))?;
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let persistence_credit = None;
            let mut persistence = JobPersistence::for_execution(&ready, persistence_credit)
                .map_err(|e| Error::Infrastructure(format!("persistence worker: {e}")))?;
            let engine = Engine::with_cancellation(ready, request, cancel.clone())
                .map_err(|e| Error::Config(e.to_string()))?;
            execute(
                engine,
                &runtime,
                &mut persistence,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                &mut executor,
                &mut NoHooks,
            )
            .map(Some)
        },
    ));
    let status = match outcome {
        Ok(Ok(Some(report))) => {
            crate::process::freeze_report();
            // Replay retained streams only to stderr. Both stream budgets and
            // supervisor deadlines remain those of the hook's execution spec.
            let mut stderr = io::stderr().lock();
            for execution in &report.data().executions {
                if let Some(ExecutionRecord::Started(root)) = execution.evidence.as_deref() {
                    for capture in [root.stdout.as_ref(), root.stderr.as_ref()]
                        .into_iter()
                        .flatten()
                    {
                        if let Some(file) = capture
                            .sink
                            .as_ref()
                            .and_then(|path| std::fs::File::open(path).ok())
                        {
                            let _ = io::copy(&mut file.take(capture.stored), &mut stderr);
                        } else {
                            let _ = stderr.write_all(&capture.retained);
                        }
                    }
                }
            }
            for line in final_warnings(&report) {
                let _ = writeln!(stderr, "{line}");
            }
            let _ = writeln!(
                stderr,
                "cpf: switch hooks: {}",
                report.data().result.verdict.wire_token()
            );
            exit_status(&ExitInputs {
                failure: report
                    .post_freeze_internal()
                    .then_some(crate::error::FailureClass::Internal),
                verdict: Some(report.data().result.verdict),
                cancellation: report.data().result.cancellation.is_some(),
                switch_hook: true,
                ..ExitInputs::default()
            })
        }
        Ok(Ok(None)) => ExitCode::Success,
        Ok(Err(error)) => {
            eprintln!("cpf: warning: switch hook job: {error}");
            exit_status(&ExitInputs {
                failure: Some(error.class()),
                cancellation: cancel.cancellation().is_some(),
                switch_hook: true,
                ..ExitInputs::default()
            })
        }
        Err(_) => {
            eprintln!("cpf: error: internal failure in switch hook job");
            ExitCode::Internal
        }
    };
    if status != ExitCode::Success {
        eprintln!("cpf: note: selection was committed before switch hooks");
    }
    crate::storage::crash::barrier("job.after_delivery");
    signals.map_or(status, |signals| signals.finish(status))
}

#[cfg(test)]
mod tests {
    use super::exceeds_run_cap;

    #[test]
    fn planned_run_cap_counts_the_build() {
        assert!(!exceeds_run_cap(0, false));
        assert!(!exceeds_run_cap(10_000, false));
        assert!(exceeds_run_cap(10_001, false));
        assert!(!exceeds_run_cap(9_999, true));
        assert!(exceeds_run_cap(10_000, true));
        assert!(exceeds_run_cap(usize::MAX, true));
    }
}
