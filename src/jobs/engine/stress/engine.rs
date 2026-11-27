//! The batch adapter lends one live iteration to the existing execution path.
use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::config::Comparator;
use crate::exec::ResolvedStage;
use crate::jobs::engine::{self as e, *};
use crate::report::{self as r, Coverage, RunClass};

/// Already validated templates. Role planning belongs to the command planner;
/// this adapter performs no configuration reads or OS creation. Instantiation
/// substitutes the run directory, the stdin path and the generator seed
/// argument (stress §§3.1-3.2), so a generator template carries no seed.
#[derive(Debug)]
pub struct StressRequest {
    pub templates: BatchTemplates<Box<ResolvedStage>>,
    pub schedule: Schedule,
    pub clock: Box<dyn StressClock>,
    pub comparator: Comparator,
    pub validator_profile: r::JudgeProfile,
    pub checker_profile: r::JudgeProfile,
    pub scoring: r::Scoring,
    pub min_score: Option<r::Score>,
    #[cfg(any(test, feature = "storage-test-hooks"))]
    pub counters: StressCounters,
}

impl StressRequest {
    pub fn new(
        templates: BatchTemplates<Box<ResolvedStage>>,
        schedule: Schedule,
        comparator: Comparator,
    ) -> Self {
        Self {
            templates,
            schedule,
            comparator,
            clock: Box::<MonotonicClock>::default(),
            validator_profile: r::JudgeProfile::Simple,
            checker_profile: r::JudgeProfile::Simple,
            scoring: r::Scoring::PassFail,
            min_score: None,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            counters: StressCounters::default(),
        }
    }
}

/// Per-run state is owned by the live iteration, never by `executions[]`.
#[derive(Debug)]
pub(crate) struct IterationExecution {
    pub id: IterationRunId,
    pub staged: Vec<crate::jobs::roles::StagedFile>,
    pub retirement: Option<RetirementRun>,
    pub finalized_at: tokio::time::Instant,
    pub testlib: Option<r::Scoring>,
}

impl<'probe> Engine<'probe> {
    pub fn stress(
        ready: impl Into<crate::jobs::ReadyExecutionJob<'probe>>,
        request: StressRequest,
    ) -> Result<Self, PlanError> {
        request
            .schedule
            .validate()
            .map_err(|_| PlanError::InvalidIteration)?;
        for template in request.templates.roles.iter().flatten() {
            let stage = &template.value;
            if stage
                .stdout
                .bytes
                .value
                .checked_add(stage.stderr.bytes.value)
                != Some(template.capture_bound)
            {
                return Err(PlanError::InvalidIteration);
            }
        }
        let mut engine = Self::new(
            ready,
            JobRequest::Iterations {
                kind: CommandKind::Cmd,
                iterations: Vec::new(),
                needs_build: true,
                keep_going: true,
            },
        )?;
        engine.kind = r::JobKind::Stress;
        engine.comparator = Some(request.comparator.clone());
        engine.stress = Some(Box::new(request));
        Ok(engine)
    }

    /// Promotion/removal are bounded retirement operations, separate from the
    /// phase hooks used by test jobs. The same hook is lent across iterations.
    pub async fn run_stress(
        self,
        executor: &mut dyn Executor,
        persistence: &mut dyn Persistence,
        retirement: &mut dyn RetirementHooks,
    ) -> Arc<FinalizedReport> {
        Box::pin(self.run_phases(executor, persistence, &mut NoHooks, retirement)).await
    }
}

