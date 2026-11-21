//! Job registration (§6.2 steps 1–6), with no execution or ready-job API.
//!
//! Acquire a workspace guard once at outer-operation entry (which performs
//! leftover recovery), then transfer it to `register_job`. Registration consumes
//! and releases that guard before reclaiming, including on early failures.
//! Sequence allocation is never rolled back. A directory created before failure
//! is inactive interrupted storage for a later prune, not a runnable job.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use super::budget::{BudgetExceeded, CommitError, JobBudget};
use super::job::{Epoch, JobId, JobLease, JobOwner, JobRecord, publish_job_record};
use super::job_seq::read_job_sequence;
use super::prune::{PrunePolicy, PruneReport, ReclaimReport, reclaim, select_and_rename};
use super::{
    Cleanup, MalformedRecord, Published, Record, RecoveryIncomplete, StorageError, WorkspaceGuard,
    atomic_replace, crash,
};

const ID_ATTEMPTS: usize = 32;

/// Registration grants a lease and accounting, but no ability to start runs.
/// Freezing and planning must succeed before a later layer produces a ready job.
#[derive(Debug)]
pub struct RegisteredJob {
    pub id: JobId,
    pub started_at: SystemTime,
    pub started: Instant,
    pub dir: PathBuf,
    pub sequence: (Epoch, u64),
    pub owner: JobOwner,
    pub budget: Arc<JobBudget>,
    pub prune_report: AdmissionReport,
    // Freeze failures survive a new borrow of the inputs capability.
    pub(crate) inputs_failed: std::sync::atomic::AtomicBool,
}

