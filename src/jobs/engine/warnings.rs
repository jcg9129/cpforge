//! Bounded projection of preparation and finalized cleanup observations.

use super::{ExecutionRecord, ExecutionReport, Warning};
use crate::storage::admission::AdmissionReport;
use crate::storage::{Cleanup, Durability, ErrorKind};

pub(super) fn admission(report: &AdmissionReport) -> Vec<Warning> {
    let mut warnings = Vec::new();
    let mut push = |warning| {
        if warnings.len() < 24 {
            warnings.push(warning);
        }
    };
    if let Some(recovery) = &report.recovery {
        if !recovery.paths.is_empty() {
            push(Warning::Recovery {
                paths: recovery.paths.len().min(u32::MAX as usize) as u32,
                first: recovery.paths.first().cloned(),
            });
        }
    }
    for protected in &report.selection.protected {
        push(Warning::Prune {
            job: Some(protected.job.clone()),
            detail: "newer_schema",
        });
    }
    for _ in &report.selection.failed_probes {
        push(Warning::Prune {
            job: None,
            detail: "lease_unreadable",
        });
    }
    if report.selection.incomplete_measurement {
        push(Warning::Prune {
            job: None,
            detail: "measurement_incomplete",
        });
    }
    for _ in report
        .selection
        .errors
        .iter()
        .chain(&report.selection.unreadable_metadata)
    {
        push(Warning::Prune {
            job: None,
            detail: "unmeasurable",
        });
    }
    for failure in &report.selection.failed_renames {
        push(Warning::Prune {
            job: Some(failure.job.clone()),
            detail: if matches!(failure.error.kind, ErrorKind::Exists { .. }) {
                "reclaim_name_taken"
            } else {
                "rename_failed"
            },
        });
    }
    for _ in 0..report.reclaim.errors.len()
        + report.reclaim.walk.errors.len()
        + report.reclaim.walk.skipped.len()
    {
        push(Warning::Prune {
            job: None,
            detail: "reclaim_leftover",
        });
    }
    for (_, publication) in &report.publications {
        if matches!(publication.durability, Durability::Uncertain(_)) {
            push(Warning::Durability {
                artefact: "job_metadata",
                detail: "uncertain",
            });
        }
        if matches!(publication.cleanup, Cleanup::Pending(_)) {
            push(Warning::Durability {
                artefact: "job_metadata",
                detail: "cleanup_pending",
            });
        }
    }
    warnings
}

pub(super) fn cleanup(executions: &[ExecutionReport]) -> Option<Warning> {
    let runs = executions
        .iter()
        .filter(|execution| {
            matches!(execution.evidence.as_deref(), Some(ExecutionRecord::Started(root))
            if matches!(root.termination.cleanup, crate::supervisor::Cleanup::Unverified { .. }))
        })
        .count();
    (runs != 0).then_some(Warning::CleanupUnverified { runs: runs as u32 })
}
