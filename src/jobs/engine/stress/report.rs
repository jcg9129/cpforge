//! Fixed-size aggregates; no history of passing iterations survives retirement.
use super::*;
use crate::jobs::engine::warnings::push as push_warning;
use crate::jobs::engine::{ExecutionRecord, ExecutionReport};
use crate::report::{self as r, JobVerdict};
use crate::storage::job::Epoch;

pub const STRESS_REPORT_BOUND: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureId {
    pub epoch: Epoch,
    pub sequence: u64,
    pub iteration: u64,
}
impl FailureId {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        let mut fields = value.split('-');
        if fields.next() != Some("f") {
            return Err("invalid failure id prefix");
        }
        let epoch = Epoch::parse(fields.next().ok_or("missing epoch")?)?;
        let number = |field: Option<&str>| {
            field
                .and_then(|v| {
                    (!v.is_empty()
                        && (v == "0" || !v.starts_with('0'))
                        && v.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| v.parse::<u64>().ok())
                    .flatten()
                })
                .ok_or("invalid failure id number")
        };
        let sequence = number(fields.next())?;
        let iteration = number(fields.next())?;
        if fields.next().is_some() {
            return Err("extra failure id field");
        }
        Ok(Self {
            epoch,
            sequence,
            iteration,
        })
    }
}
impl std::fmt::Display for FailureId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "f-{}-{}-{}", self.epoch, self.sequence, self.iteration)
    }
}

impl IterationClass {
    pub const ALL: [Self; 8] = [
        Self::Passed,
        Self::GeneratorFailure,
        Self::ValidatorFailure,
        Self::OracleFailure,
        Self::JudgeFailure,
        Self::CandidateFailure,
        Self::Infrastructure,
        Self::Cancelled,
    ];
    pub fn token(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::GeneratorFailure => "generator_failure",
            Self::ValidatorFailure => "validator_failure",
            Self::OracleFailure => "oracle_failure",
            Self::JudgeFailure => "judge_failure",
            Self::CandidateFailure => "candidate_failure",
            Self::Infrastructure => "infrastructure",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug)]
