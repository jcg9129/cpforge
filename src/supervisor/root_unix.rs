//! Unix root lifecycle policy, currently compiled only on Linux.
//!
//! The cfg-selected `os` module supplies this execution-contract checklist:
//! - Creation/limits (§3.4–3.5, §4.1–4.2, §7.2):
//!   `prepare_limits(&[LocalLimit]) -> Result<Vec<InstalledLimit>, NotStarted>`;
//!   `spawn(ProgramSpec, &[InstalledLimit], bool) -> Result<(i32, Instant), NotStarted>`.
//!   Creation establishes the group before exec, classifies limit-phase failures,
//!   and returns an owned, unreaped root with its creation-completion timestamp.
//! - Observation (§3.1, §4.1–4.2): owned `Observer: Debug + Send`;
//!   `register_observer(&mut Root, i32, &SpawnOptions)`;
//!   `async observe_root(&mut Root, i32) -> io::Result<()>`;
//!   `exit_check(Option<&Observer>, i32) -> io::Result<bool>`;
//!   `observe(i32) -> io::Result<bool>`;
//!   `observe_status(i32) -> io::Result<Option<Termination>>`.
//!   Registration/readiness failures retain ownership and fall back to polling;
//!   successful observation never consumes status. Polling uses 1–10 ms backoff.
//! - Signalling/reaping (§3.1.5–3.1.6, §4.1–4.2):
//!   `kill_group(i32) -> io::Result<()>`, `kill_root(i32) -> io::Result<()>`;
//!   `reap(i32) -> io::Result<Option<(Termination, RawRusage)>>`.
//!   The core authorizes signals and revokes authority before consuming waits;
//!   `RawRusage` is normalized to bytes and durations by the OS (§7.1).
//! - Verification (§4.4): owned `CleanupScan: Debug + Send`;
//!   `cleanup_scan(i32, Option<&Observer>) -> Result<CleanupScan, UnverifiedReason>`;
//!   `CleanupScan::set_entry_delay(&mut self, Duration)` (test injection);
//!   `CleanupScan::poll(&mut self, Instant, Option<Instant>) -> Option<Cleanup>`.
//!   Scans retain progress across transfer; only a completed supported scan confirms.
//! - Startup (§3.5): `init_process() -> io::Result<()>` marks inherited fds ≥3 CLOEXEC.
//!   The caller separately resets SIGCHLD before sanitization and service startup.
//!
//! Linux delegates native operations to `sys` (pidfd/procfs, phase pipe, KiB RSS).
//! A later Darwin selection must supply the same interface with its own mechanisms.

use super::*;
use crate::supervisor::capture::{self, Captures};
use std::sync::Mutex;

#[cfg(target_os = "linux")]
#[path = "root_linux.rs"]
mod os;
use os as sys;
pub(super) use os::init_process;
use os::observe_root as observe;

pub(in crate::supervisor) const REAP_DEADLINE: Duration = Duration::from_secs(2);

/// A registry entry moves with its sole owner (root, then cleanup service).
/// No signalling handle escapes this entry, so unrelated runs never contend
/// for its capability lock. The admission key distinguishes reused numeric PIDs.
#[derive(Debug)]
struct Lease {
    pid: i32,
    key: u64,
    capable: Mutex<bool>,
}

