//! Shared problem job pipeline: preflight, admission, capture, execution and delivery.
//!
//! Implement [`JobCommand`] to supply the kind, configuration operation, selected
//! tests, engine request and human summary. [`run`] owns the invocation lifecycle;
//! commands supply phase hooks (build uses `NoHooks`, test can publish facts/hooks).
//! No executor can see original source bindings or an incomplete capture.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::build::{self, PlanInputs};
use crate::cancel_signals::CancelSignals;
use crate::config::{
    ConfigRequest, Environment, LoadedConfig, Operation, Platform, TargetPlan, load_config,
};
use crate::error::Error;
use crate::exec::{Baseline, InvocationDirs, NativeProbe, ProblemBinding};
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

/// Command-specific policy, evaluated before job registration. Test selection
/// must return stable catalog ids; the shared pipeline freezes that exact list.
pub trait JobCommand {
    fn kind(&self) -> JobKind;
    fn operation(&self) -> Operation;
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
        if self.cases.is_empty() {
            return Ok(catalog
                .entries
                .into_iter()
                .filter(|e| e.enabled)
                .map(|e| e.id)
                .collect());
        }
        self.cases
            .iter()
            .map(|id| {
                catalog
                    .entries
                    .iter()
                    .find(|entry| entry.id.as_str() == id)
                    .map(|entry| entry.id.clone())
                    .ok_or_else(|| {
                        Error::Workspace(format!(
                            "unknown registered test id {id:?}; register candidates with cpf test register first"
                        ))
                    })
            })
            .collect()
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
            .filter(|t| matches!(t.verdict, Some(crate::report::TestVerdict::Unchecked)))
            .count() as u32;
        let comparator = data
            .comparator
            .as_ref()
            .map_or("tokens", crate::report::comparator_token);
        let template = if data.result.verdict == JobVerdict::NoTests {
            crate::report::SummaryTemplate::NoTests
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
                    ..DisplayContext::default()
                },
            )));
        }
        lines.push(
            crate::report::summary(template, completed, data.result.cancellation.is_some())
                .render(),
        );
        if unchecked > 0 {
            lines.push(crate::report::SummaryTemplate::Unchecked { count: unchecked }.render());
        }
        lines
    }
}

