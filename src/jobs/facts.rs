//! Test-command adapter for report phase A. It owns only capture descriptors;
//! the engine retains the ready job's lease throughout publication.
use std::path::PathBuf;

use crate::jobs::engine::{Fact, FactOutcome, JobFuture, PhaseHooks, WarningKind, WireWarning};
use crate::names::Identifier;
use crate::report::JobVerdict;
use crate::storage::facts::fingerprint::{SourceFile, source_fingerprint, tests_fingerprint};
use crate::storage::facts::{FactRecord, PublicationOutcome, SOLUTION_TARGET, publish_fact};

#[derive(Debug)]
pub(crate) struct TestFacts {
    workspace: PathBuf,
    problem: Identifier,
    target_name: Identifier,
    target: crate::config::TargetPlan,
    files: Vec<SourceFile>,
    tests: Vec<crate::tests_store::FrozenTest>,
    record: FactRecord,
    fingerprints: Option<([u8; 32], [u8; 32])>,
    fingerprint_error: Option<String>,
    fact: Fact,
    warnings: Vec<WireWarning>,
}
impl TestFacts {
    pub(crate) fn new(ready: &super::ReadyJob<'_>) -> Self {
        Self {
            workspace: ready.workspace.clone(),
            // The fact directory is keyed by the problem name, which the
            // selected identity supplied as a validated identifier.
            problem: Identifier::new(ready.report.problem_name.as_str())
                .expect("validated problem name"),
            target_name: Identifier::new(
                ready
                    .report
                    .target_name
                    .as_deref()
                    .unwrap_or(SOLUTION_TARGET),
            )
            .expect("validated target name"),
            target: ready.target.clone(),
            files: ready.sources.files().iter().map(SourceFile::from).collect(),
            tests: ready.manifest.entries().to_vec(),
            record: FactRecord {
                epoch: ready.sequence.0.clone(),
                n: ready.sequence.1,
                job: ready.id.clone(),
                finished_at: String::new(),
                verdict: JobVerdict::Passed,
                source: [0; 32],
                tests: [0; 32],
                judge: ready.judging.as_ref().and_then(|j| j.judge_fingerprint),
                capture_check: ready.capture_check.clone(),
            },
            fingerprints: None,
            fingerprint_error: None,
            fact: Fact {
                outcome: FactOutcome::NotAttempted {
                    reason: "not_completed",
                },
                warnings: Vec::new(),
            },
            warnings: Vec::new(),
        }
    }
    fn failed(&mut self, message: String) {
        self.fact.outcome = FactOutcome::NotPublished {
            reason: "io",
            message: Some(message),
        };
    }
}
impl PhaseHooks for TestFacts {
    fn fingerprints(&mut self) -> Option<([u8; 32], [u8; 32])> {
        let result = source_fingerprint(&self.files, &self.target)
            .and_then(|source| tests_fingerprint(&self.tests).map(|tests| (source, tests)));
        match result {
            Ok((source, tests)) => {
                self.record.source = source;
                self.record.tests = tests;
                self.fingerprints = Some((source, tests));
            }
            Err(error) => self.fingerprint_error = Some(error.to_string()),
        }
        self.fingerprints
    }
    fn fact<'a>(
        &'a mut self,
        primary: JobVerdict,
        cutoff: &'a mut dyn FnMut() -> bool,
    ) -> JobFuture<'a, ()> {
        Box::pin(async move {
            if let Some(error) = self.fingerprint_error.take() {
                self.failed(error);
                return;
            }
            self.record.verdict = primary;
            match fact_finish_time().and_then(crate::report::wire::timestamp) {
                Ok(time) => self.record.finished_at = time,
                Err(error) => {
                    self.failed(error.to_string());
                    return;
                }
            }
            let result = publish_fact(
                &self.workspace,
                &self.problem,
                &self.target_name,
                &self.record,
                cutoff,
            );
            self.fact.outcome = match result.outcome {
                PublicationOutcome::Published => FactOutcome::Published,
                PublicationOutcome::Superseded => FactOutcome::Superseded,
                PublicationOutcome::NotAttemptedCancelled => FactOutcome::NotAttempted {
                    reason: "cancelled",
                },
                PublicationOutcome::NotPublished { reason, message } => {
                    FactOutcome::NotPublished { reason, message }
                }
            };
            for (present, token, detail) in [
                (
                    result.durability_uncertain,
                    "durability_uncertain",
                    "uncertain",
                ),
                (result.cleanup_pending, "cleanup_pending", "cleanup_pending"),
            ] {
                if present {
                    self.fact.warnings.push(token);
                    self.warnings.push(WireWarning {
                        kind: WarningKind::Durability {
                            artefact: "fact",
                            detail,
                        },
                        message: format!("fact publication: {token}"),
                    });
                }
            }
            if !result.recovery_paths.is_empty() {
                self.warnings.push(WireWarning {
                    kind: WarningKind::Recovery {
                        paths: result.recovery_paths.len().min(u32::MAX as usize) as u32,
                        first: result.recovery_paths.first().cloned(),
                    },
                    message: "fact publication recovery incomplete".into(),
                });
            }
        })
    }
    fn fact_evidence(&self) -> Option<(Fact, Vec<WireWarning>)> {
        let mut warnings = self.warnings.clone();
        if let FactOutcome::NotPublished { reason, message } = &self.fact.outcome {
            warnings.push(WireWarning {
                kind: WarningKind::Fact(self.fact.outcome.clone()),
                message: format!(
                    "fact not published: {reason}{}",
                    message.as_ref().map_or(String::new(), |m| format!(": {m}"))
                ),
            });
        }
        Some((self.fact.clone(), warnings))
    }
}

// Feature-gated clock injection for byte-exact fact vectors, alongside storage's
// process crash hooks. Normal builds always use the actual phase-A finish time.
fn fact_finish_time() -> Result<std::time::SystemTime, crate::report::wire::Error> {
    #[cfg(feature = "storage-test-hooks")]
    if let Ok(milliseconds) = std::env::var("CPF_STORAGE_FIXED_FACT_TIME_MS") {
        let milliseconds = crate::storage::job_seq::parse_decimal_u64(&milliseconds)
            .ok_or(crate::report::wire::Error("invalid fixed fact time"))?;
        return std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_millis(milliseconds))
            .ok_or(crate::report::wire::Error("fixed fact time out of range"));
    }
    Ok(std::time::SystemTime::now())
}