impl Lease {
    fn kill(&self) -> io::Result<()> {
        let capable = self.capable.try_lock().expect("exclusive lease owner");
        if *capable {
            sys::kill_group(self.pid)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn kill_and_revoke(&self) -> io::Result<()> {
        // Only this lease's owner can access the capability; try_lock therefore
        // cannot fail. In particular, transfer never waits for a registry lock.
        let mut capable = self.capable.try_lock().expect("exclusive lease owner");
        let result = if *capable {
            sys::kill_group(self.pid)
        } else {
            Ok(())
        };
        *capable = false;
        result
    }

    fn revoke(&mut self) {
        *self.capable.get_mut().unwrap_or_else(|e| e.into_inner()) = false;
    }
}

#[derive(Debug)]
pub(in crate::supervisor) struct Root {
    lease: Option<Lease>,
    admission: Option<Admission>,
    observer: Option<sys::Observer>,
    outcome: Option<RootOutcome>,
    opts: SpawnOptions,
    controls: Option<RunControls>,
    scan: Option<sys::CleanupScan>,
    cleanup_started: bool,
    captures: Option<Captures>,
}

impl Root {
    pub(super) fn pid(&self) -> u32 {
        self.lease.as_ref().expect("root still owned").pid as u32
    }

    pub(super) fn ticket(&self) -> TransferTicket {
        self.admission.as_ref().expect("root still owned").ticket()
    }

    fn outcome(&mut self) -> &mut RootOutcome {
        self.outcome.as_mut().expect("outcome still owned")
    }

    fn wait_failed(&mut self, error: io::Error) {
        if let Some(mut lease) = self.lease.take() {
            // On every wait error, stop using this PID immediately. Its identity
            // protection may be gone; neither Drop nor the service may retry it.
            lease.revoke();
        }
        wait_failed(self.outcome(), error);
    }
}

fn wait_failed(outcome: &mut RootOutcome, error: io::Error) {
    outcome.status = Termination::Unknown;
    outcome.termination.cleanup = Cleanup::Unverified {
        reason: UnverifiedReason::ObserverFailed,
    };
    outcome
        .errors
        .push(SupervisorError::Wait(error.raw_os_error().unwrap_or(0)));
}

impl Drop for Root {
    fn drop(&mut self) {
        let transfer_cutoff = Instant::now() + capture::DRAIN;
        if let Some(captures) = &mut self.captures {
            captures.start_drain(Some(transfer_cutoff));
        }
        if let Some(mut lease) = self.lease.take() {
            let mut outcome = self.outcome.take().expect("full outcome transfers");
            // A public drop is cancellation intent. Internal reap-timeout
            // transfer is not another trigger and must not invent cancellation.
            if !outcome.errors.contains(&SupervisorError::ReapTimeout) {
                let reconciled =
                    if outcome.timing.exit_observed.is_none() && outcome.stop_trigger.is_none() {
                        let exited = if self.opts.observer_never_ready {
                            Ok(false)
                        } else {
                            sys::exit_check(self.observer.as_ref(), lease.pid)
                        };
                        match exited {
                            Ok(true) => {
                                record_exit(
                                    &mut outcome,
                                    self.controls.as_ref().map(|c| c.wall_deadline),
                                );
                                true
                            }
                            Ok(false) => true,
                            Err(error) => {
                                lease.revoke();
                                wait_failed(&mut outcome, error);
                                false
                            }
                        }
                    } else {
                        true
                    };
                if reconciled {
                    if outcome.timing.exit_observed.is_none() && outcome.stop_trigger.is_none() {
                        outcome.stop_trigger = Some(StopTrigger::Cancelled);
                    } else if outcome.stop_trigger != Some(StopTrigger::Cancelled) {
                        push_note(&mut outcome, Note::LateTrigger(StopTrigger::Cancelled));
                    }
                }
            }
            if let Some(controls) = &self.controls {
                let deadline = outcome.timing.create_end + controls.wall_deadline;
                note_late_triggers(&mut outcome, controls, deadline);
            }
            request_kill(&lease, &mut outcome);
            push_note(&mut outcome, Note::Transferred);
            self.admission
                .take()
                .expect("reserved transfer entry")
                .transfer(Transfer {
                    key: lease.key,
                    lease: Some(lease),
                    physical_done: false,
                    logical_published: false,
                    transfer_cutoff,
                    root_gate: self.opts.transfer_root_gate.clone(),
                    captures: self.captures.take(),
                    outcome,
                    observer: self.observer.take(),
                    scan: self.scan.take(),
                    cleanup_started: self.cleanup_started,
                    deadline: self
                        .controls
                        .as_ref()
                        .map(|controls| controls.wall_deadline),
                });
        } else {
            self.observer.take();
            if let (Some(captures), Some(outcome), Some(admission)) = (
                self.captures.take(),
                self.outcome.take(),
                self.admission.take(),
            ) {
                if captures.is_empty() {
                    return;
                }
                let key = admission.key();
                admission.transfer(Transfer {
                    key,
                    lease: None,
                    outcome,
                    observer: None,
                    scan: None,
                    cleanup_started: true,
                    deadline: None,
                    physical_done: true,
                    logical_published: false,
                    transfer_cutoff,
                    root_gate: self.opts.transfer_root_gate.clone(),
                    captures: Some(captures),
                });
            }
        }
    }
}

fn push_note(outcome: &mut RootOutcome, note: Note) {
    if outcome.notes.len() < 16 && !outcome.notes.contains(&note) {
        outcome.notes.push(note);
    }
}

fn push_error(outcome: &mut RootOutcome, error: SupervisorError) {
    if outcome.errors.len() < 16 && !outcome.errors.contains(&error) {
        outcome.errors.push(error);
    }
}

fn request_kill(lease: &Lease, outcome: &mut RootOutcome) {
    outcome.kill_attempts = outcome.kill_attempts.saturating_add(1);
    outcome
        .termination
        .requested_at
        .get_or_insert_with(Instant::now);
    outcome.termination.method = Some("killpg(SIGKILL)");
    if let Err(error) = lease.kill() {
        push_error(
            outcome,
            SupervisorError::Kill(error.raw_os_error().unwrap_or(0)),
        );
    }
}

fn record_exit(outcome: &mut RootOutcome, deadline: Option<Duration>) {
    let observed = *outcome
        .timing
        .exit_observed
        .get_or_insert_with(Instant::now);
    let wall = observed.duration_since(outcome.timing.create_end);
    outcome.timing.wall = Some(wall);
    if let Some(deadline) = deadline {
        if outcome.enforced_at.is_some() || wall > deadline.saturating_add(DEADLINE_GRACE) {
            if !outcome
                .violations
                .iter()
                .any(|v| matches!(v, Violation::WallDeadline { .. }))
            {
                outcome.violations.push(Violation::WallDeadline {
                    observed_by: observed,
                });
            }
        } else if wall > deadline {
            push_note(outcome, Note::DeadlineGrace);
        }
    }
}

pub(super) fn spawn(
    spec: ProgramSpec,
    admission: Admission,
    opts: &SpawnOptions,
) -> Result<Root, NotStarted> {
    spawn_prepared(prepare(spec, admission, opts)?, opts)
}

pub(in crate::supervisor) struct Prepared {
    spec: ProgramSpec,
    captures: Captures,
    limits: Vec<InstalledLimit>,
    setup_start: Instant,
}

pub(in crate::supervisor) fn prepare(
    mut spec: ProgramSpec,
    admission: Admission,
    opts: &SpawnOptions,
) -> Result<Prepared, NotStarted> {
    let setup_start = Instant::now();
    let limits = sys::prepare_limits(&spec.limits)?;
    let captures = Captures::prepare(&mut spec.stdout, &mut spec.stderr, opts, admission)?;
    Ok(Prepared {
        spec,
        captures,
        limits,
        setup_start,
    })
}

pub(in crate::supervisor) fn spawn_prepared(
    prepared: Prepared,
    opts: &SpawnOptions,
) -> Result<Root, NotStarted> {
    let Prepared {
        spec,
        mut captures,
        limits,
        setup_start,
    } = prepared;
    let measurements = crate::supervisor::accounting::measurements(&spec.measure);
    let mut notes = Vec::with_capacity(16);
    if limits.iter().any(|limit| {
        limit
            .installed
            .is_some_and(|value| value < limit.normalised)
    }) {
        notes.push(Note::LimitLowered);
    }
    let errors = Vec::with_capacity(16);
    let (pid, create_end) = sys::spawn(spec, &limits, opts.fail_limit_install)?;
    let admission = captures.take_admission();
    captures.start_readers();
    let lease = Lease {
        pid,
        key: admission.key(),
        capable: Mutex::new(true),
    };
    let mut root = Root {
        lease: Some(lease),
        admission: Some(admission),
        observer: None,
        opts: opts.clone(),
        controls: None,
        scan: None,
        cleanup_started: false,
        captures: Some(captures),
        outcome: Some(RootOutcome {
            kill_attempts: 0,
            stdout: None,
            stderr: None,
            status: Termination::Unknown,
            stop_trigger: None,
            enforced_at: None,
            termination: TerminationEvidence {
                requested_at: None,
                method: None,
                cleanup: Cleanup::Unverified {
                    reason: UnverifiedReason::RootNotObserved,
                },
            },
            violations: Vec::with_capacity(3),
            measurements,
            limits,
            timing: RootTiming {
                setup: create_end.duration_since(setup_start),
                create_end,
                exit_observed: None,
                observer: ObserverKind::Polling,
                wall: None,
                method: WallMethod::Observed,
                grace: DEADLINE_GRACE,
            },
            notes,
            errors,
        }),
    };
    sys::register_observer(&mut root, pid, opts);
    Ok(root)
}

pub(in crate::supervisor) async fn wait(mut root: Box<Root>, controls: RunControls) -> RootOutcome {
    let pid = root.pid() as i32;
    let deadline = root.outcome().timing.create_end + controls.wall_deadline;
    root.controls = Some(controls.clone());
    let mut budget = root.captures.as_ref().unwrap().budget_receiver();
    root.captures
        .as_ref()
        .unwrap()
        .set_events(controls.events.clone());
    if let Some(gate) = root.opts.supervisor_gate.take() {
        gate.cancelled().await;
    }
    // Both selection orders feed the same nonblocking reconciliation. The
    // hidden option exercises both orders with exit and timer already ready.
    let event = if root.opts.timer_first {
        tokio::select! { biased;
            _ = tokio::time::sleep_until(deadline.into()) => Ok(Some(StopTrigger::WallDeadline)),
            result = observe(&mut root, pid) => result.map(|()| None),
            _ = controls.cancel.cancelled() => Ok(Some(StopTrigger::Cancelled)),
            stream = budget.wait_for(|value| value.is_some()) => Ok(stream.ok().and_then(|value| (*value).map(StopTrigger::OutputBudget))),
        }
    } else {
        tokio::select! { biased;
            result = observe(&mut root, pid) => result.map(|()| None),
            _ = tokio::time::sleep_until(deadline.into()) => Ok(Some(StopTrigger::WallDeadline)),
            _ = controls.cancel.cancelled() => Ok(Some(StopTrigger::Cancelled)),
            stream = budget.wait_for(|value| value.is_some()) => Ok(stream.ok().and_then(|value| (*value).map(StopTrigger::OutputBudget))),
        }
    };
    let trigger = match event {
        Err(error) => {
            root.wait_failed(error);
            return finish_capture(&mut root, false).await;
        }
        Ok(trigger) => trigger,
    };
    let exited = if trigger.is_none() {
        Ok(true)
    } else if root.opts.observer_never_ready {
        Ok(false)
    } else {
        sys::exit_check(root.observer.as_ref(), pid)
    };
    let reap_deadline = match exited {
        Err(error) => {
            root.wait_failed(error);
            return finish_capture(&mut root, false).await;
        }
        Ok(true) => {
            record_exit(root.outcome(), Some(controls.wall_deadline));
            if let Some(trigger) = trigger {
                push_note(root.outcome(), Note::LateTrigger(trigger));
            }
            root.outcome().timing.exit_observed.unwrap() + REAP_DEADLINE
        }
        Ok(false) => {
            let trigger = trigger.expect("an unobserved exit has a stop trigger");
            // A session may already have committed this root's first trigger.
            root.outcome().stop_trigger.get_or_insert(trigger);
            if trigger == StopTrigger::WallDeadline && root.outcome().enforced_at.is_none() {
                let committed = Instant::now();
                root.outcome().enforced_at =
                    Some(committed.duration_since(root.outcome().timing.create_end));
                root.outcome().violations.push(Violation::WallDeadline {
                    observed_by: committed,
                });
            }
            request_kill(root.lease.as_ref().unwrap(), root.outcome.as_mut().unwrap());
            let reap_deadline = root.outcome().termination.requested_at.unwrap() + REAP_DEADLINE;
            match tokio::time::timeout_at(reap_deadline.into(), observe(&mut root, pid)).await {
                Ok(Ok(())) => record_exit(root.outcome(), Some(controls.wall_deadline)),
                Ok(Err(error)) => {
                    root.wait_failed(error);
                    return finish_capture(&mut root, false).await;
                }
                Err(_) => return reap_timeout(root).await,
            }
            reap_deadline
        }
    };
    note_late_triggers(root.outcome(), &controls, deadline);
    finish_observed(root, reap_deadline, controls).await
}

async fn finish_observed(
    mut root: Box<Root>,
    reap_deadline: Instant,
    controls: RunControls,
) -> RootOutcome {
    let pid = root.pid() as i32;
    let deadline = root.outcome().timing.create_end + controls.wall_deadline;
    // Post-exit containment kill retains the capability throughout verification.
    request_kill(root.lease.as_ref().unwrap(), root.outcome.as_mut().unwrap());
    let cleanup_deadline = Instant::now() + Duration::from_secs(1);
    root.cleanup_started = true;
    match sys::cleanup_scan(pid, root.observer.as_ref()) {
        Ok(mut scan) => {
            scan.set_entry_delay(root.opts.cleanup_entry_delay);
            root.scan = Some(scan);
        }
        Err(reason) => root.outcome().termination.cleanup = Cleanup::Unverified { reason },
    }
    while let Some(scan) = root.scan.as_mut() {
        if let Some(cleanup) = scan.poll(cleanup_deadline, Some(reap_deadline)) {
            root.outcome().termination.cleanup = cleanup;
            if cleanup
                == (Cleanup::Unverified {
                    reason: UnverifiedReason::ReapDeadline,
                })
            {
                // Retain the unfinished scan and signalling capability for the service.
                return reap_timeout(root).await;
            }
            root.scan = None;
            break;
        }
        tokio::time::sleep_until(
            (Instant::now() + Duration::from_millis(1))
                .min(reap_deadline)
                .into(),
        )
        .await;
    }
    root.lease.as_mut().unwrap().revoke();
    let mut backoff = 1;
    loop {
        if Instant::now() >= reap_deadline {
            return reap_timeout(root).await;
        }
        let result = if root.opts.reap_never_ready {
            Ok(None)
        } else {
            sys::reap(pid)
        };
        match result {
            Ok(Some((status, usage))) => {
                root.outcome().status = status;
                crate::supervisor::accounting::record_usage(root.outcome(), usage);
                root.lease.take();
                break;
            }
            Ok(None) => {
                push_error(root.outcome(), SupervisorError::Reap);
                tokio::time::sleep_until(
                    (Instant::now() + Duration::from_millis(backoff))
                        .min(reap_deadline)
                        .into(),
                )
                .await;
                backoff = (backoff * 2).min(10);
            }
            Err(error) => {
                root.wait_failed(error);
                break;
            }
        }
    }
    note_late_triggers(root.outcome(), &controls, deadline);
    finish_capture(&mut root, false).await
}

async fn finish_capture(root: &mut Root, transferred: bool) -> RootOutcome {
    if let Some(captures) = &mut root.captures {
        captures
            .finish(root.outcome.as_mut().unwrap(), transferred)
            .await;
        if captures.resources_released() {
            root.captures.take();
            return root.outcome.take().expect("final outcome");
        }
    }
    root.outcome.as_ref().expect("final outcome").clone()
}

fn note_late_triggers(outcome: &mut RootOutcome, controls: &RunControls, deadline: Instant) {
    let observed = outcome.timing.exit_observed.unwrap_or_else(Instant::now);
    if controls
        .cancel
        .requested_at()
        .is_some_and(|requested| requested <= observed)
        && outcome.stop_trigger != Some(StopTrigger::Cancelled)
    {
        push_note(outcome, Note::LateTrigger(StopTrigger::Cancelled));
    }
    if observed >= deadline && outcome.stop_trigger != Some(StopTrigger::WallDeadline) {
        push_note(outcome, Note::LateTrigger(StopTrigger::WallDeadline));
    }
}

async fn reap_timeout(mut root: Box<Root>) -> RootOutcome {
    if let Some(controls) = root.controls.clone() {
        let deadline = root.outcome().timing.create_end + controls.wall_deadline;
        note_late_triggers(root.outcome(), &controls, deadline);
    }
    push_error(root.outcome(), SupervisorError::ReapTimeout);
    root.outcome().status = Termination::Unknown;
    if root.outcome().timing.exit_observed.is_none() {
        root.outcome().termination.cleanup = Cleanup::Unverified {
            reason: UnverifiedReason::RootNotObserved,
        };
    }
    push_note(root.outcome(), Note::Transferred);
    // The logical outcome is immutable. Physical ownership and a separate copy
    // of the partial record move to the service, whose result goes to the ticket.
    if let Some(captures) = &mut root.captures {
        captures.finish(root.outcome.as_mut().unwrap(), true).await;
    }
    root.outcome().clone()
}

/// Full partially populated record, moved into its pre-reserved transfer entry.
#[derive(Debug)]
pub(in crate::supervisor) struct Transfer {
    key: u64,
    lease: Option<Lease>,
    physical_done: bool,
    logical_published: bool,
    transfer_cutoff: Instant,
    root_gate: Option<CancelToken>,
    captures: Option<Captures>,
    pub(in crate::supervisor) outcome: RootOutcome,
    observer: Option<sys::Observer>,
    scan: Option<sys::CleanupScan>,
    cleanup_started: bool,
    deadline: Option<Duration>,
}

impl Transfer {
    pub(in crate::supervisor) fn key(&self) -> u64 {
        self.key
    }

    pub(in crate::supervisor) fn close_observer(&mut self) {
        self.observer.take();
    }

    /// One bounded, nonblocking poll. True means this record is finalized.
    pub(in crate::supervisor) fn poll(&mut self) -> bool {
        if !self.physical_done {
            self.physical_done = self.poll_root();
        }
        let Some(captures) = &mut self.captures else {
            return self.physical_done;
        };
        let frozen = captures.poll_freeze(&mut self.outcome);
        self.physical_done && frozen && captures.resources_released()
    }

    pub(in crate::supervisor) fn logical_outcome(&mut self) -> Option<RootOutcome> {
        if self.logical_published {
            return None;
        }
        let frozen = self
            .captures
            .as_mut()
            .is_none_or(|captures| captures.poll_freeze(&mut self.outcome));
        if frozen && (self.physical_done || Instant::now() >= self.transfer_cutoff) {
            self.logical_published = true;
            return Some(self.outcome.clone());
        }
        None
    }

    fn poll_root(&mut self) -> bool {
        if self
            .root_gate
            .as_ref()
            .is_some_and(|gate| !gate.is_cancelled())
        {
            return false;
        }
        let lease = self.lease.as_mut().expect("physical root owned");
        if self.outcome.termination.cleanup
            == (Cleanup::Unverified {
                reason: UnverifiedReason::ObserverFailed,
            })
        {
            return true;
        }
        if self.outcome.timing.exit_observed.is_none() {
            match sys::observe(lease.pid) {
                Ok(false) => return false,
                Ok(true) => record_exit(&mut self.outcome, self.deadline),
                Err(error) => {
                    lease.revoke();
                    wait_failed(&mut self.outcome, error);
                    return true;
                }
            }
        }
        if !self.cleanup_started {
            record_exit(&mut self.outcome, self.deadline);
            request_kill(lease, &mut self.outcome);
            self.cleanup_started = true;
            match sys::cleanup_scan(lease.pid, self.observer.as_ref()) {
                Ok(scan) => self.scan = Some(scan),
                Err(reason) => self.outcome.termination.cleanup = Cleanup::Unverified { reason },
            }
        }
        if let Some(scan) = self.scan.as_mut() {
            let deadline = self.outcome.timing.exit_observed.unwrap() + Duration::from_secs(1);
            match scan.poll(deadline, None) {
                Some(cleanup) => {
                    self.outcome.termination.cleanup = cleanup;
                    self.scan = None;
                }
                None => return false,
            }
        }
        lease.revoke();
        match sys::reap(lease.pid) {
            Ok(Some((status, usage))) => {
                self.outcome.status = status;
                crate::supervisor::accounting::record_usage(&mut self.outcome, usage);
                true
            }
            Ok(None) => false,
            Err(error) => {
                lease.revoke();
                wait_failed(&mut self.outcome, error);
                true
            }
        }
    }
}

impl Root {
    pub(in crate::supervisor) async fn session_ready(&mut self) {
        let pid = self.pid() as i32;
        if let Err(error) = observe(self, pid).await {
            self.wait_failed(error);
        }
    }

    pub(in crate::supervisor) fn session_reap_deadline(&self) -> Option<Instant> {
        let outcome = self.outcome.as_ref().unwrap();
        outcome
            .termination
            .requested_at
            .or(outcome.timing.exit_observed)
            .map(|origin| origin + REAP_DEADLINE)
    }

    pub(in crate::supervisor) fn session_start(&mut self) -> Instant {
        self.captures.as_ref().unwrap().set_events(None);
        self.outcome().timing.create_end
    }

    pub(in crate::supervisor) fn session_observe(
        &mut self,
        wall: Duration,
    ) -> io::Result<Option<Termination>> {
        if self.lease.is_none() || self.outcome().timing.exit_observed.is_some() {
            return Ok(Some(self.outcome().status));
        }
        let ready = if self.opts.observer_never_ready {
            Ok(false)
        } else {
            sys::exit_check(self.observer.as_ref(), self.pid() as i32)
        };
        let status = match ready {
            Ok(true) => sys::observe_status(self.pid() as i32),
            Ok(false) => Ok(None),
            Err(error) => Err(error),
        };
        match status {
            Ok(Some(status)) => {
                record_exit(self.outcome(), Some(wall));
                self.outcome().status = status;
                Ok(Some(status))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.wait_failed(error);
                Ok(Some(Termination::Unknown))
            }
        }
    }

    pub(in crate::supervisor) fn session_stop(
        &mut self,
        trigger: Option<StopTrigger>,
        dialogue_offset: u64,
    ) -> io::Result<()> {
        if let Some(trigger) = trigger {
            self.outcome().stop_trigger = Some(trigger);
            if trigger == StopTrigger::WallDeadline {
                let now = Instant::now();
                self.outcome().enforced_at =
                    Some(now.duration_since(self.outcome().timing.create_end));
                self.outcome()
                    .violations
                    .push(Violation::WallDeadline { observed_by: now });
            }
            if let StopTrigger::OutputBudget(stream) = trigger {
                self.outcome().violations.push(Violation::OutputBudget {
                    stream,
                    offset: dialogue_offset,
                });
            }
        }
        // A root-only kill preserves descendant writers until final drain.
        // Numeric signalling is allowed only while this lease retains authority.
        let Some(lease) = self.lease.as_ref() else {
            return Ok(());
        };
        if !*lease.capable.try_lock().expect("exclusive lease owner") {
            return Ok(());
        }
        let pid = lease.pid;
        self.outcome()
            .termination
            .requested_at
            .get_or_insert_with(Instant::now);
        self.outcome().termination.method = Some("kill(SIGKILL)");
        let outcome = self.outcome();
        outcome.kill_attempts = outcome.kill_attempts.saturating_add(1);
        let result = sys::kill_root(pid);
        if let Err(error) = &result {
            push_error(
                self.outcome(),
                SupervisorError::Kill(error.raw_os_error().unwrap_or(0)),
            );
        }
        result
    }
}

pub(in crate::supervisor) async fn session_finish(
    mut root: Box<Root>,
    wall: Duration,
    cancel: CancelToken,
) -> RootOutcome {
    let controls = RunControls {
        wall_deadline: wall,
        cancel,
        events: None,
    };
    root.controls = Some(controls.clone());
    if root.lease.is_none() {
        return finish_capture(&mut root, false).await;
    }
    // Continue the original kill/reap deadline without inventing a trigger.
    let reap_deadline = root
        .session_reap_deadline()
        .expect("session exited or stopped root");
    if root.outcome().timing.exit_observed.is_none() {
        let pid = root.pid() as i32;
        match tokio::time::timeout_at(reap_deadline.into(), observe(&mut root, pid)).await {
            Ok(Ok(())) => record_exit(root.outcome(), Some(wall)),
            Ok(Err(error)) => {
                root.wait_failed(error);
                return finish_capture(&mut root, false).await;
            }
            Err(_) => return reap_timeout(root).await,
        }
    }
    finish_observed(root, reap_deadline, controls).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpf_fixture::harness::fixture_path;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn observed_wall_judgment_has_exact_grace_boundaries_and_keeps_enforcement() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::supervisor::init_process().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    let mut root = spawn(
                        ProgramSpec {
                            program: fixture_path(),
                            args: vec!["exit:0".into()],
                            cwd: std::env::current_dir().unwrap(),
                            env: Vec::new(),
                            stdin: StdinSpec::Null,
                            stdout: OutputSpec::Null,
                            stderr: OutputSpec::Null,
                            limits: Vec::new(),
                            measure: vec![MetricKind::PeakRss, MetricKind::CpuTime],
                        },
                        crate::supervisor::admit().unwrap(),
                        &SpawnOptions::default(),
                    )
                    .unwrap();
                    // Use an independently cloned record for precise clock inputs;
                    // the real root is still observed/reaped by its single owner.
                    let template = root.outcome().clone();
                    let deadline = Duration::from_secs(1);
                    for (wall, enforced, violation, grace) in [
                        (deadline, false, false, false),
                        (deadline + Duration::from_nanos(1), false, false, true),
                        (deadline + DEADLINE_GRACE, false, false, true),
                        (
                            deadline + DEADLINE_GRACE + Duration::from_nanos(1),
                            false,
                            true,
                            false,
                        ),
                        (deadline + Duration::from_millis(1), true, true, false),
                    ] {
                        let mut outcome = template.clone();
                        outcome.timing.exit_observed = Some(outcome.timing.create_end + wall);
                        outcome.enforced_at = enforced.then_some(deadline);
                        record_exit(&mut outcome, Some(deadline));
                        record_exit(&mut outcome, Some(deadline)); // capped and deduplicated
                        assert_eq!(outcome.timing.wall, Some(wall));
                        assert_eq!(outcome.violations.len(), usize::from(violation));
                        assert_eq!(outcome.notes.contains(&Note::DeadlineGrace), grace);
                    }
                    wait(
                        Box::new(root),
                        RunControls {
                            wall_deadline: deadline,
                            cancel: CancelToken::new(),
                            events: None,
                        },
                    )
                    .await;
                })
                .await
                .expect("deadline judgment test timed out");
            });
    }

    #[test]
    fn wait_errors_stop_pid_use_and_transfer_preserves_partial_outcome() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::supervisor::init_process().unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(10), async {
                    for state in 0..5 {
                        let mut root = spawn(
                            ProgramSpec {
                                program: fixture_path(),
                                args: vec!["exit:3".into()],
                                cwd: std::env::current_dir().unwrap(),
                                env: Vec::new(),
                                stdin: StdinSpec::Null,
                                stdout: OutputSpec::Null,
                                stderr: OutputSpec::Null,
                                limits: Vec::new(),
                                measure: vec![MetricKind::PeakRss, MetricKind::CpuTime],
                            },
                            crate::supervisor::admit().unwrap(),
                            &SpawnOptions {
                                force_polling_observer: true,
                                ..SpawnOptions::default()
                            },
                        )
                        .unwrap();
                        let pid = root.pid() as i32;
                        let ticket = root.ticket();
                        while !sys::observe(pid).unwrap() {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        let observed = Instant::now();
                        if state >= 2 {
                            root.outcome().timing.exit_observed = Some(observed);
                        }
                        root.lease.as_ref().unwrap().kill_and_revoke().unwrap();
                        if state != 4 {
                            // Model an external reaper, after revocation so this
                            // test cannot signal a reused process-group id.
                            assert!(sys::reap(pid).unwrap().is_some());
                        }
                        if state == 0 {
                            let outcome = wait(
                                Box::new(root),
                                RunControls {
                                    wall_deadline: Duration::from_secs(60),
                                    cancel: CancelToken::new(),
                                    events: None,
                                },
                            )
                            .await;
                            assert_eq!(outcome.status, Termination::Unknown);
                            assert_eq!(outcome.errors, [SupervisorError::Wait(libc::ECHILD)]);
                            assert!(ticket.outcome().await.is_none());
                        } else if state == 3 {
                            root.wait_failed(io::Error::from_raw_os_error(libc::EINVAL));
                            assert!(root.lease.is_none());
                            assert_eq!(root.outcome().status, Termination::Unknown);
                            assert_eq!(
                                root.outcome().errors,
                                [SupervisorError::Wait(libc::EINVAL)]
                            );
                            drop(root);
                            assert!(ticket.outcome().await.is_none());
                        } else {
                            // Retain metadata populated by the original owner.
                            root.outcome()
                                .errors
                                .push(SupervisorError::Register(libc::EIO));
                            drop(root);
                            let outcome = ticket.outcome().await.unwrap();
                            let mut expected_notes = vec![Note::ObserverFallback];
                            if state >= 2 {
                                expected_notes.push(Note::LateTrigger(StopTrigger::Cancelled));
                            }
                            expected_notes.push(Note::Transferred);
                            assert_eq!(outcome.notes, expected_notes);
                            assert_eq!(outcome.errors[0], SupervisorError::Register(libc::EIO));
                            if state == 4 {
                                assert_eq!(outcome.status, Termination::Exited { code: 3 });
                                assert!(outcome.measurements.iter().all(|m| m.value.is_some()));
                                assert_eq!(outcome.errors.len(), 1);
                            } else {
                                assert_eq!(outcome.status, Termination::Unknown);
                                assert!(outcome.measurements.iter().all(|m| m.value.is_none()));
                                assert_eq!(outcome.errors[1], SupervisorError::Wait(libc::ECHILD));
                            }
                            assert_eq!(
                                outcome.timing.exit_observed,
                                (state >= 2).then_some(observed)
                            );
                        }
                        // Logical ticket publication can precede physical slot
                        // release. Resource assertions require quiescence (E §10.2).
                        tokio::time::timeout(Duration::from_secs(1), async {
                            loop {
                                let counters = crate::supervisor::debug_counters();
                                if counters.slots_in_use == 0 && counters.service_pending == 0 {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            }
                        })
                        .await
                        .expect("cleanup service did not reach quiescence");
                        let counters = crate::supervisor::debug_counters();
                        assert_eq!(counters.slots_in_use, 0);
                        assert_eq!(counters.service_pending, 0);
                    }
                })
                .await
                .expect("wait error/partial transfer test exceeded 10 seconds");
            });
    }
}
