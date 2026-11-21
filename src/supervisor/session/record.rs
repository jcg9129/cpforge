//! Lossless, bounded execution facts. No verdict policy lives here.
use super::super::{Cleanup, NotStarted, Termination};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Role {
    S,
    I,
}
impl Role {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum OwnLimit {
    Wall,
    DialogueOutput,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Teardown {
    Cancel,
    Infrastructure,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum StopCause {
    OwnLimit(OwnLimit),
    PeerAllowanceExpired,
    SessionLimit,
    Idle,
    Teardown(Teardown),
}
/// Lossy telemetry for a later stop condition; never a session fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportDiagnostic {
    pub role: Role,
    pub cause: StopCause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Provenance {
    Natural,
    Stopped(u64),
}

/// The escaped JSON content is bounded, including multi-byte UTF-8 characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BoundedText {
    text: String,
    pub truncated: bool,
}
impl BoundedText {
    pub fn new(text: &str) -> Self {
        let mut retained = String::new();
        let mut escaped_bytes = 0;
        let mut truncated = false;
        for ch in text.chars() {
            // The quotes are not content bytes; serde owns all escape decisions.
            let length = serde_json::to_string(&ch).expect("character JSON").len() - 2;
            if escaped_bytes + length > 256 {
                truncated = true;
                break;
            }
            retained.push(ch);
            escaped_bytes += length;
        }
        Self {
            text: retained,
            truncated,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum StartFailureKind {
    Unsupported,
    CancelledBeforeStart,
    Worker,
    CleanupCapacity,
    UnsupportedLimit,
    InvalidLimit,
    Limit,
    CreateFailed,
    Setup,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StartFailure {
    pub kind: StartFailureKind,
    pub code: i32,
    pub text: BoundedText,
}
impl StartFailure {
    pub(super) fn from_reason(reason: &NotStarted) -> Self {
        let (kind, code) = match reason {
            NotStarted::Unsupported => (StartFailureKind::Unsupported, 0),
            NotStarted::CancelledBeforeStart => (StartFailureKind::CancelledBeforeStart, 0),
            NotStarted::Worker => (StartFailureKind::Worker, 0),
            NotStarted::CleanupCapacity => (StartFailureKind::CleanupCapacity, 0),
            NotStarted::UnsupportedLimit => (StartFailureKind::UnsupportedLimit, 0),
            NotStarted::InvalidLimit(_) => (StartFailureKind::InvalidLimit, 0),
            NotStarted::Limit(code) => (StartFailureKind::Limit, *code),
            NotStarted::CreateFailed { raw_error, .. } => {
                (StartFailureKind::CreateFailed, *raw_error)
            }
            NotStarted::Setup(error) => {
                (StartFailureKind::Setup, error.raw_os_error().unwrap_or(0))
            }
        };
        Self {
            kind,
            code,
            text: BoundedText::new(&format!("{reason:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum InfrastructureKind {
    NotStarted,
    UnknownTermination,
    CleanupIncomplete,
    Transport,
    Supervisor,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InfrastructureDetail {
    pub kind: InfrastructureKind,
    pub text: BoundedText,
}
impl InfrastructureDetail {
    pub fn new(kind: InfrastructureKind, text: &str) -> Self {
        Self {
            kind,
            text: BoundedText::new(text),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Fact {
    Started {
        role: Role,
    },
    NotStarted {
        role: Role,
        reason: StartFailure,
    },
    StopRequested {
        role: Role,
        cause: StopCause,
    },
    CancelRequested,
    RootExited {
        role: Role,
        status: Termination,
        provenance: Provenance,
    },
    CleanupResult {
        role: Role,
        cleanup: Cleanup,
    },
    Infrastructure {
        detail: InfrastructureDetail,
        suppressed: u32,
    },
}
impl Fact {
    fn key(&self) -> (u8, Option<StopCause>, Option<Role>) {
        match self {
            Self::NotStarted { role, .. } => (0, None, Some(*role)),
            Self::RootExited { role, .. } => (1, None, Some(*role)),
            Self::CleanupResult { role, .. } => (2, None, Some(*role)),
            Self::Started { role } => (6, None, Some(*role)),
            Self::StopRequested { role, cause } => (7, Some(*cause), Some(*role)),
            Self::CancelRequested => (8, None, None),
            Self::Infrastructure { .. } => (9, None, None),
        }
    }
    fn infrastructure(&self) -> bool {
        matches!(self, Self::NotStarted { reason, .. } if reason.kind != StartFailureKind::CancelledBeforeStart)
            || matches!(
                self,
                Self::RootExited {
                    status: Termination::Unknown,
                    ..
                } | Self::CleanupResult {
                    cleanup: Cleanup::Incomplete { .. },
                    ..
                }
            )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Event {
    pub obs_seq: u64,
    pub fact: Fact,
}
impl Event {
    /// Stable JSON form; only bounded summaries of run-level data are embedded.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("bounded fact JSON")
    }
}

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct SessionRecord {
    events: Vec<Event>,
    #[serde(skip)]
    observed: Vec<std::time::Instant>,
}
impl Default for SessionRecord {
    fn default() -> Self {
        Self {
            events: Vec::with_capacity(32),
            observed: Vec::with_capacity(32),
        }
    }
}
impl SessionRecord {
    pub fn events(&self) -> &[Event] {
        &self.events
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn transcript_events(
        &self,
        origin: std::time::Instant,
        cursor: &mut usize,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (event, observed) in self.events.iter().zip(&self.observed).skip(*cursor) {
            if matches!(
                event.fact,
                Fact::Started { .. } | Fact::StopRequested { .. } | Fact::RootExited { .. }
            ) {
                let entry = serde_json::json!({"time_ns": observed.saturating_duration_since(origin).as_nanos().min(u64::MAX as u128) as u64, "event": event});
                serde_json::to_writer(&mut bytes, &entry).expect("bounded event JSON");
                bytes.push(b'\n');
            }
        }
        *cursor = self.events.len();
        bytes
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("bounded record JSON")
    }
    pub(super) fn stop(&self, role: Role) -> Option<u64> {
        self.events.iter().find_map(|e| {
            matches!(e.fact, Fact::StopRequested { role: r, .. } if r == role).then_some(e.obs_seq)
        })
    }
    pub(super) fn exited(&self, role: Role) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e.fact, Fact::RootExited { role: r, .. } if r == role))
    }
    fn started(&self, role: Role) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e.fact, Fact::Started { role: r } if r == role))
    }
    pub(super) fn cancelled(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e.fact, Fact::CancelRequested))
    }
    pub(super) fn infrastructure(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e.fact, Fact::Infrastructure { .. }))
    }
    fn eligible(&self, fact: &Fact) -> bool {
        match fact {
            Fact::StopRequested { role, cause } => {
                self.started(*role)
                    && match cause {
                        StopCause::Teardown(Teardown::Cancel) => self.cancelled(),
                        StopCause::Teardown(Teardown::Infrastructure) => self.infrastructure(),
                        _ => true,
                    }
            }
            Fact::RootExited {
                role, provenance, ..
            } => {
                self.started(*role)
                    && (matches!(provenance, Provenance::Natural) || self.stop(*role).is_some())
            }
            Fact::CleanupResult { role, .. } => self.exited(*role),
            Fact::NotStarted { reason, .. }
                if reason.kind == StartFailureKind::CancelledBeforeStart =>
            {
                self.cancelled()
            }
            _ => true,
        }
    }
    /// Re-evaluate eligibility after *each* append, rather than sorting a batch.
    /// Pending facts are coordinator-owned; callers cannot mutate the record.
    pub(super) fn wakeup(&mut self, pending: Vec<Fact>) {
        self.wakeup_with_diagnostics(pending, None);
    }
    pub(super) fn wakeup_with_diagnostics(
        &mut self,
        mut pending: Vec<Fact>,
        diagnostics: Option<&tokio::sync::mpsc::Sender<TransportDiagnostic>>,
    ) {
        while !pending.is_empty() {
            let index = pending
                .iter()
                .enumerate()
                .filter(|(_, fact)| self.eligible(fact))
                .min_by_key(|(_, fact)| fact.key())
                .map(|(index, _)| index)
                .expect("acyclic fact prerequisites");
            let mut fact = pending.remove(index);
            match &mut fact {
                Fact::StopRequested { role, cause }
                    if self.stop(*role).is_some()
                        || (self.exited(*role)
                            && *cause != StopCause::OwnLimit(OwnLimit::DialogueOutput)) =>
                {
                    if let Some(sender) = diagnostics {
                        let _ = sender.try_send(TransportDiagnostic {
                            role: *role,
                            cause: *cause,
                        });
                    }
                    continue;
                }
                Fact::CancelRequested if self.cancelled() => continue,
                Fact::Infrastructure { suppressed, .. } if self.infrastructure() => {
                    if let Some(Event {
                        fact:
                            Fact::Infrastructure {
                                suppressed: old, ..
                            },
                        ..
                    }) = self
                        .events
                        .iter_mut()
                        .find(|e| matches!(e.fact, Fact::Infrastructure { .. }))
                    {
                        *old = old.saturating_add(1).saturating_add(*suppressed);
                    }
                    continue;
                }
                Fact::RootExited {
                    role,
                    provenance: Provenance::Stopped(seq),
                    ..
                } => *seq = self.stop(*role).expect("eligible stop"),
                _ => {}
            }
            if fact.infrastructure() {
                let kind = match &fact {
                    Fact::NotStarted { .. } => InfrastructureKind::NotStarted,
                    Fact::RootExited { .. } => InfrastructureKind::UnknownTermination,
                    Fact::CleanupResult { .. } => InfrastructureKind::CleanupIncomplete,
                    _ => unreachable!(),
                };
                pending.push(Fact::Infrastructure {
                    detail: InfrastructureDetail::new(kind, &format!("{fact:?}")),
                    suppressed: 0,
                });
            }
            assert!(
                self.events.len() < 17,
                "internal_policy_error: session record bound"
            );
            self.observed.push(std::time::Instant::now());
            self.events.push(Event {
                obs_seq: self.events.len() as u64,
                fact,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::UnverifiedReason;
    fn infrastructure() -> Fact {
        Fact::Infrastructure {
            detail: InfrastructureDetail::new(InfrastructureKind::Supervisor, "failure"),
            suppressed: 0,
        }
    }
    #[test]
    fn selection_rechecks_prerequisites_and_coalesces_stops() {
        let mut record = SessionRecord::default();
        let (diagnostics, mut received) = tokio::sync::mpsc::channel(1);
        record.wakeup_with_diagnostics(
            vec![
                Fact::StopRequested {
                    role: Role::I,
                    cause: StopCause::Teardown(Teardown::Cancel),
                },
                Fact::StopRequested {
                    role: Role::S,
                    cause: StopCause::OwnLimit(OwnLimit::DialogueOutput),
                },
                Fact::StopRequested {
                    role: Role::S,
                    cause: StopCause::OwnLimit(OwnLimit::Wall),
                },
                Fact::Started { role: Role::I },
                Fact::Started { role: Role::S },
                Fact::CancelRequested,
                infrastructure(),
            ],
            Some(&diagnostics),
        );
        assert_eq!(
            received.try_recv().unwrap(),
            TransportDiagnostic {
                role: Role::S,
                cause: StopCause::OwnLimit(OwnLimit::DialogueOutput)
            }
        );
        assert!(matches!(
            record.events[0].fact,
            Fact::Started { role: Role::S }
        ));
        assert!(matches!(
            record.events[1].fact,
            Fact::Started { role: Role::I }
        ));
        assert!(matches!(
            record.events[2].fact,
            Fact::StopRequested {
                role: Role::S,
                cause: StopCause::OwnLimit(OwnLimit::Wall)
            }
        ));
        assert!(matches!(record.events[3].fact, Fact::CancelRequested));
        assert!(matches!(
            record.events[4].fact,
            Fact::StopRequested {
                role: Role::I,
                cause: StopCause::Teardown(Teardown::Cancel)
            }
        ));
        for (seq, event) in record.events.iter().enumerate() {
            assert_eq!(event.obs_seq, seq as u64);
        }
        assert_eq!(
            record
                .events
                .iter()
                .filter(|e| matches!(e.fact, Fact::StopRequested { role: Role::S, .. }))
                .count(),
            1
        );
    }
    #[test]
    fn exit_precedes_timer_and_cleanup_becomes_eligible_immediately() {
        let mut record = SessionRecord::default();
        record.wakeup(vec![
            Fact::Started { role: Role::S },
            Fact::Started { role: Role::I },
        ]);
        record.wakeup(vec![
            Fact::StopRequested {
                role: Role::S,
                cause: StopCause::OwnLimit(OwnLimit::Wall),
            },
            Fact::StopRequested {
                role: Role::I,
                cause: StopCause::OwnLimit(OwnLimit::Wall),
            },
            Fact::CleanupResult {
                role: Role::I,
                cleanup: Cleanup::Confirmed,
            },
            Fact::RootExited {
                role: Role::I,
                status: Termination::Exited { code: 0 },
                provenance: Provenance::Natural,
            },
        ]);
        assert!(matches!(
            record.events[2].fact,
            Fact::RootExited { role: Role::I, .. }
        ));
        assert!(matches!(
            record.events[3].fact,
            Fact::CleanupResult { role: Role::I, .. }
        ));
        assert!(matches!(
            record.events[4].fact,
            Fact::StopRequested { role: Role::S, .. }
        ));
        assert_eq!(record.events.len(), 5);
    }
    #[test]
    fn teardown_waits_for_infrastructure_and_stopped_exit_waits_for_stop() {
        let mut record = SessionRecord::default();
        record.wakeup(vec![Fact::Started { role: Role::S }]);
        record.wakeup(vec![
            Fact::RootExited {
                role: Role::S,
                status: Termination::Unknown,
                provenance: Provenance::Stopped(0),
            },
            Fact::StopRequested {
                role: Role::S,
                cause: StopCause::Teardown(Teardown::Infrastructure),
            },
            infrastructure(),
            Fact::CleanupResult {
                role: Role::S,
                cleanup: Cleanup::Incomplete {
                    survivors: u32::MAX,
                },
            },
        ]);
        assert!(matches!(
            record.events[1].fact,
            Fact::Infrastructure { suppressed: 2, .. }
        ));
        assert!(matches!(record.events[2].fact, Fact::StopRequested { .. }));
        assert!(matches!(
            record.events[3].fact,
            Fact::RootExited {
                provenance: Provenance::Stopped(2),
                ..
            }
        ));
        record.wakeup(vec![Fact::Infrastructure {
            detail: InfrastructureDetail::new(InfrastructureKind::Supervisor, "later"),
            suppressed: u32::MAX,
        }]);
        assert!(matches!(
            record.events[1].fact,
            Fact::Infrastructure {
                suppressed: u32::MAX,
                ..
            }
        ));
    }
    #[test]
    fn largest_b7_variants_and_record_fit_json_bound() {
        let text = BoundedText::new(&"\u{0}\n\"\\🦀".repeat(1000));
        assert!(text.truncated);
        assert!(serde_json::to_string(&text.text).unwrap().len() - 2 <= 256);
        // All variant payload alternatives, including integer and reason extremes.
        let mut facts = vec![
            Fact::Started { role: Role::I },
            Fact::CancelRequested,
            Fact::Infrastructure {
                detail: InfrastructureDetail {
                    kind: InfrastructureKind::UnknownTermination,
                    text: text.clone(),
                },
                suppressed: u32::MAX,
            },
        ];
        for kind in [
            StartFailureKind::Unsupported,
            StartFailureKind::CancelledBeforeStart,
            StartFailureKind::Worker,
            StartFailureKind::CleanupCapacity,
            StartFailureKind::UnsupportedLimit,
            StartFailureKind::InvalidLimit,
            StartFailureKind::Limit,
            StartFailureKind::CreateFailed,
            StartFailureKind::Setup,
        ] {
            facts.push(Fact::NotStarted {
                role: Role::I,
                reason: StartFailure {
                    kind,
                    code: i32::MIN,
                    text: text.clone(),
                },
            });
        }
        for cause in [
            StopCause::OwnLimit(OwnLimit::Wall),
            StopCause::OwnLimit(OwnLimit::DialogueOutput),
            StopCause::PeerAllowanceExpired,
            StopCause::SessionLimit,
            StopCause::Idle,
            StopCause::Teardown(Teardown::Cancel),
            StopCause::Teardown(Teardown::Infrastructure),
        ] {
            facts.push(Fact::StopRequested {
                role: Role::I,
                cause,
            });
        }
        for status in [
            Termination::Exited { code: i64::MIN },
            Termination::Signalled {
                signal: i32::MIN,
                core_dumped: false,
            },
            Termination::Unknown,
        ] {
            for provenance in [Provenance::Natural, Provenance::Stopped(u64::MAX)] {
                facts.push(Fact::RootExited {
                    role: Role::I,
                    status,
                    provenance,
                });
            }
        }
        let reasons = [
            UnverifiedReason::NamespaceUnknown,
            UnverifiedReason::ProcFiltered,
            UnverifiedReason::ProcUnavailable,
            UnverifiedReason::ScanFailed,
            UnverifiedReason::RootNotObserved,
            UnverifiedReason::ObserverFailed,
            UnverifiedReason::Malformed,
            UnverifiedReason::ScanIncomplete,
            UnverifiedReason::ReapDeadline,
        ];
        let mut cleanups = vec![
            Cleanup::Confirmed,
            Cleanup::Incomplete {
                survivors: u32::MAX,
            },
        ];
        cleanups.extend(reasons.map(|reason| Cleanup::Unverified { reason }));
        for cleanup in cleanups {
            facts.push(Fact::CleanupResult {
                role: Role::I,
                cleanup,
            });
        }
        let mut largest = 0;
        for fact in facts {
            let event = Event {
                obs_seq: u64::MAX,
                fact,
            };
            let json = event.to_json();
            let _: serde_json::Value = serde_json::from_str(&json).unwrap();
            let text_len = match &event.fact {
                Fact::NotStarted { reason, .. } => {
                    serde_json::to_string(&reason.text.text).unwrap().len() - 2
                }
                Fact::Infrastructure { detail, .. } => {
                    serde_json::to_string(&detail.text.text).unwrap().len() - 2
                }
                _ => 0,
            };
            assert!(json.len() - text_len <= 512, "{json}");
            largest = largest.max(json.len());
        }
        assert!(17 * (largest + 1) + 2 <= 16 * 1024);
        let record = SessionRecord {
            observed: Vec::new(),
            events: (0..17)
                .map(|seq| Event {
                    obs_seq: seq,
                    fact: Fact::Infrastructure {
                        detail: InfrastructureDetail {
                            kind: InfrastructureKind::UnknownTermination,
                            text: text.clone(),
                        },
                        suppressed: u32::MAX,
                    },
                })
                .collect(),
        };
        assert!(record.to_json().len() <= 16 * 1024);
        let _: serde_json::Value = serde_json::from_str(&record.to_json()).unwrap();
        assert_eq!(SessionRecord::default().events.capacity(), 32);
    }
}