/// Configuration contract §8.2: builds + tests × repeats + hooks, at most
/// 10 000. Test jobs have one execution per test and no planned hooks yet.
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
    let kind = command.kind();
    let mode = machine.mode();
    let mut delivery = Some(machine.prepare(kind)?);
    let mut refusal = transport::early_refusal(kind, &Error::Internal("preparing job".into()));
    let cancel = CancelState::new();
    let mut signals = None;
    let mut progress = None;
    let mut events = None;
    let outcome = (|| {
        if !cfg!(any(target_os = "linux", target_os = "macos")) {
            return Err(Error::Infrastructure(
                "unsupported platform: build/test/run execution requires Linux or macOS".into(),
            ));
        }
        let cwd = std::env::current_dir()
            .map_err(|e| Error::Workspace(format!("cannot read current directory: {e}")))?;
        let root = crate::workspace::find_root(&cwd)?;
        refusal.workspace = Some(root.clone());
        let selected = selection::resolve(&root, &cwd, problem)?;
        let presentation = presentation(&selected)?;
        refusal.problem = Some(crate::jobs::engine::Problem {
            identity: selected.identity.to_string(),
            name: presentation.problem_name.clone(),
            kind: presentation.problem_kind.clone(),
            dir: selected.dir.clone(),
        });
        if presentation.problem_kind != "batch" {
            return Err(Error::Config(format!(
                "problem kind {:?}: build/test/run unsupported in this version",
                presentation.problem_kind
            )));
        }
        let config = load_config(
            &selected.config_request(ConfigRequest {
                environment: Environment {
                    home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                        .map(PathBuf::from),
                    xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
                    appdata: std::env::var_os("APPDATA"),
                },
                current_dir: cwd,
                platform: Platform::host(),
                operation: command.operation(),
                profile,
                deadline_ms: command.deadline_ms()?,
                ..ConfigRequest::default()
            }),
        );
        let target = config.target().map_err(|error| {
            refusal.diagnostics = diagnostics(&error.diagnostics);
            Error::Config(error.to_string())
        })?;
        let retention = config
            .retention()
            .map_err(|e| Error::Config(e.to_string()))?;
        let tests = command.select_tests(&selected, &config, &target)?;
        let primary_runs = tests.len().checked_add(usize::from(kind == JobKind::Run));
        if primary_runs.is_none_or(|count| exceeds_run_cap(count, target.build.is_some())) {
            return Err(Error::Config(
                "planned runs exceed the 10 000 execution cap".into(),
            ));
        }
        let request = command.request(&target);
        let baseline = Baseline::capture();
        let binding = ProblemBinding {
            dir: selected.dir.clone(),
            id: selected.identity.to_string(),
        };
        preflight(
            &selected,
            &target,
            &baseline,
            &binding,
            retention.build_bytes.value,
        )?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Infrastructure(format!("job runtime: {e}")))?;
        let writer = ProgressWriter::new(io::stderr())
            .map_err(|e| Error::Infrastructure(format!("stderr worker: {e}")))?;
        for diagnostic in config.diagnostics() {
            writer.queue().offer(&diagnostic.to_string());
        }
        progress = Some(writer);
        signals = Some(
            CancelSignals::install(cancel.clone())
                .map_err(|e| Error::Infrastructure(format!("signal listener: {e}")))?,
        );
        check_cancel(&cancel)?;
        let lock = WorkspaceLock::new(&selected.root)?;
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
        let inputs = PrepareJobInputs {
            workspace_root: selected.root,
            problem: binding,
            report: presentation,
            baseline,
            platform: Platform::host(),
            build_bytes: retention.build_bytes.value,
            probe: &NativeProbe,
        };
        let ready = if matches!(kind, JobKind::Build | JobKind::Run) {
            prepare_build_job(registered, &target, inputs)
        } else {
            let frozen = registered
                .inputs()
                .freeze(
                    &TestStore::new(inputs.problem.dir.clone()),
                    &tests,
                    &CapturedSource::from_target(&target),
                    &inputs.problem.dir,
                )
                .map_err(|e| Error::Workspace(e.to_string()))?;
            prepare_job(registered, frozen, &target, inputs)
        }
        .map_err(ready_error)?;
        let mut hooks = command.phase_hooks(&ready, &config)?;
        let mut persistence = JobPersistence::new(&ready)
            .map_err(|e| Error::Infrastructure(format!("persistence worker: {e}")))?;
        let engine = Engine::with_cancellation(ready, request, cancel.clone())
            .map_err(|e| Error::Config(e.to_string()))?
            .progress(progress.as_ref().expect("stderr writer").queue())
            .output_events(mode.output_chunks());
        if let MachineMode::Events { output } = mode {
            let receiver = engine.events().subscribe().expect("first event subscriber");
            events = Some(
                delivery
                    .take()
                    .expect("stdout delivery")
                    .start_events(receiver, kind, output)?,
            );
        }
        execute(engine, &runtime, &mut persistence, hooks.as_mut())
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

fn presentation(selected: &Selected) -> Result<ReportPresentation, Error> {
    let path = selected.dir.join("problem.toml");
    let record = crate::storage::read_user_record(&path, 1, |document| {
        let kind = document.get("problem").and_then(|p| p.get("kind"));
        let kind = kind
            .map(|v| v.as_str().ok_or("problem.kind must be a string"))
            .transpose()?
            .unwrap_or("batch");
        Ok::<_, String>(ReportPresentation {
            problem_name: selected.identity.name().as_str().into(),
            problem_kind: kind.into(),
            target_name: None,
            published_limits: None,
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
        ReadyError::Plan(_) | ReadyError::Guard(_) => Error::Config(error.to_string()),
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
) -> Result<(), Error> {
    // Same depth and spelling as a real job, without creating any storage.
    // The actual plan is checked again after capture; this is not a capability.
    let job = selected
        .root
        .join(".cpforge/runs/00000000000000000-0000000000000000");
    let cwd = job.join(if target.build.is_some() {
        "build/cwd"
    } else {
        "run/cwd"
    });
    let outputs = job.join("builds/r00000");
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
    Ok(())
}

fn execute(
    engine: Engine<'_>,
    runtime: &tokio::runtime::Runtime,
    persistence: &mut JobPersistence,
    hooks: &mut dyn PhaseHooks,
) -> Result<Arc<FinalizedReport>, Error> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        Ok(runtime.block_on(Box::pin(engine.run(
            &mut crate::jobs::engine::SupervisorExecutor::default(),
            persistence,
            hooks,
        ))))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (engine, runtime, persistence, hooks);
        Err(Error::Infrastructure(
            "unsupported execution platform".into(),
        ))
    }
}

fn final_warnings(report: &FinalizedReport) -> Vec<String> {
    let mut lines: Vec<_> = report
        .data()
        .warnings
        .iter()
        .map(|w| {
            wire::engine_warning(w, report.data().result.cancellation).map_or_else(
                |e| format!("cpf: warning: {e}"),
                |w| format!("cpf: warning: {} ({:?})", w.message, w.kind),
            )
        })
        .collect();
    lines.extend(
        report
            .data()
            .details
            .warnings
            .iter()
            .map(|w| format!("cpf: warning: {}", w.message)),
    );
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
