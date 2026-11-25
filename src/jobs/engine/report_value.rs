//! Finalized schema evidence owned by the engine, independent of delivery.
use super::CancelPhase;
use crate::config::Comparator;
use crate::jobs::engine as e;
use crate::report::HookOutcome;
use std::path::PathBuf;

/// Display facts supplied with ready-job preparation, where problem metadata
/// and the selection are available. They are frozen before execution.
#[derive(Clone, Debug)]
pub struct ReportPresentation {
    pub problem_name: String,
    pub problem_kind: String,
    pub target_name: Option<String>,
    pub published_limits: Option<(Option<u32>, Option<u64>)>,
}

#[derive(Clone, Debug)]
pub struct Problem {
    pub identity: String,
    pub name: String,
    pub kind: String,
    pub dir: PathBuf,
}
#[derive(Clone, Debug)]
pub struct Target {
    pub name: Option<String>,
    pub language: Option<String>,
    pub profile: Option<String>,
    pub build_free: bool,
    pub compare: Option<Comparator>,
    pub check: Option<crate::report::CheckConfig>,
    pub io: Option<crate::report::FileIo>,
    pub run_deadline: Option<(u64, &'static str)>,
    pub published_limits: Option<(Option<u32>, Option<u64>)>,
    pub local_limits: Vec<crate::supervisor::InstalledLimit>,
}
#[derive(Clone, Debug)]
pub enum FactOutcome {
    Published,
    Superseded,
    NotPublished {
        reason: &'static str,
        message: Option<String>,
    },
    NotAttempted {
        reason: &'static str,
    },
}
#[derive(Clone, Debug)]
pub struct Fact {
    pub outcome: FactOutcome,
    pub warnings: Vec<&'static str>,
}
#[derive(Clone, Debug)]
pub enum HookResult {
    Planned(HookOutcome),
    NotPlanned { reason: &'static str },
}
#[derive(Clone, Debug)]
pub struct Hook {
    pub name: String,
    pub trigger: &'static str,
    pub execution: Option<u32>,
    pub outcome: HookResult,
}
#[derive(Clone, Debug)]
pub enum WarningKind {
    IoScratch {
        run: String,
        detail: crate::report::IoScratchDetail,
    },
    Prune {
        job: Option<crate::storage::job::JobId>,
        detail: &'static str,
    },
    Recovery {
        paths: u32,
        first: Option<PathBuf>,
    },
    Persist {
        artefact: &'static str,
        run: String,
        error: e::PersistError,
    },
    Durability {
        artefact: &'static str,
        detail: &'static str,
    },
    Retention {
        detail: &'static str,
        run: Option<String>,
        bytes: u64,
    },
    CleanupUnverified {
        runs: u32,
    },
    Fact(FactOutcome),
    Hook {
        name: String,
        outcome: HookResult,
    },
    HooksDisabled {
        reason: &'static str,
    },
    LateCancellation {
        phase: CancelPhase,
    },
    ProgressDropped {
        lines: u32,
    },
}
#[derive(Clone, Debug)]
pub struct WireWarning {
    pub kind: WarningKind,
    pub message: String,
}
/// Already selected at finalization; serialization never selects process status.
#[derive(Clone, Copy, Debug)]
pub struct PreDeliveryExit {
    pub code: i64,
    pub reraise: Option<&'static str>,
}
#[derive(Clone, Debug)]
pub struct ReportDetails {
    /// The planned shape the report reservation used; None keeps the legacy
    /// compare-only bound and is invalid with file-I/O evidence.
    pub shape: Option<crate::report::wire::ReportShape>,
    pub problem: Option<Problem>,
    pub target: Option<Target>,
    pub score: Option<crate::report::AggregateScore>,
    pub judge_fingerprint: Option<[u8; 32]>,
    pub fingerprints: Option<([u8; 32], [u8; 32])>,
    pub fact: Option<Fact>,
    pub hooks: Vec<Hook>,
    pub hooks_dropped: u32,
    pub warnings: Vec<WireWarning>,
    pub warnings_dropped: u32,
    pub pre_delivery_exit: PreDeliveryExit,
    pub internal_message: String,
}
#[derive(Clone, Debug)]
pub enum ReportPersistence {
    Published {
        path: PathBuf,
        uncertain: bool,
        /// Retained for stderr delivery after S; omitted from report.json.
        cleanup_pending: bool,
    },
    Failed {
        error: e::PersistError,
        message: String,
    },
    Refusal,
}

/// Report §5.2 rows 3 and 5–14, evaluated once under F's verdict lock.
pub fn pre_delivery_exit(
    kind: crate::report::JobKind,
    verdict: crate::report::JobVerdict,
    cancellation: Option<super::Cancellation>,
    score: Option<&crate::report::AggregateScore>,
) -> PreDeliveryExit {
    use crate::exit::{ExitCode, ExitInputs, exit_status};
    let status = exit_status(&ExitInputs {
        verdict: Some(verdict),
        score_full: matches!(
            score,
            Some(crate::report::AggregateScore::Points { full: true, .. })
        ),
        cancellation: cancellation.is_some(),
        switch_hook: kind == crate::report::JobKind::Hook,
        ..ExitInputs::default()
    });
    let reraise = if status == ExitCode::Cancelled && cfg!(unix) {
        cancellation.and_then(|intent| match intent.source {
            super::CancelSource::Console => Some("sigint"),
            super::CancelSource::Terminate => Some("sigterm"),
            super::CancelSource::Api => None,
        })
    } else {
        None
    };
    PreDeliveryExit {
        code: match reraise {
            Some("sigint") => 130,
            Some("sigterm") => 143,
            _ => status as i64,
        },
        reraise,
    }
}
