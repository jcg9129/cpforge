//! Connected Unix sessions. This module records execution facts, never verdicts.
mod record;
pub use record::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod relay;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod transcript;

use super::{
    CancelToken, NotStarted, OutputSpec, ProgramSpec, RootOutcome, SpawnOptions, StdinSpec,
};
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    Relay,
    Direct,
}
#[derive(Debug)]
pub struct SessionSpec {
    pub solution: ProgramSpec,
    pub interactor: ProgramSpec,
    pub wall_deadline: Duration,
    pub interactor_allowance: Duration,
    pub peer_allowance: Duration,
    /// Defaults to max(S wall, I wall) + 1 s; may be shortened by the caller.
    pub session_deadline: Option<Duration>,
    /// S→I and I→S, respectively.
    pub dialogue_cap: [u64; 2],
    pub queue_bytes: usize,
    pub queue_records: usize,
    pub mode: SessionMode,
    /// Caller-owned directory, created if missing; relay mode only.
    pub transcript: Option<PathBuf>,
    /// No successful forwarding for this duration stops both live roots. Relay only.
    pub idle: Option<Duration>,
    #[doc(hidden)]
    pub transcript_test: TranscriptTestOptions,
    pub cancel: CancelToken,
    /// Bounded, lossy telemetry; a full or disconnected consumer never delays runs.
    pub diagnostics: Option<tokio::sync::mpsc::Sender<TransportDiagnostic>>,
    #[doc(hidden)]
    pub spawn_options: [SpawnOptions; 2],
    #[doc(hidden)]
    /// Test readiness signals, published after the coordinator records root exit.
    pub exit_observed: [Option<CancelToken>; 2],
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub relay_start_gate: Option<CancelToken>,
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    /// Linux-only kernel pipe capacity injection.
    pub relay_pipe_bytes: Option<usize>,
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    /// Pause each relay after this forwarded prefix and notify the test.
    pub relay_pause_after: Option<(u64, CancelToken)>,
}
impl SessionSpec {
    pub fn new(
        mut solution: ProgramSpec,
        mut interactor: ProgramSpec,
        wall_deadline: Duration,
    ) -> Self {
        for run in [&mut solution, &mut interactor] {
            run.stdin = StdinSpec::Connected;
            run.stdout = OutputSpec::Connected;
        }
        Self {
            solution,
            interactor,
            wall_deadline,
            interactor_allowance: Duration::from_secs(1),
            peer_allowance: Duration::from_secs(1),
            session_deadline: None,
            dialogue_cap: [64 * 1024 * 1024; 2],
            queue_bytes: 256 * 1024,
            queue_records: 4096,
            mode: SessionMode::Relay,
            transcript: None,
            idle: None,
            transcript_test: TranscriptTestOptions::default(),
            cancel: CancelToken::new(),
            diagnostics: None,
            spawn_options: std::array::from_fn(|_| SpawnOptions::default()),
            exit_observed: [None, None],
            #[cfg(debug_assertions)]
            relay_start_gate: None,
            #[cfg(debug_assertions)]
            relay_pipe_bytes: None,
            #[cfg(debug_assertions)]
            relay_pause_after: None,
        }
    }
}
#[derive(Debug)]
pub enum RunRecord {
    NotStarted(StartFailure),
    Started(Box<RootOutcome>),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RelayState {
    #[default]
    Open,
    HalfClosed,
    SinkGone,
    Closed,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnreadPipeContents {
    KnownZero,
    #[default]
    Unknown,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeliveryIncomplete {
    pub bytes: u64,
    pub unread_pipe_contents: UnreadPipeContents,
}
#[derive(Clone, Debug, Default)]
pub struct RelayStatistics {
    pub bytes_read: u64,
    pub bytes_forwarded: u64,
    pub discarded_over_cap: u64,
    pub bytes_queued_at_end: u64,
    pub peak_queued_bytes: u64,
    pub peak_queued_records: usize,
    pub source_eof: bool,
    pub final_state: RelayState,
    /// HalfClosed/SinkGone remain visible after the final Closed transition.
    pub transitions: Vec<RelayState>,
    pub delivery_incomplete: DeliveryIncomplete,
}
/// Direct pipes cannot provide dialogue measurements.
#[derive(Clone, Debug)]
pub enum DirectionStatistics {
    Available(RelayStatistics),
    Unavailable,
}
impl DirectionStatistics {
    pub fn as_relay(&self) -> Option<&RelayStatistics> {
        match self {
            Self::Available(stats) => Some(stats),
            Self::Unavailable => None,
        }
    }
}
/// Effective relay limits. Direct pipes cannot enforce either facility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayLimits {
    Available {
        dialogue_cap: [u64; 2],
        idle: Option<Duration>,
    },
    Unavailable,
}
impl RelayLimits {
    fn new(mode: SessionMode, dialogue_cap: [u64; 2], idle: Option<Duration>) -> Self {
        match mode {
            SessionMode::Relay => Self::Available { dialogue_cap, idle },
            SessionMode::Direct => Self::Unavailable,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DialogueSnapshot {
    pub bytes_read: u64,
    pub bytes_forwarded: u64,
    pub bytes_queued: u64,
}
#[derive(Clone, Debug)]
pub struct IdleObservation {
    pub limit: Duration,
    pub elapsed: Duration,
    pub directions: [DialogueSnapshot; 2],
}
#[derive(Clone, Debug, Default)]
pub struct TranscriptTestOptions {
    /// Test-only startup delay, ignored in optimized builds.
    pub writer_delay: Duration,
    /// Inject a permanent error before writing this many raw bytes (debug only).
    pub fail_after_bytes: Option<u64>,
    /// Override the bounded queue capacity (debug only).
    pub queue_records: Option<usize>,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct TranscriptGap {
    pub first_record: u64,
    pub last_record: u64,
    pub start_offset: u64,
    pub end_offset: u64,
    pub records: u64,
    /// One for an ordinary gap; counts merged ranges in the final counter.
    pub ranges: u64,
}
#[derive(Clone, Debug, Default)]
pub struct TranscriptStatistics {
    pub retained_bytes: u64,
    /// Includes the reserved final-offset record; excludes bounded gap markers.
    pub retained_records: u64,
    pub gaps: Vec<TranscriptGap>,
    pub further_gaps: Option<TranscriptGap>,
}
#[derive(Clone, Debug)]
pub enum TranscriptStatus {
    Disabled,
    Unavailable,
    Complete,
    Gapped,
    Failed { error: BoundedText, at_offset: u64 },
}
#[derive(Clone, Debug)]
pub struct TranscriptOutcome {
    pub status: TranscriptStatus,
    pub excerpt: bool,
    pub directions: [TranscriptStatistics; 2],
}
impl TranscriptOutcome {
    fn inactive(mode: SessionMode) -> Self {
        Self {
            status: if mode == SessionMode::Direct {
                TranscriptStatus::Unavailable
            } else {
                TranscriptStatus::Disabled
            },
            excerpt: false,
            directions: std::array::from_fn(|_| TranscriptStatistics::default()),
        }
    }
}
#[derive(Debug)]
pub struct SessionOutcome {
    pub solution: RunRecord,
    pub interactor: RunRecord,
    pub record: SessionRecord,
    pub transcript: TranscriptOutcome,
    pub idle: Option<IdleObservation>,
    pub relay_limits: RelayLimits,
    pub solution_to_interactor: DirectionStatistics,
    pub interactor_to_solution: DirectionStatistics,
    pub mode: SessionMode,
    pub queue_bytes: usize,
    pub queue_records: usize,
    pub elapsed: Duration,
    #[doc(hidden)]
    pub coordinator_wakeups: u64,
}

/// Own both roots and all relay endpoints until drain, then use E's cleanup path.
/// Dropping this future drops roots into the existing bounded cleanup service.
pub async fn run_session(spec: SessionSpec) -> SessionOutcome {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::run(spec).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        failed(&spec, NotStarted::Unsupported)
    }
}

fn failed(spec: &SessionSpec, reason: NotStarted) -> SessionOutcome {
    let mut record = SessionRecord::default();
    let reason = StartFailure::from_reason(&reason);
    let mut facts = Vec::new();
    if reason.kind == StartFailureKind::CancelledBeforeStart {
        facts.push(Fact::CancelRequested);
    }
    for role in [Role::S, Role::I] {
        facts.push(Fact::NotStarted {
            role,
            reason: reason.clone(),
        });
    }
    record.wakeup(facts);
    SessionOutcome {
        solution: RunRecord::NotStarted(reason.clone()),
        interactor: RunRecord::NotStarted(reason),
        record,
        transcript: TranscriptOutcome::inactive(spec.mode),
        idle: None,
        relay_limits: RelayLimits::new(spec.mode, spec.dialogue_cap, spec.idle),
        solution_to_interactor: unavailable_or_closed(spec.mode),
        interactor_to_solution: unavailable_or_closed(spec.mode),
        mode: spec.mode,
        queue_bytes: spec.queue_bytes,
        queue_records: spec.queue_records,
        elapsed: Duration::ZERO,
        coordinator_wakeups: 0,
    }
}

fn unavailable_or_closed(mode: SessionMode) -> DirectionStatistics {
    match mode {
        SessionMode::Direct => DirectionStatistics::Unavailable,
        SessionMode::Relay => DirectionStatistics::Available(RelayStatistics {
            final_state: RelayState::Closed,
            transitions: vec![RelayState::Closed],
            ..RelayStatistics::default()
        }),
    }
}

/// Bound for flushing a transcript after the final drain (not process timing).
#[cfg(any(target_os = "linux", target_os = "macos"))]
const TRANSCRIPT_FINALIZE: Duration = Duration::from_secs(1);

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::*;
    use crate::supervisor::{
        Exceed, Sink, StopTrigger, StreamPolicy, admit_pair, root::unix as roots, stdout_pipe, sys,
    };
    use std::time::Instant;

    pub(super) async fn run(mut spec: SessionSpec) -> SessionOutcome {
        let started = Instant::now();
        if spec.cancel.is_cancelled() {
            return failed(&spec, NotStarted::CancelledBeforeStart);
        }
        if (spec.mode == SessionMode::Direct && (spec.transcript.is_some() || spec.idle.is_some()))
            || (spec.mode == SessionMode::Relay
                && (spec.queue_bytes == 0
                    || spec.queue_bytes > 256 * 1024
                    || spec.queue_records == 0
                    || spec.queue_records > 4096))
            || [&spec.solution, &spec.interactor].iter().any(|r| {
                !matches!(r.stdin, StdinSpec::Connected)
                    || !matches!(r.stdout, OutputSpec::Connected)
                    || matches!(
                        r.stderr,
                        OutputSpec::Connected
                            | OutputSpec::Capture(StreamPolicy {
                                sink: Sink::Forward(_),
                                ..
                            })
                    )
            })
        {
            return failed(&spec, NotStarted::Unsupported);
        }
        // Create every path before reserving admission or starting a worker.
        let transcript_files = match spec
            .transcript
            .as_ref()
            .map(|path| sys::TranscriptFiles::create(path))
            .transpose()
        {
            Ok(files) => files,
            Err(error) => return failed(&spec, NotStarted::Setup(error)),
        };
        let mut admissions = match admit_pair() {
            Ok(pair) => pair,
            Err(error) => return failed(&spec, error),
        };
        let wiring = (|| {
            if spec.mode == SessionMode::Direct {
                let (i_input, s_output) = sys::direct_pipe()?;
                let (s_input, i_output) = sys::direct_pipe()?;
                spec.solution.stdin = StdinSpec::Pipe(s_input);
                spec.solution.stdout =
                    OutputSpec::Pipe(crate::supervisor::ChildEnd { inner: s_output });
                spec.interactor.stdin = StdinSpec::Pipe(i_input);
                spec.interactor.stdout =
                    OutputSpec::Pipe(crate::supervisor::ChildEnd { inner: i_output });
                return Ok::<_, std::io::Error>([None, None]);
            }
            let s = stdout_pipe()?;
            let i = stdout_pipe()?;
            let (s_input, to_s) = sys::input_pipe()?;
            let (i_input, to_i) = sys::input_pipe()?;
            #[cfg(all(target_os = "linux", debug_assertions))]
            if let Some(bytes) = spec.relay_pipe_bytes {
                sys::resize_pipe(&to_s, bytes)?;
                sys::resize_pipe(&to_i, bytes)?;
            }
            spec.solution.stdin = StdinSpec::Pipe(s_input);
            spec.solution.stdout = OutputSpec::Pipe(s.child);
            spec.interactor.stdin = StdinSpec::Pipe(i_input);
            spec.interactor.stdout = OutputSpec::Pipe(i.child);
            Ok([
                Some(relay::Relay::new(
                    s.parent,
                    to_i,
                    spec.dialogue_cap[0],
                    spec.queue_bytes,
                    spec.queue_records,
                )),
                Some(relay::Relay::new(
                    i.parent,
                    to_s,
                    spec.dialogue_cap[1],
                    spec.queue_bytes,
                    spec.queue_records,
                )),
            ])
        })();
        let mut relays = match wiring {
            Ok(wiring) => wiring,
            Err(error) => return failed(&spec, NotStarted::Setup(error)),
        };
        // Retention is independent of execution limits and forwarding.
        for run in [&mut spec.solution, &mut spec.interactor] {
            let sink = match &run.stderr {
                OutputSpec::Capture(policy) => policy.sink.clone(),
                _ => Sink::Memory,
            };
            run.stderr = OutputSpec::Capture(StreamPolicy {
                sink,
                budget: 1024 * 1024,
                on_exceed: Exceed::Discard,
                events: false,
            });
        }
        let counts = [&spec.solution, &spec.interactor].map(|run| {
            usize::from(matches!(
                run.stderr,
                OutputSpec::Capture(StreamPolicy {
                    sink: Sink::File(_),
                    ..
                })
            ))
        });
        if let Err(error) =
            crate::supervisor::cleanup::reserve_pair_workers(&mut admissions, counts)
        {
            return failed(&spec, error);
        }
        let (mut transcript_writer, directions) = match transcript_files {
            Some(files) => {
                let (writer, directions) =
                    transcript::Writer::start(files, started, &spec.transcript_test);
                (Some(writer), directions)
            }
            None => (None, [None, None]),
        };
        for (relay, direction) in relays.iter_mut().zip(directions) {
            if let Some(relay) = relay {
                relay.transcript = direction;
                relay.origin = started;
                #[cfg(debug_assertions)]
                {
                    relay.pause_after = spec.relay_pause_after.clone();
                }
            }
        }
        let counters = relays.each_ref().map(|relay| {
            relay
                .as_ref()
                .map(|r| r.counters.clone())
                .unwrap_or_default()
        });
        let [s_admission, i_admission] = admissions;
        let mut record = SessionRecord::default();
        let mut records: [Option<RunRecord>; 2] = [None, None];
        let mut owned = [None, None];
        // Both preparations (credits, pipes, workers) complete before creation.
        let prepared_s = roots::prepare(spec.solution, s_admission, &spec.spawn_options[0]);
        let prepared_i = roots::prepare(spec.interactor, i_admission, &spec.spawn_options[1]);
        let preparations = [prepared_s, prepared_i];
        let preparation_failed = preparations.iter().any(Result::is_err);
        let mut pending = Vec::new();
        for (index, prepared) in preparations.into_iter().enumerate() {
            let role = [Role::S, Role::I][index];
            let result = match prepared {
                Ok(prepared) if !preparation_failed => {
                    roots::spawn_prepared(prepared, &spec.spawn_options[index])
                }
                Ok(_) => Err(NotStarted::Setup(std::io::Error::other(
                    "peer preparation failed",
                ))),
                Err(error) => Err(error),
            };
            match result {
                Ok(root) => {
                    owned[index] = Some(Box::new(root));
                    pending.push(Fact::Started { role });
                }
                Err(error) => {
                    let reason = StartFailure::from_reason(&error);
                    pending.push(Fact::NotStarted {
                        role,
                        reason: reason.clone(),
                    });
                    records[index] = Some(RunRecord::NotStarted(reason));
                }
            }
        }
        record.wakeup(pending);
        emit_events(&mut transcript_writer, &record);
        let startup_failed = records.iter().any(Option::is_some);
        if startup_failed {
            record.wakeup(
                [Role::S, Role::I]
                    .into_iter()
                    .filter(|r| owned[r.index()].is_some())
                    .map(|role| Fact::StopRequested {
                        role,
                        cause: StopCause::Teardown(Teardown::Infrastructure),
                    })
                    .collect(),
            );
        }
        emit_events(&mut transcript_writer, &record);
        let walls = [
            spec.wall_deadline,
            spec.wall_deadline.saturating_add(spec.interactor_allowance),
        ];
        let origins = std::array::from_fn::<_, 2, _>(|index| {
            owned[index]
                .as_mut()
                .map(|r| r.session_start())
                .unwrap_or(started)
        });
        let session_origin = origins[0].max(origins[1]);
        let session_deadline = session_origin
            + spec
                .session_deadline
                .unwrap_or(walls[1].saturating_add(Duration::from_secs(1)));
        let mut first_exit: Option<Instant> = None;
        let (s_progress, mut s_updates) = tokio::sync::watch::channel(relay::Progress::default());
        let (i_progress, mut i_updates) = tokio::sync::watch::channel(relay::Progress::default());
        let shutdown = CancelToken::new();
        let mut transport_seen = [[false; 2]; 2];
        let mut coordinator_wakeups = 0;
        let mut idle_observation = None;
        let mut drain_deadline = started;
        let coordinate = async {
            let mut applied = [false, false];
            loop {
                let relays = [
                    s_updates.borrow_and_update().clone(),
                    i_updates.borrow_and_update().clone(),
                ];
                let mut pending = Vec::new();
                // Non-consuming exit checks precede every timer/cancel reconciliation.
                for role in [Role::S, Role::I] {
                    let index = role.index();
                    if record.exited(role) {
                        continue;
                    }
                    if let Some(root) = owned[index].as_mut() {
                        if let Ok(Some(status)) = root.session_observe(walls[index]) {
                            pending.push(Fact::RootExited {
                                role,
                                status,
                                provenance: record
                                    .stop(role)
                                    .map_or(Provenance::Natural, Provenance::Stopped),
                            });
                            first_exit.get_or_insert_with(Instant::now);
                        }
                    }
                }
                let now = Instant::now();
                if spec.cancel.is_cancelled() && !record.cancelled() {
                    pending.push(Fact::CancelRequested);
                }
                let idle_deadline = spec
                    .idle
                    .filter(|_| {
                        [Role::S, Role::I].into_iter().all(|role| {
                            owned[role.index()].is_some()
                                && !record.exited(role)
                                && record.stop(role).is_none()
                                && !pending.iter().any(
                                    |f| matches!(f, Fact::RootExited { role: r, .. } if *r == role),
                                )
                        })
                    })
                    .map(|idle| {
                        let last = counters
                            .iter()
                            .map(|c| c.last_forward_ns.load(std::sync::atomic::Ordering::Acquire))
                            .max()
                            .unwrap_or(0);
                        (started + Duration::from_nanos(last)).max(session_origin) + idle
                    });
                if idle_deadline.is_some_and(|deadline| now >= deadline) {
                    idle_observation = Some(IdleObservation {
                        limit: spec.idle.expect("idle deadline requires a limit"),
                        elapsed: started.elapsed(),
                        directions: counters.each_ref().map(|c| c.snapshot()),
                    });
                    for role in [Role::S, Role::I] {
                        pending.push(Fact::StopRequested {
                            role,
                            cause: StopCause::Idle,
                        });
                    }
                }
                for role in [Role::S, Role::I] {
                    let index = role.index();
                    if owned[index].is_some() {
                        transport_facts(
                            &relays[index],
                            role,
                            &mut transport_seen[index],
                            &mut pending,
                        );
                    }
                    if owned[index].is_none()
                        || record.exited(role)
                        || pending
                            .iter()
                            .any(|f| matches!(f, Fact::RootExited { role: r, .. } if *r == role))
                    {
                        continue;
                    }
                    if now >= origins[index] + walls[index] {
                        pending.push(Fact::StopRequested {
                            role,
                            cause: StopCause::OwnLimit(OwnLimit::Wall),
                        });
                    }
                    if first_exit.is_some_and(|exit| now >= exit + spec.peer_allowance) {
                        pending.push(Fact::StopRequested {
                            role,
                            cause: StopCause::PeerAllowanceExpired,
                        });
                    }
                    if now >= session_deadline {
                        pending.push(Fact::StopRequested {
                            role,
                            cause: StopCause::SessionLimit,
                        });
                    }
                    if spec.cancel.is_cancelled() {
                        pending.push(Fact::StopRequested {
                            role,
                            cause: StopCause::Teardown(Teardown::Cancel),
                        });
                    }
                    if record.infrastructure() {
                        pending.push(Fact::StopRequested {
                            role,
                            cause: StopCause::Teardown(Teardown::Infrastructure),
                        });
                    }
                }
                let recovering = owned.iter().enumerate().any(|(index, root)| {
                    let role = [Role::S, Role::I][index];
                    root.as_ref()
                        .and_then(|r| r.session_reap_deadline())
                        .is_some_and(|deadline| now >= deadline)
                        && !record.exited(role)
                        && !pending.iter().any(
                            |fact| matches!(fact, Fact::RootExited { role: r, .. } if *r == role),
                        )
                });
                if recovering {
                    pending.push(Fact::Infrastructure {
                        detail: InfrastructureDetail::new(
                            InfrastructureKind::Supervisor,
                            "root exit not observed before reap deadline",
                        ),
                        suppressed: 0,
                    });
                    for role in [Role::S, Role::I] {
                        if owned[role.index()].is_some() && !record.exited(role) {
                            pending.push(Fact::StopRequested {
                                role,
                                cause: StopCause::Teardown(Teardown::Infrastructure),
                            });
                        }
                    }
                }
                record.wakeup_with_diagnostics(pending, spec.diagnostics.as_ref());
                emit_events(&mut transcript_writer, &record);
                for role in [Role::S, Role::I] {
                    if record.exited(role) {
                        if let Some(observed) = &spec.exit_observed[role.index()] {
                            observed.cancel();
                        }
                    }
                }
                // Commit facts before signalling; later triggers cannot replace them.
                for role in [Role::S, Role::I] {
                    let index = role.index();
                    if !applied[index] && record.stop(role).is_some() && !record.exited(role) {
                        let cause = record
                            .events()
                            .iter()
                            .find_map(|event| match event.fact {
                                Fact::StopRequested { role: r, cause } if r == role => Some(cause),
                                _ => None,
                            })
                            .unwrap();
                        let trigger = match cause {
                            StopCause::OwnLimit(OwnLimit::Wall) => Some(StopTrigger::WallDeadline),
                            StopCause::OwnLimit(OwnLimit::DialogueOutput) => {
                                Some(StopTrigger::OutputBudget(crate::supervisor::Stream::Stdout))
                            }
                            StopCause::Teardown(Teardown::Cancel) => Some(StopTrigger::Cancelled),
                            _ => None,
                        };
                        if let Err(error) = owned[index]
                            .as_mut()
                            .unwrap()
                            .session_stop(trigger, counters[index].snapshot().bytes_read)
                        {
                            record.wakeup(vec![Fact::Infrastructure {
                                detail: InfrastructureDetail::new(
                                    InfrastructureKind::Supervisor,
                                    &error.to_string(),
                                ),
                                suppressed: 0,
                            }]);
                        }
                        emit_events(&mut transcript_writer, &record);
                        applied[index] = true;
                    }
                }
                if [Role::S, Role::I]
                    .into_iter()
                    .all(|r| owned[r.index()].is_none() || record.exited(r))
                {
                    break;
                }
                // Transfer at E's original kill/reap deadline, without a second drain.
                if recovering {
                    break;
                }
                // Only live, unstopped roots contribute policy timers. Past timers
                // must not leave a permanently ready branch while waiting for reap.
                let mut next = idle_deadline.filter(|deadline| *deadline > now);
                for role in [Role::S, Role::I] {
                    let index = role.index();
                    if owned[index].is_none() || record.exited(role) {
                        continue;
                    }
                    let deadline = if applied[index] {
                        owned[index]
                            .as_ref()
                            .unwrap()
                            .session_reap_deadline()
                            .unwrap()
                    } else {
                        let mut deadline = (origins[index] + walls[index]).min(session_deadline);
                        if let Some(exit) = first_exit {
                            deadline = deadline.min(exit + spec.peer_allowance);
                        }
                        deadline
                    };
                    next = Some(next.map_or(deadline, |current: Instant| current.min(deadline)));
                }
                let [s_root, i_root] = &mut owned;
                tokio::select! {
                    _ = root_ready(s_root), if !record.exited(Role::S) => {},
                    _ = root_ready(i_root), if !record.exited(Role::I) => {},
                    _ = s_updates.changed(), if spec.mode == SessionMode::Relay => {},
                    _ = i_updates.changed(), if spec.mode == SessionMode::Relay => {},
                    _ = tokio::time::sleep_until(next.expect("live root has a deadline").into()) => {},
                    _ = spec.cancel.cancelled(), if !record.cancelled() => {},
                }
            }
            // Keep descendants and their inherited writers alive throughout drain.
            drain_deadline = Instant::now() + Duration::from_millis(500);
            let transport_deadline = drain_deadline;
            let roots_closed = [Role::S, Role::I]
                .into_iter()
                .all(|r| owned[r.index()].is_none() || record.exited(r));
            loop {
                let relays = [
                    s_updates.borrow_and_update().clone(),
                    i_updates.borrow_and_update().clone(),
                ];
                let mut pending = Vec::new();
                for role in [Role::S, Role::I] {
                    if owned[role.index()].is_some() {
                        transport_facts(
                            &relays[role.index()],
                            role,
                            &mut transport_seen[role.index()],
                            &mut pending,
                        );
                    }
                }
                record.wakeup_with_diagnostics(pending, spec.diagnostics.as_ref());
                emit_events(&mut transcript_writer, &record);
                if spec.mode == SessionMode::Direct
                    || !roots_closed
                    || relays.iter().all(|r| r.done)
                    || Instant::now() >= transport_deadline
                {
                    break;
                }
                tokio::select! {
                    _ = s_updates.changed(), if spec.mode == SessionMode::Relay => {},
                    _ = i_updates.changed(), if spec.mode == SessionMode::Relay => {},
                    _ = tokio::time::sleep_until(transport_deadline.into()) => {},
                    _ = spec.cancel.cancelled(), if !record.cancelled() => {
                        record.wakeup(vec![Fact::CancelRequested]);
                        emit_events(&mut transcript_writer, &record);
                    },
                }
            }
            shutdown.cancel();
        };
        let mut tasks = RelayTasks {
            handles: relays
                .into_iter()
                .zip([s_progress, i_progress])
                .map(|(relay, progress)| {
                    let shutdown = shutdown.clone();
                    #[cfg(debug_assertions)]
                    let gate = spec.relay_start_gate.clone();
                    tokio::spawn(async move {
                        drive(
                            relay,
                            progress,
                            &shutdown,
                            #[cfg(debug_assertions)]
                            gate.as_ref(),
                        )
                        .await
                    })
                })
                .collect(),
        };
        // Count actual polls, including spurious/shared-task wakeups.
        {
            let mut coordinate = std::pin::pin!(coordinate);
            std::future::poll_fn(|cx| {
                coordinator_wakeups += 1;
                std::future::Future::poll(coordinate.as_mut(), cx)
            })
            .await;
        }
        let s_stats = (&mut tasks.handles[0]).await.expect("relay task panicked");
        let i_stats = (&mut tasks.handles[1]).await.expect("relay task panicked");
        // All relay futures have stopped. Reconcile their last updates before
        // cleanup can close the record, including progress at the drain cutoff.
        let mut pending = Vec::new();
        for (role, updates) in [(Role::S, s_updates), (Role::I, i_updates)] {
            if owned[role.index()].is_some() {
                transport_facts(
                    &updates.borrow(),
                    role,
                    &mut transport_seen[role.index()],
                    &mut pending,
                );
            }
        }
        record.wakeup_with_diagnostics(pending, spec.diagnostics.as_ref());
        emit_events(&mut transcript_writer, &record);
        // Finalization is not process timing: it gets its own bounded window,
        // concurrent with descendant cleanup, and its workers are joined before
        // the outcome is returned, so nothing writes after it.
        let transcript_deadline = Instant::now().max(drain_deadline) + TRANSCRIPT_FINALIZE;
        let transcript = async {
            match transcript_writer {
                Some(writer) => writer.finish(transcript_deadline).await,
                None => TranscriptOutcome::inactive(spec.mode),
            }
        };
        let [s_root, i_root] = owned;
        let (transcript, s_outcome, i_outcome) = tokio::join!(
            transcript,
            finish(s_root, walls[0], spec.cancel.clone()),
            finish(i_root, walls[1], spec.cancel.clone())
        );
        let mut pending = Vec::new();
        for (role, outcome) in [(Role::S, s_outcome), (Role::I, i_outcome)] {
            if let Some(outcome) = outcome {
                if !record.exited(role) {
                    pending.push(Fact::RootExited {
                        role,
                        status: outcome.status,
                        provenance: record
                            .stop(role)
                            .map_or(Provenance::Natural, Provenance::Stopped),
                    });
                }
                pending.push(Fact::CleanupResult {
                    role,
                    cleanup: outcome.termination.cleanup,
                });
                if !outcome.errors.is_empty()
                    || outcome
                        .stderr
                        .as_ref()
                        .is_some_and(|s| s.has_infrastructure_loss())
                {
                    pending.push(Fact::Infrastructure {
                        detail: InfrastructureDetail::new(
                            InfrastructureKind::Supervisor,
                            &format!("run errors: {:?}", outcome.errors),
                        ),
                        suppressed: 0,
                    });
                }
                records[role.index()] = Some(RunRecord::Started(Box::new(outcome)));
            }
        }
        if spec.cancel.is_cancelled() && !record.cancelled() {
            pending.push(Fact::CancelRequested);
        }
        record.wakeup(pending);
        let [s_record, i_record] = records;
        SessionOutcome {
            solution: s_record.unwrap(),
            interactor: i_record.unwrap(),
            solution_to_interactor: s_stats,
            interactor_to_solution: i_stats,
            transcript,
            idle: idle_observation,
            relay_limits: RelayLimits::new(spec.mode, spec.dialogue_cap, spec.idle),
            record,
            mode: spec.mode,
            queue_bytes: spec.queue_bytes,
            queue_records: spec.queue_records,
            elapsed: started.elapsed(),
            coordinator_wakeups,
        }
    }
    struct RelayTasks {
        handles: Vec<tokio::task::JoinHandle<DirectionStatistics>>,
    }
    impl Drop for RelayTasks {
        fn drop(&mut self) {
            for handle in &self.handles {
                handle.abort();
            }
        }
    }
    fn emit_events(writer: &mut Option<transcript::Writer>, record: &SessionRecord) {
        if let Some(writer) = writer {
            writer.record_events(record);
        }
    }
    async fn drive(
        relay: Option<relay::Relay>,
        progress: tokio::sync::watch::Sender<relay::Progress>,
        shutdown: &CancelToken,
        #[cfg(debug_assertions)] gate: Option<&CancelToken>,
    ) -> DirectionStatistics {
        match relay {
            Some(relay) => DirectionStatistics::Available(
                relay
                    .run(
                        progress,
                        shutdown,
                        #[cfg(debug_assertions)]
                        gate,
                    )
                    .await,
            ),
            None => DirectionStatistics::Unavailable,
        }
    }
    fn transport_facts(
        progress: &relay::Progress,
        role: Role,
        seen: &mut [bool; 2],
        pending: &mut Vec<Fact>,
    ) {
        if progress.exceeded && !seen[0] {
            pending.push(Fact::StopRequested {
                role,
                cause: StopCause::OwnLimit(OwnLimit::DialogueOutput),
            });
            seen[0] = true;
        }
        if let Some(error) = &progress.error {
            if !seen[1] {
                pending.push(Fact::Infrastructure {
                    detail: InfrastructureDetail::new(InfrastructureKind::Transport, error),
                    suppressed: 0,
                });
                seen[1] = true;
            }
        }
    }
    async fn root_ready(root: &mut Option<Box<roots::Root>>) {
        match root {
            Some(root) => root.session_ready().await,
            None => std::future::pending().await,
        }
    }
    async fn finish(
        root: Option<Box<roots::Root>>,
        wall: Duration,
        cancel: CancelToken,
    ) -> Option<RootOutcome> {
        Some(roots::session_finish(root?, wall, cancel).await)
    }
}