impl e::Context<'_> {
    #[allow(clippy::too_many_arguments)] // Fixed job services and the frozen request.
    pub(crate) async fn stress_loop(
        &self,
        request: StressRequest,
        builds_succeeded: bool,
        report_refused: bool,
        executor: &mut dyn Executor,
        persistence: &mut dyn Persistence,
        hooks: &mut dyn RetirementHooks,
    ) -> Result<StressReport, AdmissionError> {
        let StressRequest {
            templates,
            schedule,
            clock,
            comparator,
            validator_profile,
            checker_profile,
            scoring,
            min_score,
            #[cfg(any(test, feature = "storage-test-hooks"))]
            counters,
        } = request;
        let has_checker = templates.roles[BatchRole::Checker.index()].is_some();
        let mut admission = StressAdmission::new(
            templates,
            schedule,
            self.cancel.clone(),
            builds_succeeded,
            clock,
        )?;
        #[cfg(any(test, feature = "storage-test-hooks"))]
        {
            if !matches!(schedule, Schedule::Replay { .. }) {
                admission.set_counters(counters)?;
            }
            if let Some(observer) = self.observer.cloned() {
                admission.observe_engine(move |point| (observer.0)(point));
            }
        }
        let mut report = StressReport::new(schedule);
        let mut progress_at = Instant::now();
        // No directory or run of a later iteration is instantiated in advance.
        // mkdir of this fixed parent is safe even if admission stops at zero.
        let _ = std::fs::create_dir(self.dir.join("iterations"));
        loop {
            let mut live = match admission.admit() {
                Ok(live) => live,
                Err(stop) => {
                    report.stop = stop;
                    break;
                }
            };
            let k = live.iteration();
            let seed = live.seed();
            self.events
                .offer(EventPayload::IterationStarted { iteration: k, seed });
            let mut finalized_at = tokio::time::Instant::now();
            let mut runs = Vec::with_capacity(5);
            let mut evidence = Vec::with_capacity(5);
            let mut validation = None;
            let mut check = None;
            let mut invalid_answer = false;
            let mut diagnostics = Vec::with_capacity(5);
            let mut outputs: [Option<(PathBuf, u64)>; 5] = Default::default();
            let mut target = e::TargetState {
                pending: None,
                free: None,
                validated: None,
            };
            while let Some(run) = live.admit_run()? {
                let role = run.id.role;
                let (stage, execution_role) = match role {
                    BatchRole::Generator => (
                        Stage::Generator,
                        Role::Test {
                            test: String::new(),
                        },
                    ),
                    BatchRole::Brute => (
                        Stage::Brute,
                        Role::Test {
                            test: String::new(),
                        },
                    ),
                    BatchRole::Candidate => (
                        Stage::Run,
                        Role::Test {
                            test: String::new(),
                        },
                    ),
                    BatchRole::Validator => (
                        Stage::Validator,
                        Role::Validator {
                            test: String::new(),
                        },
                    ),
                    BatchRole::Checker => (
                        Stage::Checker,
                        Role::Checker {
                            test: String::new(),
                        },
                    ),
                };
                let directory = run.id.directory(self.dir);
                let mut staged = Vec::new();
                let mut copy = |name: &str, source: Option<&(PathBuf, u64)>| {
                    staged.push(crate::jobs::roles::StagedFile {
                        source: source.map(|s| s.0.clone()),
                        bytes: Some(source.map_or(0, |s| s.1)),
                        destination: directory.join("cwd").join(name),
                    });
                };
                if matches!(role, BatchRole::Validator | BatchRole::Checker) {
                    copy("input", outputs[BatchRole::Generator.index()].as_ref());
                    if role == BatchRole::Checker {
                        copy("answer", outputs[BatchRole::Brute.index()].as_ref());
                        copy("output", outputs[BatchRole::Candidate.index()].as_ref());
                    }
                }
                let stdin = if matches!(role, BatchRole::Brute | BatchRole::Candidate) {
                    outputs[BatchRole::Generator.index()]
                        .as_ref()
                        .map(|v| v.0.clone())
                } else {
                    None
                };
                let mut template = run.template;
                if role == BatchRole::Generator {
                    // Stress §3.2: `gen <seed>`, the decimal g_k appended as
                    // the last argument after expansion, never expanded.
                    match &mut template.command {
                        crate::exec::ResolvedCommand::Argv { args, .. }
                        | crate::exec::ResolvedCommand::Shell { args, .. } => {
                            args.push(run.seed.to_string().into());
                        }
                    }
                }
                let plan = PlannedExecution {
                    index: role.index() as u32,
                    stage,
                    role: execution_role,
                    depends_on_build: false,
                    recipe: Recipe::Resolved(template),
                    stdin,
                    test: None,
                };
                let mut iteration = Some(IterationExecution {
                    id: run.id,
                    staged,
                    retirement: None,
                    finalized_at,
                    testlib: (role == BatchRole::Checker
                        && checker_profile == r::JudgeProfile::Testlib)
                        .then_some(scoring),
                });
                let mut refused = false;
                let execution = Box::pin(self.execute_inner(
                    plan,
                    &mut target,
                    executor,
                    persistence,
                    report_refused,
                    &mut refused,
                    &mut iteration,
                ))
                .await;
                let state = iteration.take().expect("iteration state");
                finalized_at = state.finalized_at;
                let Some(retirement) = state.retirement else {
                    return Err(AdmissionError::State);
                };
                let class = execution
                    .result
                    .class
                    .clone()
                    .ok_or(AdmissionError::State)?;
                let complete = retirement
                    .record
                    .stdout()
                    .is_some_and(crate::supervisor::StreamCapture::complete);
                if let Some(capture) = retirement.record.stdout() {
                    if let Some(path) = &capture.sink {
                        outputs[role.index()] = Some((path.clone(), capture.stored));
                    }
                }
                let started = execution.result.coverage == Coverage::Started;
                let captured = match retirement.record.as_ref() {
                    ExecutionRecord::Started(root) => root
                        .stdout
                        .iter()
                        .chain(root.stderr.iter())
                        .try_fold(0u64, |sum, stream| sum.checked_add(stream.stored))
                        .ok_or(AdmissionError::CaptureBound)?,
                    _ => 0,
                };
                let mut gate = if class == RunClass::ExitedZero && complete {
                    RunGate::Continue
                } else {
                    RunGate::Decided
                };
                if role == BatchRole::Validator {
                    validation = Some(match &class {
                        RunClass::ExitedZero => r::ValidationOutcome::Valid,
                        RunClass::NonzeroExit { code: 1 }
                            if validator_profile == r::JudgeProfile::Simple =>
                        {
                            r::ValidationOutcome::Invalid
                        }
                        class
                            if class.diagnostic()
                                && validator_profile == r::JudgeProfile::Testlib =>
                        {
                            r::ValidationOutcome::ValidationFailed
                        }
                        _ => r::ValidationOutcome::ValidatorFailed,
                    });
                    // Validator stdout is not an input or answer capture.
                    gate = if validation == Some(r::ValidationOutcome::Valid) {
                        RunGate::Continue
                    } else {
                        RunGate::Decided
                    };
                }
                if role == BatchRole::Brute && gate == RunGate::Continue && !has_checker {
                    invalid_answer = read_output(&retirement.record).is_ok_and(|answer| {
                        matches!(
                            crate::compare::check(&comparator, &answer, Some(&answer), true),
                            crate::compare::CheckOutcome::CheckerFailed(_)
                        )
                    });
                    if invalid_answer {
                        gate = RunGate::Decided;
                    }
                }
                if role == BatchRole::Candidate && class == RunClass::ExitedZero && !has_checker {
                    // Check admission is separately arbitrated. Late intent does
                    // not rewrite a comparison that already finished.
                    let intent = self.cancel.lock().intent.is_some();
                    let comparison = if intent {
                        r::Comparison::NotRun
                    } else {
                        let read = || -> std::io::Result<_> {
                            let output = read_output(&retirement.record)?;
                            let answer = outputs[BatchRole::Brute.index()]
                                .as_ref()
                                .map(|v| std::fs::read(&v.0))
                                .transpose()?
                                .unwrap_or_default();
                            Ok(crate::compare::check(
                                &comparator,
                                &output,
                                Some(&answer),
                                complete,
                            ))
                        };
                        match read() {
                            Ok(value) => r::Comparison::Outcome(value),
                            Err(e) => r::Comparison::IoError {
                                os_error: e.raw_os_error(),
                            },
                        }
                    };
                    check = Some(r::check_outcome(
                        &execution.result,
                        if intent {
                            r::CheckAdmission::IntentBeforeAdmission
                        } else {
                            r::CheckAdmission::NoIntent
                        },
                        true,
                        &comparison,
                    ));
                }
                if role == BatchRole::Checker && class != RunClass::Cancelled {
                    let solution = r::ExecutionResult {
                        coverage: Coverage::Started,
                        class: Some(RunClass::ExitedZero),
                        invariant: None,
                    };
                    let judge = e::judging::input(&execution, checker_profile);
                    check = Some(
                        r::batch_outcome(&r::BatchInput {
                            validator: None,
                            solution: &solution,
                            checker: Some(&judge),
                            scoring,
                            admission: r::CheckAdmission::NoIntent,
                            answer_present: true,
                            comparison: &r::Comparison::NotRun,
                            output_file: None,
                        })
                        .0,
                    );
                }
                live.record_finalized(RunCompletion {
                    started,
                    captured_bytes: captured,
                    gate,
                    retention_refused: refused,
                })?;
                report.fold_run(role, &execution)?;
                // Judges already read their message and frozen stderr head.
                let diagnostic = execution
                    .judge_evidence
                    .as_ref()
                    .and_then(|e| e.message.clone())
                    .map(|m| ("message", m))
                    .or_else(|| execution.judge_stderr.clone().map(|m| ("stderr", m)));
                diagnostics.push((role, diagnostic));
                evidence.push(BatchRunEvidence {
                    role,
                    class,
                    stdout_complete: complete,
                });
                runs.push(retirement);
            }
            // R1 is the last record's finalization, before classification/folding.
            let decision = live.classify_batch(&BatchEvidence {
                runs: &evidence,
                check: check.as_ref(),
                validation,
                invalid_answer,
                min_score,
            });
            // Stress §9: the deciding run's evidence text. A file sink keeps no
            // stderr in memory, so a non-judge's head is one read bounded by
            // its frozen length, outside the verdict lock and only for an
            // iteration that reports a diagnostic (never for `passed`).
            let diagnostic = decision
                .role
                .filter(|_| decision.class != IterationClass::Passed)
                .and_then(|role| {
                    diagnostics
                        .into_iter()
                        .find(|(r, _)| *r == role)
                        .and_then(|(_, d)| d)
                        .or_else(|| {
                            runs.iter()
                                .find(|run| run.id.role == role)
                                .and_then(|run| {
                                    e::judge_files::diagnostic(Some(run.record.as_ref()))
                                        .ok()
                                        .flatten()
                                })
                                .map(|text| ("stderr", text))
                        })
                })
                .or_else(|| match check.as_ref().map(|c| &c.kind) {
                    Some(r::CheckKind::Failed { diag }) => Some((
                        "compare_diag",
                        r::JudgeText::new(diag.to_string().as_bytes(), 1024),
                    )),
                    _ => None,
                });
            let lease = live.begin_retirement(decision.class)?;
            report.fold(k, seed, decision, diagnostic);
            let batch = RetirementBatch::new(runs, finalized_at)?;
            #[cfg(any(test, feature = "storage-test-hooks"))]
            let batch = if let Some(observer) = self.observer.cloned() {
                batch.observe(move |point| (observer.0)(point))
            } else {
                batch
            };
            #[cfg(any(test, feature = "storage-test-hooks"))]
            let probe = batch.probe();
            let result = batch
                .retire(lease, self.owner.clone(), self.budget, persistence, hooks)
                .await?;
            #[cfg(any(test, feature = "storage-test-hooks"))]
            self.phase_point(Point::StressRetired {
                iteration: k,
                debug: probe.snapshot(persistence),
            });
            if decision.class.failing() || progress_at.elapsed() >= Duration::from_millis(250) {
                if let Some(progress) = &self.progress {
                    progress.offer(&format!("iteration {k}: {}", decision.class.token()));
                }
                progress_at = Instant::now();
            }
            let id = FailureId {
                epoch: self.sequence.0.clone(),
                sequence: self.sequence.1,
                iteration: k,
            };
            let saved = result
                .saved
                .filter(|s| !matches!(s, SaveOutcome::NotSaved(_)))
                .map(|_| id.clone());
            report.finish_iteration(id, seed, decision, result, &evidence);
            self.events.offer(EventPayload::IterationFinished {
                iteration: k,
                decision,
                saved,
            });
            // No iteration evidence is retained in the ordinary execution maps.
            self.runs.borrow_mut().clear();
            self.frozen_outputs.borrow_mut().clear();
            // An injected executor may return only ready futures. Let event
            // consumers and cancellation producers run between retirements.
            tokio::task::yield_now().await;
        }
        report.counters = admission.counters();
        Ok(report)
    }
}

fn read_output(record: &ExecutionRecord) -> std::io::Result<Vec<u8>> {
    let capture = record
        .stdout()
        .ok_or_else(|| std::io::Error::other("missing frozen stdout"))?;
    capture
        .sink
        .as_ref()
        .map_or_else(|| Ok(capture.retained.clone()), std::fs::read)
}
