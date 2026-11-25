use super::value::*;
use crate::jobs::engine as e;
use crate::report as r;
use crate::supervisor as p;

pub(super) fn causes(causes: &r::InfrastructureCauses) -> Value {
    array(causes.as_slice(), cause)
}
pub(super) fn cause(c: &r::InfrastructureCause) -> Value {
    let mut fields = vec![("kind", s(c.wire_token()))];
    match c {
        r::InfrastructureCause::NotStarted { reason } => {
            fields.push(("reason", s(reason.wire_token())))
        }
        r::InfrastructureCause::StatusUnknown => {}
        r::InfrastructureCause::CleanupIncomplete { survivors } => {
            fields.push(("survivors", n(*survivors)))
        }
        r::InfrastructureCause::SupervisorError { error } => {
            fields.push(("error", s(error.wire_token())))
        }
        r::InfrastructureCause::CaptureLoss { stream, loss } => fields.extend([
            ("stream", s(stream.wire_token())),
            ("loss", s(loss.wire_token())),
        ]),
    }
    Value::Object(fields)
}
pub(super) fn class(c: &r::RunClass) -> Result<Value> {
    if matches!(c, r::RunClass::Infrastructure { causes } if causes.as_slice().iter().any(|cause| matches!(cause,
        r::InfrastructureCause::CaptureLoss { stream: r::Stream::OutputFile, .. })))
    {
        return Err(Error(
            "output_file cannot be an infrastructure capture stream",
        ));
    }
    let mut fields = vec![("kind", s(c.wire_token()))];
    match c {
        r::RunClass::Infrastructure { causes: c } => fields.extend([
            ("causes", causes(c)),
            ("causes_dropped", n(c.causes_dropped)),
        ]),
        r::RunClass::NonzeroExit { code: c } => fields.push(("code", code(*c)?)),
        r::RunClass::Signal {
            signal,
            core_dumped,
        } => fields.extend([
            ("signal", i(*signal)),
            ("name", opt(signal_name(*signal), s)),
            ("core_dumped", b(*core_dumped)),
        ]),
        r::RunClass::OutputBudget { streams } => fields.extend([
            (
                "stream",
                s(streams
                    .primary()
                    .ok_or(Error("empty output-budget streams"))?
                    .wire_token()),
            ),
            ("streams", array(streams.streams(), |v| s(v.wire_token()))),
        ]),
        _ => {}
    }
    Ok(Value::Object(fields))
}
fn signal_name(signal: i32) -> Option<&'static str> {
    #[cfg(unix)]
    {
        match signal {
            libc::SIGINT => Some("sigint"),
            libc::SIGTERM => Some("sigterm"),
            libc::SIGKILL => Some("sigkill"),
            libc::SIGSEGV => Some("sigsegv"),
            libc::SIGABRT => Some("sigabrt"),
            libc::SIGFPE => Some("sigfpe"),
            libc::SIGILL => Some("sigill"),
            libc::SIGPIPE => Some("sigpipe"),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        None
    }
}
pub fn ntstatus_name(code: i64) -> Option<&'static str> {
    match code {
        0xc0000005 => Some("status_access_violation"),
        0xc000001d => Some("status_illegal_instruction"),
        0xc0000094 => Some("status_integer_divide_by_zero"),
        0xc00000fd => Some("status_stack_overflow"),
        _ => None,
    }
}
pub(super) fn status(v: p::Termination) -> Result<Value> {
    Ok(match v {
        p::Termination::Exited { code: c } => obj([
            ("kind", s("exited")),
            ("code", code(c)?),
            ("ntstatus", opt(ntstatus_name(c), s)),
        ]),
        p::Termination::Signalled {
            signal,
            core_dumped,
        } => obj([
            ("kind", s("signalled")),
            ("signal", i(signal)),
            ("core_dumped", b(core_dumped)),
        ]),
        p::Termination::Unknown => kind("unknown"),
    })
}
pub(super) fn trigger(v: p::StopTrigger) -> Value {
    match v {
        p::StopTrigger::WallDeadline => kind("wall_deadline"),
        p::StopTrigger::Cancelled => kind("cancelled"),
        p::StopTrigger::OutputBudget(stream) => obj([
            ("kind", s("output_budget")),
            ("stream", s(stream_token(stream))),
        ]),
    }
}
pub(super) fn stream_token(v: p::Stream) -> &'static str {
    match v {
        p::Stream::Stdout => "stdout",
        p::Stream::Stderr => "stderr",
    }
}
pub(super) fn cleanup(v: p::Cleanup) -> Value {
    match v {
        p::Cleanup::Confirmed => kind("confirmed"),
        p::Cleanup::Incomplete { survivors } => {
            obj([("kind", s("incomplete")), ("survivors", n(survivors))])
        }
        p::Cleanup::Unverified { reason } => obj([
            ("kind", s("unverified")),
            (
                "reason",
                s(match reason {
                    p::UnverifiedReason::NamespaceUnknown => "namespace_unknown",
                    p::UnverifiedReason::ProcFiltered => "proc_filtered",
                    p::UnverifiedReason::ProcUnavailable => "io",
                    p::UnverifiedReason::ScanFailed => "io",
                    p::UnverifiedReason::RootNotObserved => "root_not_observed",
                    p::UnverifiedReason::ObserverFailed => "observer_failed",
                    p::UnverifiedReason::Malformed => "malformed",
                    p::UnverifiedReason::ScanIncomplete => "scan_incomplete",
                    p::UnverifiedReason::ReapDeadline => "scan_incomplete",
                    p::UnverifiedReason::Unsupported => "unsupported",
                    p::UnverifiedReason::Io => "io",
                    p::UnverifiedReason::InfoFailed => "info_failed",
                    p::UnverifiedReason::Truncated => "truncated",
                }),
            ),
        ]),
    }
}
pub(super) fn loss(v: &p::CaptureLoss) -> Value {
    match *v {
        p::CaptureLoss::BudgetDiscard => kind("budget_discard"),
        p::CaptureLoss::WriteError(error) => {
            obj([("kind", s("write_error")), ("os_error", i(error))])
        }
        p::CaptureLoss::WriteStalled => kind("write_stalled"),
        p::CaptureLoss::NoEofBeforeDrainDeadline => kind("no_eof_before_drain_deadline"),
        p::CaptureLoss::PipeRead(error) => obj([("kind", s("pipe_read")), ("os_error", i(error))]),
        p::CaptureLoss::ForwardIncomplete => kind("forward_incomplete"),
    }
}
pub(super) fn stream(
    v: Option<&p::StreamCapture>,
    file: Option<&std::path::Path>,
) -> Result<Value> {
    if v.is_none() && file.is_some() {
        return Err(Error("capture publication without capture evidence"));
    }
    Ok(obj([
        ("observed", u(v.map_or(0, |v| v.observed))),
        ("stored", u(v.map_or(0, |v| v.stored))),
        ("forwarded", u(v.map_or(0, |v| v.forwarded))),
        ("eof", b(v.is_some_and(|v| v.eof))),
        (
            "complete",
            b(file.is_some() && v.is_some_and(p::StreamCapture::complete)),
        ),
        ("exceeded_at", opt(v.and_then(|v| v.exceeded_at), u)),
        (
            "losses",
            checked_array(v.map_or(&[], |v| v.losses.as_slice()), 6, |v| Ok(loss(v)))?,
        ),
        (
            "file",
            file.map(|v| rel(v, 17)).transpose()?.unwrap_or(Value::Null),
        ),
    ]))
}
pub(super) fn metric(v: p::MetricKind) -> &'static str {
    match v {
        p::MetricKind::PeakRss => "peak_rss",
        p::MetricKind::PeakWorkingSet => "peak_working_set",
        p::MetricKind::PeakJobCommit => "peak_job_commit",
        p::MetricKind::CpuTime => "cpu_time",
        p::MetricKind::ProcessLifetime => "process_lifetime",
    }
}
pub(super) fn measurement(v: &p::Measurement, full: bool) -> Result<Value> {
    let reason = match v.reason.as_deref() {
        None if v.value.is_none() || v.completeness == p::Completeness::Partial => {
            return Err(Error("measurement needs reason"));
        }
        None => Value::Null,
        Some(reason) if full => text(reason),
        Some(reason) => s(match reason {
            "unsupported_platform" | "unsupported on this platform" => "unsupported_platform",
            "not_validated" => "not_validated",
            "cleanup_not_confirmed" | "cleanup not confirmed" => "cleanup_not_confirmed",
            "accounting_failed" | "root was not reaped" => "accounting_failed",
            _ => return Err(Error("unmapped measurement reason")),
        }),
    };
    let method = match v.method {
        "wait4 ru_utime + ru_stime" => "wait4_cpu_time",
        other => other,
    };
    Ok(obj([
        ("kind", s(metric(v.kind))),
        ("value", opt(v.value, u)),
        (
            "unit",
            s(match v.unit {
                p::Unit::Bytes => "bytes",
                p::Unit::Microseconds => "microseconds",
            }),
        ),
        (
            "scope",
            s(match v.scope {
                p::Scope::Root => "root",
                p::Scope::RootAndWaitedDescendants => "root_and_waited_descendants",
                p::Scope::Job => "job",
            }),
        ),
        (
            "aggregation",
            s(match v.aggregation {
                p::Aggregation::MaxOfProcesses => "max_of_processes",
                p::Aggregation::Sum => "sum",
                p::Aggregation::JobPeak => "job_peak",
            }),
        ),
        (
            "completeness",
            s(match v.completeness {
                p::Completeness::Final => "final",
                p::Completeness::Partial => "partial",
            }),
        ),
        ("method", supervisor_token(method)?),
        ("reason", reason),
    ]))
}
/// Supervisor method and mechanism names are descriptive (`wait4 ru_maxrss`,
/// `setrlimit RLIMIT_AS`, `killpg(SIGKILL)`); the wire carries them as tokens:
/// lowercase, each run of other characters folded to one `_`.
pub(super) fn supervisor_token(raw: &str) -> Result<Value> {
    let mut value = std::string::String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            value.push(ch.to_ascii_lowercase());
        } else if !value.is_empty() && !value.ends_with('_') {
            value.push('_');
        }
    }
    while value.ends_with('_') {
        value.pop();
    }
    token(&value)
}
pub(super) fn local(v: p::LocalLimit) -> (&'static str, u64) {
    match v {
        p::LocalLimit::AddressSpace { bytes } => ("address_space", bytes),
        p::LocalLimit::JobCommit { bytes } => ("job_commit", bytes),
    }
}
pub(super) fn outer(v: p::OuterRestriction) -> Value {
    match v {
        p::OuterRestriction::None => kind("none"),
        p::OuterRestriction::Unknown => kind("unknown"),
        p::OuterRestriction::Known(bytes) => obj([("kind", s("known")), ("bytes", u(bytes))]),
    }
}
pub(super) fn limit(v: &p::InstalledLimit, installed: bool) -> Result<Value> {
    let (kind, configured) = local(v.configured);
    let mut fields = vec![
        ("kind", s(kind)),
        ("configured", u(configured)),
        ("installed", opt(v.installed, u)),
        ("mechanism", supervisor_token(v.mechanism)?),
        ("outer", outer(v.outer)),
    ];
    if installed {
        fields.extend([
            ("normalised", u(v.normalised)),
            ("inherited_soft", opt(v.inherited.map(|v| v.0), u)),
            ("inherited_hard", opt(v.inherited.map(|v| v.1), u)),
        ]);
    }
    Ok(Value::Object(fields))
}
pub(super) fn note(v: p::Note) -> &'static str {
    match v {
        p::Note::ObserverFallback => "observer_fallback",
        p::Note::Transferred => "transferred",
        p::Note::DeadlineGrace => "deadline_grace",
        p::Note::LimitLowered => "limit_lowered",
        p::Note::LateTrigger(_) => "late_trigger",
    }
}
pub(super) fn violation(v: &p::Violation) -> Value {
    match *v {
        p::Violation::WallDeadline { .. } => kind("wall_deadline"),
        p::Violation::OutputBudget { stream, offset } => obj([
            ("kind", s("output_budget")),
            ("stream", s(stream_token(stream))),
            ("offset", u(offset)),
        ]),
    }
}
pub(super) fn started(v: &p::RootOutcome) -> Result<Value> {
    Ok(obj([
        ("kind", s("started")),
        ("status", status(v.status)?),
        ("stop_trigger", opt(v.stop_trigger, trigger)),
        (
            "enforced_at_us",
            v.enforced_at.map(dur).transpose()?.unwrap_or(Value::Null),
        ),
        (
            "violations",
            checked_array(&v.violations, 3, |v| Ok(violation(v)))?,
        ),
        ("cleanup", cleanup(v.termination.cleanup)),
        ("kill_attempts", n(v.kill_attempts)),
    ]))
}
pub(super) fn timing(v: &p::RootOutcome) -> Result<Value> {
    Ok(obj([
        ("setup_us", dur(v.timing.setup)?),
        (
            "wall_us",
            v.timing.wall.map(dur).transpose()?.unwrap_or(Value::Null),
        ),
        (
            "observer",
            s(match v.timing.observer {
                p::ObserverKind::Pidfd => "pidfd",
                p::ObserverKind::Kqueue => "kqueue",
                p::ObserverKind::Polling => "polling",
            }),
        ),
    ]))
}
pub(super) fn summary(v: &e::RunSummary) -> Result<Value> {
    if matches!(v.class, r::RunClass::OutputBudget { streams } if streams.output_file) {
        return Err(Error("output_file stream is only valid in a test class"));
    }
    super::schema::cwd_notes(&v.notes, v.cwd_overflow_bytes)?;
    let (execution, timing, stdout, stderr, measurements, limits, errors, mut notes) =
        match v.record.as_ref() {
            e::ExecutionRecord::Started(root) => (
                started(root)?,
                timing(root)?,
                stream(
                    root.stdout.as_ref(),
                    v.observation.capture_files[0].as_deref(),
                )?,
                stream(
                    root.stderr.as_ref(),
                    v.observation.capture_files[1].as_deref(),
                )?,
                checked_array(&root.measurements, 5, |v| measurement(v, false))?,
                checked_array(&root.limits, 2, |v| limit(v, true))?,
                {
                    let mut errors: Vec<_> = root
                        .errors
                        .iter()
                        .copied()
                        .map(r::SupervisorErrorKind::from)
                        .collect();
                    errors.sort_unstable();
                    errors.dedup();
                    array(errors, |v| s(v.wire_token()))
                },
                root.notes.iter().copied().map(note).collect::<Vec<_>>(),
            ),
            e::ExecutionRecord::NotStarted(_) | e::ExecutionRecord::Transferred => {
                let (setup, observer) = v
                    .observation
                    .not_started_timing
                    .ok_or(Error("not-started summary needs timing observation"))?;
                if !matches!(observer, "pidfd" | "kqueue" | "polling" | "process_handle") {
                    return Err(Error("invalid observer"));
                }
                let transferred = matches!(v.record.as_ref(), e::ExecutionRecord::Transferred);
                (
                    kind(if transferred {
                        "transferred"
                    } else {
                        "not_started"
                    }),
                    obj([
                        ("setup_us", dur(setup)?),
                        ("wall_us", Value::Null),
                        (
                            "observer",
                            if transferred {
                                Value::Null
                            } else {
                                s(observer)
                            },
                        ),
                    ]),
                    stream(None, None)?,
                    stream(None, None)?,
                    Value::Array(vec![]),
                    Value::Array(vec![]),
                    Value::Array(vec![]),
                    vec![],
                )
            }
        };
    notes.extend(v.notes.iter().map(|v| v.wire_token()));
    let mut unique = Vec::new();
    for note in notes {
        if !unique.contains(&note) {
            unique.push(note);
        }
    }
    let value = obj([
        ("class", class(&v.class)?),
        ("execution", execution),
        ("timing", timing),
        ("deadline_us", dur(v.deadline)?),
        ("stdout", stdout),
        ("stderr", stderr),
        ("measurements", measurements),
        ("limits", limits),
        ("supervisor_errors", errors),
        ("notes", array(unique.iter().take(16), |v| s(*v))),
        ("notes_dropped", dropped(unique.len(), 16, 0)),
        ("cwd_overflow_bytes", opt(v.cwd_overflow_bytes, u)),
    ]);
    bounded(&value, 7 * 1024)?;
    Ok(value)
}
pub fn encode_run_summary(v: &e::RunSummary) -> Result<Vec<u8>> {
    Ok(encode(&summary(v)?))
}
pub(super) fn not_started(v: &e::StartFailure) -> Result<Value> {
    let mut fields = vec![("kind", s(v.reason.wire_token()))];
    match v.reason {
        r::NotStartedReason::CreateFailed => fields.extend([
            (
                "api",
                token(v.api.ok_or(Error("creation failure needs API"))?)?,
            ),
            (
                "os_error",
                i(v.os_error.ok_or(Error("creation failure needs OS error"))?),
            ),
        ]),
        r::NotStartedReason::Limit | r::NotStartedReason::Setup => fields.push((
            "os_error",
            i(v.os_error.ok_or(Error("start failure needs OS error"))?),
        )),
        _ => {}
    }
    Ok(Value::Object(fields))
}
pub(super) fn persist(v: &e::PersistError, message: &str) -> Value {
    match v {
        e::PersistError::Timeout => kind("timeout"),
        e::PersistError::QueueFull => kind("queue_full"),
        e::PersistError::Cancelled => kind("cancelled"),
        e::PersistError::WorkerUnavailable => kind("worker_unavailable"),
        e::PersistError::Io { os_error } => obj([
            ("kind", s("io")),
            ("os_error", opt(*os_error, i)),
            ("message", text(message)),
        ]),
    }
}
pub(super) fn reference(v: &e::ArtifactRef) -> Result<Value> {
    if v.path.is_some() == v.error.is_some() {
        return Err(Error("reference needs exactly one path or error"));
    }
    Ok(obj([
        ("digest", opt(v.digest.as_ref(), |v| hex(v))),
        (
            "path",
            v.path
                .as_deref()
                .map(|v| rel(v, 18))
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "error",
            opt(v.error.as_ref(), |v| {
                persist(v, "artefact persistence failed")
            }),
        ),
    ]))
}
pub(super) fn role(v: &e::Role) -> Result<Value> {
    Ok(match v {
        e::Role::Build => kind("build"),
        e::Role::Raw => kind("raw"),
        e::Role::Test { test } => obj([("kind", s("test")), ("test", ident(test)?)]),
        e::Role::Command { test, iteration } => obj([
            ("kind", s("command")),
            (
                "test",
                test.as_deref()
                    .map(ident)
                    .transpose()?
                    .unwrap_or(Value::Null),
            ),
            ("iteration", n(*iteration)),
        ]),
        e::Role::Hook { name, trigger } => obj([
            ("kind", s("hook")),
            ("name", ident(name)?),
            ("trigger", token(trigger)?),
        ]),
    })
}
pub(super) fn stage(v: e::Stage) -> Value {
    s(match v {
        e::Stage::Build => "build",
        e::Stage::Run => "run",
        e::Stage::Command => "command",
        e::Stage::Hook => "hook",
    })
}
pub(super) fn execution(v: &e::ExecutionReport) -> Result<Value> {
    if matches!(v.result.class, Some(r::RunClass::OutputBudget { streams }) if streams.output_file)
    {
        return Err(Error("output_file stream is only valid in a test class"));
    }
    let coverage = match v.result.coverage {
        r::Coverage::NotStarted { reason } => {
            let reason = match v.evidence.as_deref() {
                Some(e::ExecutionRecord::NotStarted(failure)) => not_started(failure)?,
                _ => kind(reason.wire_token()),
            };
            obj([("kind", s("not_started")), ("reason", reason)])
        }
        r::Coverage::Skipped { reason } => {
            obj([("kind", s("skipped")), ("reason", s(reason.wire_token()))])
        }
        other => kind(other.wire_token()),
    };
    let summary = match &v.summary {
        Some(v) => summary(v)?,
        None => Value::Null,
    };
    let value = obj([
        ("index", n(v.plan.index)),
        ("stage", stage(v.plan.stage)),
        ("role", role(&v.plan.role)?),
        ("coverage", coverage),
        ("run", opt(v.run.as_ref(), |v| s(v.as_str()))),
        (
            "program",
            opt(v.program.as_ref(), |v| {
                super::native::nsum(&super::native::native_bytes(v.as_os_str()), 256)
            }),
        ),
        (
            "spec",
            v.spec
                .as_ref()
                .map(reference)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "record",
            v.record
                .as_ref()
                .map(reference)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("summary", summary),
        (
            "class",
            if v.summary.is_none() {
                v.result
                    .class
                    .as_ref()
                    .map(class)
                    .transpose()?
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            },
        ),
        ("invariant", opt(v.result.invariant, |v| s(v.wire_token()))),
    ]);
    bounded(&value, 9 * 1024)?;
    Ok(value)
}