pub struct StressDiagnostic {
    pub iteration: u64,
    pub seed: u64,
    pub decision: BatchDecision,
    pub source: &'static str,
    pub text: r::JudgeText,
}
#[derive(Clone, Debug)]
pub struct ClassAggregate {
    pub first: (u64, u64),
    pub diagnostics: Vec<StressDiagnostic>,
    pub diagnostics_dropped: u32,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct RoleTiming {
    pub runs: u64,
    pub wall: Option<[u64; 3]>, // min/max/total microseconds
}
#[derive(Clone, Debug)]
pub struct FailureRef {
    pub id: FailureId,
    pub seed: u64,
    pub decision: BatchDecision,
    pub saved: SaveOutcome,
    pub runs: Vec<(BatchRole, r::RunClass)>,
    pub records: Vec<SettledRun>,
}
#[derive(Clone, Debug)]
pub struct StressReport {
    pub schedule: Schedule,
    pub stop: Stop,
    pub counters: StressCounters,
    pub classes: [Option<ClassAggregate>; 8],
    pub timing: [RoleTiming; 5],
    pub failures: Vec<FailureRef>,
    pub cleanup_failed: CleanupFailure,
}
impl StressReport {
    pub(crate) fn new(schedule: Schedule) -> Self {
        Self {
            schedule,
            stop: Stop {
                reason: StopReason::Infrastructure,
                iteration: None,
            },
            counters: StressCounters::default(),
            classes: Default::default(),
            timing: Default::default(),
            failures: Vec::new(),
            cleanup_failed: CleanupFailure::default(),
        }
    }
    pub(super) fn fold_run(
        &mut self,
        role: BatchRole,
        run: &ExecutionReport,
    ) -> Result<(), AdmissionError> {
        let Some(ExecutionRecord::Started(root)) = run.evidence.as_deref() else {
            return Ok(());
        };
        let timing = &mut self.timing[role.index()];
        timing.runs = timing.runs.checked_add(1).ok_or(AdmissionError::State)?;
        if let Some(wall) = root.timing.wall {
            let wall = u64::try_from(wall.as_micros()).map_err(|_| AdmissionError::State)?;
            timing.wall = Some(match timing.wall {
                None => [wall; 3],
                Some([min, max, total]) => [
                    min.min(wall),
                    max.max(wall),
                    total.checked_add(wall).ok_or(AdmissionError::State)?,
                ],
            });
        }
        Ok(())
    }
    pub(super) fn fold(
        &mut self,
        k: u64,
        seed: u64,
        decision: BatchDecision,
        diagnostic: Option<(&'static str, r::JudgeText)>,
    ) {
        let aggregate =
            self.classes[decision.class.index()].get_or_insert_with(|| ClassAggregate {
                first: (k, seed),
                diagnostics: Vec::new(),
                diagnostics_dropped: 0,
            });
        if decision.class != IterationClass::Passed {
            if aggregate.diagnostics.len() < 16 {
                let (source, text) =
                    diagnostic.unwrap_or_else(|| ("none", r::JudgeText::new(&[], 1024)));
                let mut text = text;
                let head = r::JudgeText::new(&text.head, 1024).head;
                text.head = head;
                aggregate.diagnostics.push(StressDiagnostic {
                    iteration: k,
                    seed,
                    decision,
                    source,
                    text,
                });
            } else {
                aggregate.diagnostics_dropped = aggregate.diagnostics_dropped.saturating_add(1);
            }
        }
    }
    pub(super) fn finish_iteration(
        &mut self,
        id: FailureId,
        seed: u64,
        decision: BatchDecision,
        result: RetirementResult,
        evidence: &[BatchRunEvidence],
    ) {
        if let Some(saved) = result.saved {
            self.failures.push(FailureRef {
                id,
                seed,
                decision,
                saved,
                records: result.records,
                runs: evidence
                    .iter()
                    .map(|run| (run.role, run.class.clone()))
                    .collect(),
            });
        }
        self.cleanup_failed = result.cleanup_failed;
    }

    pub(crate) fn result_lines(&self, elapsed: Duration) -> Vec<String> {
        let start = match self.schedule {
            Schedule::Search { start_seed, .. } => start_seed,
            Schedule::Replay { seed, .. } => seed,
        };
        let mut lines = vec![
            format!(
                "{} iterations from seed {start} in {:.3}s",
                self.counters.retired,
                elapsed.as_secs_f64()
            ),
            format!(
                "stress stop: {} at {}",
                self.stop.reason.token(),
                self.stop
                    .iteration
                    .map_or_else(|| "none".into(), |k| k.to_string())
            ),
        ];
        for (index, class) in IterationClass::ALL.into_iter().enumerate() {
            if let Some(aggregate) = &self.classes[index] {
                lines.push(format!(
                    "{}: {}, first iteration {} seed {}",
                    class.token(),
                    self.counters.classes[index],
                    aggregate.first.0,
                    aggregate.first.1
                ));
            }
        }
        for failure in &self.failures {
            // Report §8: the id or "not saved", then the saved outcome.
            let (id, saved) = match failure.saved {
                SaveOutcome::Promoted => (failure.id.to_string(), "promoted"),
                SaveOutcome::Staged => (failure.id.to_string(), "not_promoted"),
                SaveOutcome::NotSaved(reason) => (
                    "not saved".to_owned(),
                    match reason {
                        NotSavedReason::ResourcesHeld => "not_saved resources_held",
                        NotSavedReason::Io => "not_saved io",
                    },
                ),
            };
            lines.push(format!(
                "{id}: {} {} {} seed {} {saved}",
                failure.decision.class.token(),
                failure.decision.role.map_or("none", |r| r.token()),
                failure.decision.reason,
                failure.seed,
            ));
        }
        for class in IterationClass::ALL.into_iter().filter(|c| c.failing()) {
            if let Some(diag) = self.classes[class.index()]
                .as_ref()
                .and_then(|a| a.diagnostics.first())
            {
                lines.push(format!(
                    "{}: {}",
                    class.token(),
                    String::from_utf8_lossy(&diag.text.head)
                ));
            }
        }
        for (role, timing) in BatchRole::ALL.into_iter().zip(self.timing) {
            if let Some([min, max, total]) = timing.wall {
                lines.push(format!(
                    "{}: {} runs, wall min/max/total {min}/{max}/{total} us",
                    role.token(),
                    timing.runs
                ));
            }
        }
        lines.push(format!(
            "failures: {} saved, {} promoted, {} not saved, 0 binaries bytes",
            self.counters.saved, self.counters.promoted, self.counters.not_saved
        ));
        if !self.cleanup_failed.paths.is_empty() {
            lines.push(format!(
                "stress cleanup: {} leftover paths",
                self.cleanup_failed.paths.len()
            ));
        }
        lines
    }

    /// Report §2.14, independent of exit-status arbitration and late intent.
    pub fn verdict(&self, build: JobVerdict, internal: bool) -> JobVerdict {
        let has = |class: IterationClass| self.counters.classes[class.index()] != 0;
        if internal || build == JobVerdict::InternalError {
            JobVerdict::InternalError
        } else if build == JobVerdict::Infrastructure || has(IterationClass::Infrastructure) {
            JobVerdict::Infrastructure
        } else if build == JobVerdict::Cancelled
            || has(IterationClass::Cancelled)
            || self.stop.reason == StopReason::Cancelled
        {
            JobVerdict::Cancelled
        } else if build == JobVerdict::BuildFailed {
            JobVerdict::BuildFailed
        } else if self.stop.reason == StopReason::CleanupFailed {
            JobVerdict::StressCleanupFailed
        } else {
            for (class, verdict) in [
                (
                    IterationClass::CandidateFailure,
                    JobVerdict::CandidateFailure,
                ),
                (IterationClass::JudgeFailure, JobVerdict::JudgeFailure),
                (IterationClass::OracleFailure, JobVerdict::OracleFailure),
                (
                    IterationClass::ValidatorFailure,
                    JobVerdict::ValidatorFailure,
                ),
                (
                    IterationClass::GeneratorFailure,
                    JobVerdict::GeneratorFailure,
                ),
            ] {
                if has(class) {
                    return verdict;
                }
            }
            if self.counters.retired == 0 {
                JobVerdict::NoIterations
            } else {
                JobVerdict::Passed
            }
        }
    }
}

pub(super) fn run_warnings(
    details: &mut crate::jobs::engine::ReportDetails,
    id: IterationRunId,
    run: &ExecutionReport,
) {
    use crate::jobs::engine::{WarningKind, WireWarning};
    if let Some(bytes) = run.summary.as_ref().and_then(|s| s.cwd_overflow_bytes) {
        push_warning(
            details,
            WireWarning {
                kind: WarningKind::Retention {
                    detail: "cwd_overflow",
                    run: Some(id.to_string()),
                    bytes,
                },
                message: format!(
                    "CwdOverflow: working directory exceeded its byte limit ({bytes} bytes)"
                ),
            },
        );
    }
    if run.judge_scratch_kept {
        push_warning(
            details,
            WireWarning {
                kind: WarningKind::JudgeScratch {
                    run: id.to_string(),
                    detail: r::JudgeScratchDetail::StagedCopyKept,
                },
                message: "judge staged copies could not be removed".into(),
            },
        );
    }
}

pub(super) fn persistence_warnings(
    details: &mut crate::jobs::engine::ReportDetails,
    runs: &[SettledRun],
) {
    use crate::jobs::engine::{WarningKind, WireWarning};
    for run in runs {
        for (index, (artefact, reference)) in [
            ("spec", &run.spec),
            ("record", &run.record),
            ("cleanup_log", &run.cleanup),
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(error) = &reference.error {
                push_warning(
                    details,
                    WireWarning {
                        kind: WarningKind::Persist {
                            artefact,
                            run: run.id.to_string(),
                            error: error.clone(),
                        },
                        message: "artefact publication failed".into(),
                    },
                );
            }
            for (present, detail) in [
                (run.uncertain[index], "uncertain"),
                (run.cleanup_pending[index], "cleanup_pending"),
            ] {
                if present {
                    push_warning(
                        details,
                        WireWarning {
                            kind: WarningKind::Durability { artefact, detail },
                            message: "publication durability uncertain".into(),
                        },
                    );
                }
            }
        }
    }
}
