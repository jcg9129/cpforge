use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::{ArtifactRef, ExecutionRecord, ExecutionReport, ExecutionSpec, PersistError, Warning};
use crate::storage::budget::Reservation;
use crate::storage::job::JobOwner;

pub type JobFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Nonblocking enqueue and bounded settlement are separate operations. Enqueue
/// immediately at record finalization. An implementation owns metadata
/// reservations and owner clones until its last write returns, including after
/// a settlement timeout. No verdict lock is held while calling this service.
/// Settlement may change only the spec/record references, never execution
/// evidence or results. Report publication is a separate operation after F.
pub trait Persistence {
    fn enqueue(&mut self, artifact: PendingArtifacts);
    fn settle<'a>(
        &'a mut self,
        executions: &'a mut [ExecutionReport],
    ) -> JobFuture<'a, Vec<Warning>>;
}

#[derive(Debug)]
pub struct PendingArtifacts {
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
    fn enqueue(&mut self, artifact: PendingArtifacts) {
        self.pending.push(artifact);
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
                    *reference = ArtifactRef::memory();
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
