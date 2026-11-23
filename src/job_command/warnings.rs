//! Human warning details; the report's structured warning stays unchanged.
use crate::jobs::engine::{CancelPhase, PersistError, WarningKind, WireWarning};

pub(super) fn line(warning: &WireWarning) -> String {
    let message = &warning.message;
    let text = match &warning.kind {
        WarningKind::Prune { job, detail } => {
            let detail = detail.replace('_', " ");
            match job {
                Some(job) => format!("{message} for job {job}: {detail}"),
                None => format!("{message}: {detail}"),
            }
        }
        WarningKind::Recovery { paths, first } => {
            let plural = if *paths == 1 { "path" } else { "paths" };
            match first {
                Some(first) => format!("{message}: {paths} {plural}, first: {}", first.display()),
                None => format!("{message}: {paths} {plural}"),
            }
        }
        WarningKind::Persist {
            artefact,
            run,
            error,
        } => {
            let error = match error {
                PersistError::Timeout => "timed out".into(),
                PersistError::QueueFull => "persistence queue full".into(),
                PersistError::WorkerUnavailable => "persistence worker unavailable".into(),
                PersistError::Cancelled => "cancelled while waiting for the queue".into(),
                PersistError::Io {
                    os_error: Some(code),
                } => format!("I/O error (OS error {code})"),
                PersistError::Io { os_error: None } => "I/O error".into(),
            };
            format!(
                "{message} for {} of run {run}: {error}",
                artefact.replace('_', " ")
            )
        }
        WarningKind::Durability { artefact, detail } => {
            let artefact = artefact.replace('_', " ");
            match *detail {
                "uncertain" => format!("{message} for {artefact}"),
                "cleanup_pending" => format!("temporary cleanup pending for {artefact}"),
                detail => format!("{message} for {artefact}: {}", detail.replace('_', " ")),
            }
        }
        WarningKind::CleanupUnverified { runs } => {
            let plural = if *runs == 1 { "run" } else { "runs" };
            format!("{message} for {runs} {plural}")
        }
        WarningKind::LateCancellation { phase } => {
            let phase = match phase {
                CancelPhase::Primary => "primary execution",
                CancelPhase::Fact => "fact publication",
                CancelPhase::Hooks => "hooks",
                CancelPhase::Settlement => "settlement",
                CancelPhase::Closure => "closure",
            };
            format!("{message} during {phase}")
        }
        // Engine warnings (`wire::engine_warning`) never take these kinds.
        _ => message.clone(),
    };
    format!("cpf: warning: {text}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::job::JobId;
    use std::path::PathBuf;

    fn check(kind: WarningKind, message: &str, expected: &str) {
        assert_eq!(
            line(&WireWarning {
                kind,
                message: message.into()
            }),
            format!("cpf: warning: {expected}")
        );
    }

    #[test]
    fn engine_warning_members_are_rendered_in_words() {
        for (runs, expected) in [(0, "0 runs"), (1, "1 run"), (2, "2 runs")] {
            check(
                WarningKind::CleanupUnverified { runs },
                "cleanup not verified",
                &format!("cleanup not verified for {expected}"),
            );
        }
        for (phase, expected) in [
            (CancelPhase::Primary, "primary execution"),
            (CancelPhase::Fact, "fact publication"),
            (CancelPhase::Hooks, "hooks"),
            (CancelPhase::Settlement, "settlement"),
            (CancelPhase::Closure, "closure"),
        ] {
            check(
                WarningKind::LateCancellation { phase },
                "cancellation requested after primary completion",
                &format!("cancellation requested after primary completion during {expected}"),
            );
        }
        for (detail, expected) in [
            ("uncertain", "publication durability uncertain for record"),
            ("cleanup_pending", "temporary cleanup pending for record"),
        ] {
            check(
                WarningKind::Durability {
                    artefact: "record",
                    detail,
                },
                "publication durability uncertain",
                expected,
            );
        }
        let job = "20261123204756000-0123456789abcdef";
        check(
            WarningKind::Prune {
                job: Some(JobId::parse(job).unwrap()),
                detail: "lease_unreadable",
            },
            "job protected during pruning",
            &format!("job protected during pruning for job {job}: lease unreadable"),
        );
        check(
            WarningKind::Prune {
                job: None,
                detail: "reclaim_leftover",
            },
            "job protected during pruning",
            "job protected during pruning: reclaim leftover",
        );
        check(
            WarningKind::Recovery {
                paths: 2,
                first: Some(PathBuf::from("leftover")),
            },
            "interrupted publications recovered",
            "interrupted publications recovered: 2 paths, first: leftover",
        );
        check(
            WarningKind::Recovery {
                paths: 1,
                first: None,
            },
            "interrupted publications recovered",
            "interrupted publications recovered: 1 path",
        );
        for (error, expected) in [
            (PersistError::Timeout, "timed out"),
            (PersistError::QueueFull, "persistence queue full"),
            (
                PersistError::WorkerUnavailable,
                "persistence worker unavailable",
            ),
            (
                PersistError::Io { os_error: Some(28) },
                "I/O error (OS error 28)",
            ),
            (PersistError::Io { os_error: None }, "I/O error"),
            (
                PersistError::Cancelled,
                "cancelled while waiting for the queue",
            ),
        ] {
            check(
                WarningKind::Persist {
                    artefact: "cleanup_log",
                    run: "r0001".into(),
                    error,
                },
                "artefact publication failed",
                &format!("artefact publication failed for cleanup log of run r0001: {expected}"),
            );
        }
    }
}
