use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::{ExecutionRecord, ExecutionReport, ExecutionSpec, PersistError, Warning};
use crate::storage::budget::Reservation;
use crate::storage::job::JobOwner;

pub type JobFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Asynchronous credit acquisition and bounded settlement are separate operations. Enqueue
/// ordinary specs at admission and records immediately at finalization; stress
/// holds both until iteration retirement. The service
/// owns metadata
/// reservations and owner clones until its last write returns, including after
/// a settlement timeout. No verdict lock is held while calling this service.
/// Settlement may change only the spec/record references, never execution
/// evidence or results. Report publication is a separate operation after F;
/// it includes encoding within the same enqueue deadline.
pub trait Persistence {
    fn configure(&mut self, _planned: u32, _cancel: crate::supervisor::CancelToken) {}
    fn enqueue(&mut self, artifact: PendingArtifacts) -> JobFuture<'_, ()>;
    /// Stress R5: encoded spec and metadata stayed with the live iteration.
    /// Each of the three publications gets its own enqueue-call deadline.
    fn enqueue_iteration(
        &mut self,
        artifact: super::stress::IterationArtifacts,
    ) -> JobFuture<'_, ()> {
        Box::pin(async move {
            drop(artifact);
        })
    }
    /// Stress R8: drain only this iteration's cells, preserving build references.
    fn settle_iteration(&mut self) -> JobFuture<'_, Vec<super::stress::SettledRun>> {
        Box::pin(async { Vec::new() })
    }
    #[cfg(any(test, feature = "storage-test-hooks"))]
    fn iteration_debug(&self) -> super::stress::PersistenceDebug {
        super::stress::PersistenceDebug::default()
    }
    fn publish_report(
        &mut self,
        _report: Arc<super::ReportData>,
        _reservation: Option<Reservation>,
    ) -> JobFuture<'_, super::ReportPersistence> {
        Box::pin(async {
            super::ReportPersistence::Failed {
                error: PersistError::WorkerUnavailable,
                message: "persistence worker unavailable".into(),
            }
        })
    }
    fn settle<'a>(
        &'a mut self,
        executions: &'a mut [ExecutionReport],
    ) -> JobFuture<'a, Vec<Warning>>;
}

#[derive(Debug)]
pub struct PendingArtifacts {
    /// False for the admission-time spec; true for record finalization.
    pub finalized: bool,
    pub run: super::RunId,
    pub execution: u32,
    /// False on setup failure or missing metadata allowance: never write
    /// into a colliding tree or without reserved headroom.
    pub writable: bool,
    pub spec: Option<Arc<ExecutionSpec>>,
    pub record: Option<Arc<ExecutionRecord>>,
    pub metadata: Option<Reservation>,
    pub owner: JobOwner,
}

/// Retains the in-memory artefacts until W, performs no writes, and settles all
/// references explicitly as unavailable. It never fabricates a persisted path.
#[derive(Debug, Default)]
pub struct MemoryPersistence {
    pub pending: Vec<PendingArtifacts>,
}
impl Persistence for MemoryPersistence {
    fn enqueue(&mut self, artifact: PendingArtifacts) -> JobFuture<'_, ()> {
        Box::pin(async move {
            self.pending.push(artifact);
        })
    }
    fn settle<'a>(
        &'a mut self,
        executions: &'a mut [ExecutionReport],
    ) -> JobFuture<'a, Vec<Warning>> {
        Box::pin(async move {
            for execution in executions {
                for reference in [&mut execution.spec, &mut execution.record]
                    .into_iter()
                    .flatten()
                {
                    reference.path = None;
                    reference.error = Some(PersistError::WorkerUnavailable);
                }
            }
            self.pending.clear();
            Vec::new()
        })
    }
}

pub(super) fn unavailable(executions: &mut [ExecutionReport]) {
    for execution in executions {
        for reference in [&mut execution.spec, &mut execution.record]
            .into_iter()
            .flatten()
        {
            if reference.path.is_none() {
                reference.error = Some(PersistError::WorkerUnavailable);
            }
        }
    }
}