/// Preserve warnings and committed changes even when a later step fails.
#[derive(Debug, Default)]
pub struct AdmissionReport {
    pub selection: PruneReport,
    pub renamed: Vec<JobId>,
    pub reclaim: ReclaimReport,
    pub recovery: Option<RecoveryIncomplete>,
    pub publications: Vec<(PathBuf, Published)>,
    /// Set only after replacement commits. Failures never roll this back.
    pub allocated: Option<(Epoch, u64)>,
    pub created_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub struct AdmissionError {
    pub kind: AdmissionErrorKind,
    pub report: Box<AdmissionReport>,
}

#[derive(Debug)]
pub enum AdmissionErrorKind {
    Storage(StorageError),
    SequenceMissing,
    SequenceMalformed(Box<MalformedRecord<&'static str>>),
    SequenceUnreadable(io::Error),
    SequenceTooNew { schema: i64, supported: i64 },
    SequenceExhausted,
    BudgetExceeded(BudgetExceeded),
    Accounting(CommitError),
}

/// The caller supplies the guard acquired for this operation, after recovery.
/// No nested operation acquires a workspace lock. The guard stays held across
/// pruning, sequence replacement, directory creation, metadata and lease lock;
/// it is released before reclaim on both success and ordinary error paths.
pub fn register_job(
    mut guard: WorkspaceGuard<'_>,
    policy: PrunePolicy,
    budget_limit: u64,
    now: SystemTime,
) -> Result<RegisteredJob, AdmissionError> {
    let started = Instant::now();
    let selection = select_and_rename(&guard, policy);
    let mut report = AdmissionReport {
        selection: selection.report,
        renamed: selection.renamed,
        recovery: guard.take_recovery_warning(),
        ..AdmissionReport::default()
    };
    // Keep all fallible registration steps inside this call: an early return
    // there must still reach unlock + reclaim here.
    let result = register_locked(&guard, budget_limit, now, &mut report);
    drop(guard);
    crash::point("admission.lock_released");
    report.reclaim = reclaim(selection.runs_dir);
    match result {
        Ok(mut job) => {
            job.started = started;
            job.prune_report = report;
            Ok(job)
        }
        Err(kind) => Err(AdmissionError {
            kind,
            report: Box::new(report),
        }),
    }
}

fn register_locked(
    guard: &WorkspaceGuard<'_>,
    budget_limit: u64,
    now: SystemTime,
    report: &mut AdmissionReport,
) -> Result<RegisteredJob, AdmissionErrorKind> {
    let path = guard.check_path(".cpforge/job-seq")?;
    let sequence = match read_job_sequence(&path) {
        Record::Valid(sequence) => sequence,
        Record::Missing => return Err(AdmissionErrorKind::SequenceMissing),
        Record::Malformed(error) => {
            return Err(AdmissionErrorKind::SequenceMalformed(Box::new(error)));
        }
        Record::Unreadable(error) => return Err(AdmissionErrorKind::SequenceUnreadable(error)),
        Record::TooNew { schema, supported } => {
            return Err(AdmissionErrorKind::SequenceTooNew { schema, supported });
        }
    };
    let next = sequence
        .next
        .checked_add(1)
        .ok_or(AdmissionErrorKind::SequenceExhausted)?;
    // The schema-first classifier has already validated the epoch encoding.
    let epoch = Epoch::parse(&sequence.epoch).expect("validated sequence epoch");
    let bytes = format!("schema = 1\nepoch = \"{epoch}\"\nnext = \"{next}\"\n");
    let published = atomic_replace(guard, &path, bytes.as_bytes(), || Ok(()))?;
    report.publications.push((path, published));
    report.allocated = Some((epoch.clone(), sequence.next));
    crash::point("admission.job_seq_replaced");

    let runs = guard.create_dir_all(".cpforge/runs")?;
    let mut created = None;
    for _ in 0..ID_ATTEMPTS {
        let id = JobId::generate(now).map_err(|error| StorageError::io(&runs, error))?;
        let dir = runs.join(id.as_str());
        // create_dir is the exclusive claim; an existing name is never reused.
        match fs::create_dir(&dir) {
            Ok(()) => {
                guard.created.borrow_mut().push(dir.clone());
                created = Some((id, dir));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(StorageError::io(&dir, error).into()),
        }
    }
    let (id, dir) = created.ok_or_else(|| {
        StorageError::io(
            &runs,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "job id collision retry limit reached",
            ),
        )
    })?;
    report.created_dir = Some(dir.clone());
    crash::point("admission.directory_created");

    let budget = Arc::new(JobBudget::new(budget_limit));
    let record = JobRecord {
        epoch: epoch.clone(),
        n: sequence.next,
    };
    let len = record.encode().len() as u64;
    // Reserve both names before publishing. The link fallback may retain a
    // full-length temp alongside job.toml; count both until successful removal.
    let publication_bytes = len.checked_mul(2).ok_or_else(|| {
        AdmissionErrorKind::BudgetExceeded(BudgetExceeded {
            limit: budget_limit,
            usage: budget.usage(),
            requested: u64::MAX,
        })
    })?;
    let mut reservation = budget
        .reserve(publication_bytes)
        .map_err(AdmissionErrorKind::BudgetExceeded)?;
    let published = publish_job_record(guard, &dir, &record)?;
    let retained = if matches!(published.cleanup, Cleanup::Done) {
        len
    } else {
        publication_bytes
    };
    reservation
        .commit(retained)
        .map_err(AdmissionErrorKind::Accounting)?;
    report.publications.push((dir.join("job.toml"), published));
    crash::point("admission.job_record_published");

    // The empty lease's logical length is zero, so it needs no byte allowance.
    let owner = JobLease::create_and_lock_with_budget(&dir, Arc::clone(&budget))
        .map_err(|error| StorageError::io(dir.join("lease"), error))?;
    crash::point("admission.lease_locked");
    Ok(RegisteredJob {
        id,
        started_at: now,
        started: Instant::now(),
        dir,
        sequence: (epoch, sequence.next),
        owner,
        budget,
        prune_report: AdmissionReport::default(),
        inputs_failed: std::sync::atomic::AtomicBool::new(false),
    })
}

impl From<StorageError> for AdmissionErrorKind {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl fmt::Display for AdmissionErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::SequenceMissing => {
                f.write_str(".cpforge/job-seq is missing; run `cpf fsck --apply`")
            }
            Self::SequenceMalformed(error) => write!(
                f,
                ".cpforge/job-seq is malformed: {error}; run `cpf fsck --apply`"
            ),
            Self::SequenceUnreadable(error) => write!(
                f,
                ".cpforge/job-seq is unreadable: {error}; fix access to `.cpforge/job-seq`, or remove it and run `cpf fsck --apply`"
            ),
            Self::SequenceTooNew { schema, supported } => write!(
                f,
                ".cpforge/job-seq schema {schema} (supported {supported}) needs a newer cpf"
            ),
            Self::SequenceExhausted => {
                f.write_str("admission sequence exhausted (SequenceExhausted)")
            }
            Self::BudgetExceeded(error) => write!(f, "{error}"),
            Self::Accounting(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for AdmissionErrorKind {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::SequenceUnreadable(error) => Some(error),
            Self::BudgetExceeded(error) => Some(error),
            Self::Accounting(error) => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl std::error::Error for AdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}
